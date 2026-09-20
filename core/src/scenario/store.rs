//! The scenario registry: reusable definitions with the lifecycle rules that
//! make reuse trustworthy.
//!
//! The rules (enforced here, not in the HTTP layer):
//!
//! * A scenario is EDITABLE exactly while no investigation references it.
//!   Accepting an investigation pins the scenario immediately; deleting the
//!   last referencing investigation unlocks it again.
//! * Editing requires the caller's expected revision, so a stale edit can
//!   never silently overwrite another agent's work.
//! * Deletion is refused while any referencing investigation is running, and
//!   refused entirely (listing the dependents) unless `cascade` is requested.
//! * Forking copies the definition and SHARES the immutable workspace seed —
//!   no re-upload, no re-decompression.
//!
//! Everything is in memory, like the job store: registration is reuse within a
//! server lifetime, never a durability promise.

use std::collections::BTreeMap;

use sha2::{Digest, Sha256};

use crate::model::scenario::{Correction, ScenarioDefinition};
use crate::simulate::Workspace;

/// One investigation that references a scenario. `running` covers running,
/// queued and not-yet-finalized work: a run that has been accepted but has not
/// stopped spending cannot be forgotten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvestigationRef {
    pub id: String,
    pub running: bool,
}

/// A stored scenario: the reusable definition, its immutable workspace seed,
/// and its dependency bookkeeping.
pub struct ScenarioRecord {
    pub id: String,
    /// Increases on every successful edit; an investigation pins one value.
    pub revision: u64,
    /// SHA-256 of the definition's execution-relevant contents. Together with
    /// the workspace hash it identifies exactly what a run executed.
    pub definition_hash: String,
    pub created_at: u64,
    pub updated_at: u64,
    /// Display-only label. Never part of the identity or the hash.
    pub label: Option<String>,
    pub correction: Option<Correction>,
    pub definition: ScenarioDefinition,
    pub workspace: Workspace,
    investigations: Vec<InvestigationRef>,
}

impl ScenarioRecord {
    /// Whether the definition is still editable (no investigation references it).
    pub fn editable(&self) -> bool {
        self.investigations.is_empty()
    }

    pub fn investigation_ids(&self) -> Vec<String> {
        self.investigations.iter().map(|r| r.id.clone()).collect()
    }

    /// Investigation count, exposed so a reader sees the dependency before
    /// trying to edit or delete.
    pub fn investigation_count(&self) -> usize {
        self.investigations.len()
    }
}

/// What should happen to the workspace seed during an edit.
pub enum WorkspaceAction {
    /// No archive supplied: keep the existing seed.
    Keep,
    /// A new archive was uploaded: replace the seed.
    Replace(Workspace),
    /// The caller explicitly asked for an empty workspace.
    Clear,
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("no scenario '{0}' in this server's memory — already deleted, or lost on restart")]
    NotFound(String),
    #[error("scenario '{id}' is pinned by {count} investigation(s) ({examples}); \
             fork it (POST /api/scenarios/{id}/fork) to make an editable copy, or delete those \
             investigations first (DELETE /api/scenarios/{id}?cascade=true deletes them for you)")]
    Locked {
        id: String,
        count: usize,
        examples: String,
    },
    #[error("scenario '{id}' is at revision {current} but the edit expected {expected}; \
             re-read the scenario and retry against the current revision")]
    StaleRevision {
        id: String,
        expected: u64,
        current: u64,
    },
    #[error("scenario '{id}' can be deleted only with cascade=true: {count} investigation(s) \
             depend on it ({examples})")]
    HasDependents {
        id: String,
        count: usize,
        examples: String,
    },
    #[error("scenario '{id}' has running work and cannot be deleted: investigation(s) {examples} \
             are still running — poll them until done or failed (a run cannot be cancelled)")]
    Running { id: String, examples: String },
    #[error("{0}")]
    Invalid(String),
}

/// What a successful deletion removed, so the caller can purge dependent jobs
/// and probes from its own stores.
#[derive(Debug)]
pub struct DeleteOutcome {
    pub deleted: String,
    pub cascade_investigations: Vec<String>,
}

/// The registry. Empty at startup, lost on restart.
#[derive(Default)]
pub struct ScenarioStore {
    scenarios: BTreeMap<String, ScenarioRecord>,
    /// Monotonic counter making generated ids unique within a server lifetime.
    sequence: u64,
}

impl ScenarioStore {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn list(&self) -> Vec<&ScenarioRecord> {
        self.scenarios.values().collect()
    }

    pub fn get(&self, id: &str) -> Option<&ScenarioRecord> {
        self.scenarios.get(id)
    }

    /// Register a definition. Validation is deterministic (tool shape, Lua
    /// syntax); nothing is executed and no provider is contacted.
    pub fn create(
        &mut self,
        definition: ScenarioDefinition,
        workspace: Workspace,
        label: Option<String>,
        now: u64,
    ) -> Result<String, StoreError> {
        definition.validate().map_err(StoreError::Invalid)?;
        let id = self.next_id("scn", now, &definition, &workspace);
        let record = ScenarioRecord {
            revision: 1,
            definition_hash: definition.content_hash(),
            created_at: now,
            updated_at: now,
            label,
            correction: None,
            definition,
            workspace,
            investigations: Vec::new(),
            id: id.clone(),
        };
        self.scenarios.insert(id.clone(), record);
        Ok(id)
    }

    /// Edit a definition in place. Refused while any investigation references
    /// it, and while the caller's expected revision is stale.
    pub fn replace(
        &mut self,
        id: &str,
        expected_revision: u64,
        definition: ScenarioDefinition,
        workspace: WorkspaceAction,
        label: Option<Option<String>>,
        now: u64,
    ) -> Result<u64, StoreError> {
        definition.validate().map_err(StoreError::Invalid)?;
        let record = self
            .scenarios
            .get_mut(id)
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        if !record.investigations.is_empty() {
            return Err(locked_error(id, record));
        }
        if record.revision != expected_revision {
            return Err(StoreError::StaleRevision {
                id: id.into(),
                expected: expected_revision,
                current: record.revision,
            });
        }
        record.definition = definition;
        record.definition_hash = record.definition.content_hash();
        match workspace {
            WorkspaceAction::Keep => {}
            WorkspaceAction::Replace(workspace) => record.workspace = workspace,
            WorkspaceAction::Clear => record.workspace = Workspace::empty(),
        }
        if let Some(label) = label {
            record.label = label;
        }
        record.revision += 1;
        record.updated_at = now;
        Ok(record.revision)
    }

    /// Copy a scenario into a new editable one, sharing the immutable workspace
    /// seed (no re-upload, no re-decompression). `correction` records what this
    /// copy corrects; it is descriptive history, never a dependency.
    pub fn fork(
        &mut self,
        id: &str,
        correction: Option<Correction>,
        label: Option<String>,
        now: u64,
    ) -> Result<String, StoreError> {
        if let Some(correction) = &correction {
            if correction.scenario_id.trim().is_empty() || correction.reason.trim().is_empty() {
                return Err(StoreError::Invalid(
                    "correction needs a scenario_id and a reason".into(),
                ));
            }
        }
        let (definition, workspace, source_label) = {
            let source = self
                .scenarios
                .get(id)
                .ok_or_else(|| StoreError::NotFound(id.into()))?;
            (
                source.definition.clone(),
                source.workspace.clone(),
                source.label.clone(),
            )
        };
        let new_id = self.next_id("scn", now, &definition, &workspace);
        let record = ScenarioRecord {
            id: new_id.clone(),
            revision: 1,
            definition_hash: definition.content_hash(),
            created_at: now,
            updated_at: now,
            label: label.or_else(|| source_label.map(|label| format!("{label} (copy)"))),
            correction,
            definition,
            workspace,
            investigations: Vec::new(),
        };
        self.scenarios.insert(new_id.clone(), record);
        Ok(new_id)
    }

    /// Delete a scenario. `is_running` reports whether a referenced
    /// investigation is still running in the caller's job store; `cascade`
    /// opts into deleting dependent investigations.
    pub fn delete(
        &mut self,
        id: &str,
        cascade: bool,
        is_running: impl Fn(&str) -> bool,
    ) -> Result<DeleteOutcome, StoreError> {
        let record = self
            .scenarios
            .get(id)
            .ok_or_else(|| StoreError::NotFound(id.into()))?;
        let running: Vec<String> = record
            .investigations
            .iter()
            .filter(|r| r.running || is_running(&r.id))
            .map(|r| r.id.clone())
            .collect();
        if !running.is_empty() {
            return Err(StoreError::Running {
                id: id.into(),
                examples: summarize(&running),
            });
        }
        if !cascade && !record.investigations.is_empty() {
            return Err(StoreError::HasDependents {
                id: id.into(),
                count: record.investigations.len(),
                examples: summarize(&record.investigation_ids()),
            });
        }
        // Validate everything before removing anything: no partial cascade.
        let record = self.scenarios.remove(id).expect("checked above");
        Ok(DeleteOutcome {
            deleted: id.into(),
            cascade_investigations: record.investigation_ids(),
        })
    }

    /// Record that an investigation pins this scenario. Called at submission,
    /// before the run starts, so a concurrent edit cannot slip in behind it.
    pub fn attach_investigation(
        &mut self,
        scenario_id: &str,
        investigation_id: &str,
    ) -> Result<(), StoreError> {
        let record = self
            .scenarios
            .get_mut(scenario_id)
            .ok_or_else(|| StoreError::NotFound(scenario_id.into()))?;
        if record
            .investigations
            .iter()
            .any(|r| r.id == investigation_id)
        {
            return Ok(());
        }
        record.investigations.push(InvestigationRef {
            id: investigation_id.into(),
            running: true,
        });
        Ok(())
    }

    /// Mark an accepted investigation as no longer running. The reference
    /// itself stays: a finished investigation still pins the definition it ran.
    pub fn finish_investigation(&mut self, investigation_id: &str) {
        for record in self.scenarios.values_mut() {
            for reference in &mut record.investigations {
                if reference.id == investigation_id {
                    reference.running = false;
                }
            }
        }
    }

    /// Forget a deleted investigation. The scenario becomes editable again when
    /// its last reference goes away.
    pub fn detach_investigation(&mut self, investigation_id: &str) {
        for record in self.scenarios.values_mut() {
            record
                .investigations
                .retain(|reference| reference.id != investigation_id);
        }
    }

    fn next_id(&mut self, prefix: &str, now: u64, definition: &ScenarioDefinition, workspace: &Workspace) -> String {
        self.sequence += 1;
        let mut hash = Sha256::new();
        hash.update(now.to_le_bytes());
        hash.update(self.sequence.to_le_bytes());
        hash.update(definition.content_hash().as_bytes());
        hash.update(workspace.content_hash().as_bytes());
        format!("{prefix}-{}", &format!("{:x}", hash.finalize())[..12])
    }
}

fn summarize(ids: &[String]) -> String {
    let shown = ids.iter().take(3).cloned().collect::<Vec<_>>().join(", ");
    if ids.len() > 3 {
        format!("{shown}, …")
    } else {
        shown
    }
}

fn locked_error(id: &str, record: &ScenarioRecord) -> StoreError {
    StoreError::Locked {
        id: id.into(),
        count: record.investigation_count(),
        examples: summarize(&record.investigation_ids()),
    }
}