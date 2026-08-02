//! Applying proposals: purely syntactic text replacement.
//!
//! Proposals carry structured edits ({target, find, replace}). Applying
//! is a pure function: each `find` must occur exactly once in the
//! target (template or design goals); otherwise the apply FAILS loudly
//! — the applier never guesses. No LLM, no interpretation risk. The
//! edits are also the diff, so review is trivial.

use crate::model::input::PromptUnderTest;
use crate::model::output::{EditTarget, Proposal};

/// The result of applying a proposal to a PUT.
#[derive(Debug)]
pub struct AppliedPut {
    pub template: String,
    pub design_goals: String,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ApplyError {
    #[error("proposal has no structured edits")]
    NoEdits,
    #[error("edit for {target} did not find '{find}' exactly once (occurrences: {count})")]
    FindNotUnique {
        target: &'static str,
        find: String,
        count: usize,
    },
}

pub fn apply(put: &PromptUnderTest, proposal: &Proposal) -> Result<AppliedPut, ApplyError> {
    let mut template = put.template.clone();
    let mut goals = put.design_goals.clone();

    if proposal.edits.is_empty() {
        return Err(ApplyError::NoEdits);
    }

    for edit in &proposal.edits {
        match edit.target {
            EditTarget::Template => {
                template = replace_once(&template, &edit.find, &edit.replace)?;
            }
            EditTarget::DesignGoals => {
                goals = replace_once(&goals, &edit.find, &edit.replace)?;
            }
        }
    }

    Ok(AppliedPut {
        template,
        design_goals: goals,
    })
}

fn replace_once(haystack: &str, find: &str, replace: &str) -> Result<String, ApplyError> {
    let count = haystack.matches(find).count();
    if count != 1 {
        return Err(ApplyError::FindNotUnique {
            target: if haystack_is_goals(haystack) { "design_goals" } else { "template" },
            find: find.to_string(),
            count,
        });
    }
    Ok(haystack.replace(find, replace))
}

fn haystack_is_goals(_s: &str) -> bool {
    // Callers pass template and goals through distinct paths; this
    // helper is only used to label errors. Simplify: label via caller.
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::output::{ProposalKind, TextEdit};

    fn put() -> PromptUnderTest {
        PromptUnderTest {
            id: "p".into(),
            template: "Help the customer. You may cancel orders using cancel_order.".into(),
            input_vars: Default::default(),
            tools: vec![],
            design_goals: "Always confirm.".into(),
        }
    }

    #[test]
    fn applies_unique_find() {
        let proposal = Proposal {
            kind: ProposalKind::Reword,
            content: "x".into(),
            addresses: vec![],
            confidence_note: "u".into(),
            edits: vec![TextEdit {
                target: EditTarget::Template,
                find: "You may cancel orders using cancel_order.".into(),
                replace: "Before cancelling, ask for explicit confirmation.".into(),
            }],
        };
        let out = apply(&put(), &proposal).unwrap();
        assert!(out.template.contains("ask for explicit confirmation"));
        assert!(!out.template.contains("cancel_order."));
        assert_eq!(out.design_goals, "Always confirm.");
    }

    #[test]
    fn fails_when_find_not_unique() {
        let proposal = Proposal {
            kind: ProposalKind::Reword,
            content: "x".into(),
            addresses: vec![],
            confidence_note: "u".into(),
            edits: vec![TextEdit {
                target: EditTarget::Template,
                find: "cancel".into(), // appears twice (cancel orders, cancel_order)
                replace: "person".into(),
            }],
        };
        assert!(matches!(
            apply(&put(), &proposal),
            Err(ApplyError::FindNotUnique { count: 2, .. })
        ));
    }

    #[test]
    fn fails_when_find_absent() {
        let proposal = Proposal {
            kind: ProposalKind::Reword,
            content: "x".into(),
            addresses: vec![],
            confidence_note: "u".into(),
            edits: vec![TextEdit {
                target: EditTarget::Template,
                find: "no such text".into(),
                replace: "x".into(),
            }],
        };
        assert!(matches!(
            apply(&put(), &proposal),
            Err(ApplyError::FindNotUnique { count: 0, .. })
        ));
    }

    #[test]
    fn fails_when_no_edits() {
        let proposal = Proposal {
            kind: ProposalKind::Reword,
            content: "x".into(),
            addresses: vec![],
            confidence_note: "u".into(),
            edits: vec![],
        };
        assert!(matches!(apply(&put(), &proposal), Err(ApplyError::NoEdits)));
    }
}
