//! Scenario lifecycle rules: editable while unreferenced, pinned once an
//! investigation references it, deletable only with cascade, forkable without
//! re-uploading the workspace.
use std::collections::HashMap;

use prompt_explore::model::scenario::{Correction, ScenarioDefinition};
use prompt_explore::scenario::{ScenarioStore, StoreError, WorkspaceAction};
use prompt_explore::simulate::Workspace;

fn definition(world: &str) -> ScenarioDefinition {
    ScenarioDefinition {
        world: world.into(),
        input_domain: HashMap::new(),
        user_message: None,
        simulator_notes: String::new(),
        tools: Vec::new(),
        simulation: Default::default(),
    }
}

fn seeded() -> Workspace {
    // A tiny non-empty workspace, so identity comparisons are meaningful.
    let mut workspace = Workspace::empty();
    workspace.exec(
        "write",
        &serde_json::json!({"path": "src/main.rs", "content": "fn main() {}"}),
    );
    assert_ne!(workspace.content_hash(), Workspace::empty().content_hash());
    workspace
}

/// Whether a workspace holds any content at all (seed or per-run writes).
fn is_empty(workspace: &Workspace) -> bool {
    workspace.content_hash() == Workspace::empty().content_hash()
}

#[test]
fn edit_probe_edit_is_allowed_while_unreferenced() {
    let mut store = ScenarioStore::new();
    let id = store
        .create(definition("first"), seeded(), None, 1)
        .unwrap();
    assert_eq!(store.get(&id).unwrap().revision, 1);
    assert!(store.get(&id).unwrap().editable());

    let before = store.get(&id).unwrap().workspace.content_hash();
    let revision = store
        .replace(&id, 1, definition("second"), WorkspaceAction::Keep, None, 2)
        .unwrap();
    assert_eq!(revision, 2);
    let record = store.get(&id).unwrap();
    assert_eq!(record.definition.world, "second");
    assert_eq!(
        record.workspace.content_hash(),
        before,
        "kept the workspace seed"
    );
    // The hash follows the definition, never the display metadata.
    assert_ne!(
        record.definition_hash,
        scenario_hash(&definition("first"), &seeded())
    );
}

fn scenario_hash(definition: &ScenarioDefinition, workspace: &Workspace) -> String {
    let mut hash = definition.content_hash();
    hash.push_str(&workspace.content_hash());
    hash
}

#[test]
fn a_stale_expected_revision_is_refused() {
    let mut store = ScenarioStore::new();
    let id = store
        .create(definition("w"), Workspace::empty(), None, 1)
        .unwrap();
    store
        .replace(&id, 1, definition("v2"), WorkspaceAction::Keep, None, 2)
        .unwrap();
    match store.replace(&id, 1, definition("v3"), WorkspaceAction::Keep, None, 3) {
        Err(StoreError::StaleRevision {
            expected, current, ..
        }) => {
            assert_eq!((expected, current), (1, 2));
        }
        other => panic!("expected a stale-revision conflict, got {other:?}"),
    }
    assert_eq!(store.get(&id).unwrap().definition.world, "v2");
}

#[test]
fn referencing_an_investigation_pins_the_definition_until_it_is_deleted() {
    let mut store = ScenarioStore::new();
    let id = store
        .create(definition("w"), Workspace::empty(), None, 1)
        .unwrap();
    store.attach_investigation(&id, "inv-1").unwrap();
    assert!(!store.get(&id).unwrap().editable());

    let error = store
        .replace(
            &id,
            1,
            definition("changed"),
            WorkspaceAction::Keep,
            None,
            2,
        )
        .unwrap_err();
    assert!(
        error.to_string().contains("pinned by 1 investigation"),
        "{error}"
    );
    assert!(
        error.to_string().contains("fork"),
        "the error suggests forking: {error}"
    );

    // A finished investigation still pins the definition it actually ran.
    store.finish_investigation("inv-1");
    assert!(!store.get(&id).unwrap().editable());
    assert!(
        store
            .replace(
                &id,
                1,
                definition("changed"),
                WorkspaceAction::Keep,
                None,
                3
            )
            .is_err()
    );

    // Deleting the investigation unlocks it, and the revision moves on so old
    // probe results cannot be confused with the new contents.
    store.detach_investigation("inv-1");
    assert!(store.get(&id).unwrap().editable());
    assert_eq!(
        store
            .replace(
                &id,
                1,
                definition("finally"),
                WorkspaceAction::Keep,
                None,
                4
            )
            .unwrap(),
        2
    );
}

#[test]
fn deletion_requires_cascade_and_refuses_running_work() {
    let mut store = ScenarioStore::new();
    let id = store
        .create(definition("w"), Workspace::empty(), None, 1)
        .unwrap();
    store.attach_investigation(&id, "inv-1").unwrap();
    store.attach_investigation(&id, "inv-2").unwrap();
    store.finish_investigation("inv-2");

    // Running work blocks deletion even WITH cascade: a run cannot be cancelled.
    let error = store.delete(&id, true, |_| false).unwrap_err();
    assert!(matches!(error, StoreError::Running { .. }), "{error}");
    assert!(store.get(&id).is_some());

    // Once nothing is running, dependents still block a plain delete, and the
    // error names them.
    store.finish_investigation("inv-1");
    let error = store.delete(&id, false, |_| false).unwrap_err();
    assert!(
        matches!(error, StoreError::HasDependents { count: 2, .. }),
        "{error}"
    );

    // A running investigation reported by the caller's store is honored too.
    let error = store.delete(&id, true, |inv| inv == "inv-2").unwrap_err();
    assert!(matches!(error, StoreError::Running { .. }), "{error}");

    // Cascade removes the scenario and reports exactly what depended on it.
    let outcome = store.delete(&id, true, |_| false).unwrap();
    assert_eq!(outcome.deleted, id);
    let mut cascade = outcome.cascade_investigations;
    cascade.sort();
    assert_eq!(cascade, vec!["inv-1".to_string(), "inv-2".to_string()]);
    assert!(store.get(&id).is_none());
}

#[test]
fn a_fork_shares_the_workspace_seed_and_records_its_correction() {
    let mut store = ScenarioStore::new();
    let id = store
        .create(definition("original"), seeded(), None, 1)
        .unwrap();
    store.attach_investigation(&id, "inv-1").unwrap();

    let fork = store
        .fork(
            &id,
            Some(Correction {
                scenario_id: id.clone(),
                revision: 1,
                reason: "the grep handler matched paths it should not".into(),
            }),
            None,
            2,
        )
        .unwrap();
    let fork_record = store.get(&fork).unwrap();
    assert_eq!(fork_record.revision, 1);
    assert!(fork_record.editable());
    // Sharing the immutable seed: same content, no re-upload needed.
    assert_eq!(
        fork_record.workspace.content_hash(),
        seeded().content_hash()
    );
    let correction = fork_record.correction.as_ref().unwrap();
    assert_eq!(correction.scenario_id, id);
    assert_eq!(correction.revision, 1);

    // A fork may be edited, and its correction survives its predecessor.
    store
        .replace(
            &fork,
            1,
            definition("corrected"),
            WorkspaceAction::Keep,
            None,
            3,
        )
        .unwrap();
    store.finish_investigation("inv-1");
    store.delete(&id, true, |_| false).unwrap();
    assert_eq!(
        store
            .get(&fork)
            .unwrap()
            .correction
            .as_ref()
            .unwrap()
            .reason,
        "the grep handler matched paths it should not"
    );

    // An unattributed fork is allowed too (callers fork for many reasons).
    let plain = store.fork(&fork, None, Some("variant".into()), 4).unwrap();
    assert!(store.get(&plain).unwrap().correction.is_none());
    assert_eq!(store.get(&plain).unwrap().label.as_deref(), Some("variant"));
}

#[test]
fn workspace_actions_replace_and_clear_the_seed() {
    let mut store = ScenarioStore::new();
    let id = store.create(definition("w"), seeded(), None, 1).unwrap();
    let empty_hash = Workspace::empty().content_hash();
    assert!(!is_empty(&store.get(&id).unwrap().workspace));
    store
        .replace(&id, 1, definition("w"), WorkspaceAction::Clear, None, 2)
        .unwrap();
    assert_eq!(
        store.get(&id).unwrap().workspace.content_hash(),
        empty_hash,
        "clear produces the empty workspace"
    );
    store
        .replace(
            &id,
            2,
            definition("w"),
            WorkspaceAction::Replace(seeded()),
            None,
            3,
        )
        .unwrap();
    assert!(!is_empty(&store.get(&id).unwrap().workspace));
}

#[test]
fn operations_on_unknown_scenarios_are_not_found() {
    let mut store = ScenarioStore::new();
    assert!(matches!(store.get("nope"), None));
    assert!(matches!(
        store.replace("nope", 1, definition("w"), WorkspaceAction::Keep, None, 1),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.delete("nope", true, |_| false),
        Err(StoreError::NotFound(_))
    ));
    assert!(matches!(
        store.attach_investigation("nope", "inv"),
        Err(StoreError::NotFound(_))
    ));
}
