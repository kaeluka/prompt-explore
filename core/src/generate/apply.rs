//! Applying proposals: the LLM produces the full corrected prompt;
//! a diff library computes the structured change for review.
//!
//! This sidesteps the "emit verbatim find/replace" instruction (which
//! LLMs follow unreliably). Whole-prompt rewriting and deterministic
//! diffing are both tasks models/libraries do well. The diff is the
//! review artifact — the user sees exactly what changed before
//! accepting, which makes LLM-driven application safe to trust.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use similar::{ChangeTag, TextDiff};

use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::model::input::PromptUnderTest;
use crate::model::output::{Proposal, ProposalKind};

pub struct ProposalApplier {
    client: Arc<dyn LlmClient>,
    model: String,
}

/// The result of applying a proposal: the new full template + goals,
/// plus a deterministic diff against the originals for review.
#[derive(Debug)]
pub struct AppliedPut {
    pub template: String,
    pub design_goals: String,
    pub template_diff: Vec<DiffPart>,
    pub goals_diff: Vec<DiffPart>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "tag", content = "value")]
pub enum DiffPart {
    Equal(String),
    Insert(String),
    Delete(String),
}

#[derive(Deserialize)]
struct LlmApplied {
    template: String,
    design_goals: String,
}

impl ProposalApplier {
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Apply a proposal: the LLM rewrites the full prompt; we diff the
    /// result against the originals. Returns the new prompt and the
    /// diffs. The proposal's kind hints at the focus: goal_revision
    /// edits the design goals; others edit the template.
    pub async fn apply(
        &self,
        put: &PromptUnderTest,
        proposal: &Proposal,
    ) -> Result<AppliedPut, LlmError> {
        let edit_goal = matches!(proposal.kind, ProposalKind::GoalRevision);

        let system = "You are applying a proposed change to an AI agent's prompt. \
                      You are given the current prompt template, its design goals, and a \
                      proposed change. Produce the UPDATED prompt: apply the change \
                      faithfully and precisely, changing only what the proposal requires \
                      and preserving everything else (tone, structure, other instructions). \
                      Do not editorialize or add unrelated improvements.\n\
                      If the proposal is a goal_revision, update the design goals instead \
                      of (or in addition to) the template.\n\
                      Respond with a single JSON object: {\"template\": \"<full updated \
                      template>\", \"design_goals\": \"<full updated design goals>\"}. \
                      Always return the COMPLETE text for both fields, even if one is \
                      unchanged."
            .to_string();

        let user = format!(
            "CURRENT TEMPLATE:\n{template}\n\nCURRENT DESIGN GOALS:\n{goals}\n\nPROPOSAL ({kind}):\n{content}\n\nIMPLICATED INSTRUCTIONS:\n{spans}\n\nNOTE: {note}{goal_hint}",
            template = put.template,
            goals = put.design_goals,
            kind = serde_json::to_string(&proposal.kind).unwrap_or_default(),
            content = proposal.content,
            spans = proposal
                .addresses
                .iter()
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
            note = proposal.confidence_note,
            goal_hint = if edit_goal {
                "\nThis is a goal_revision: focus on the design goals."
            } else {
                ""
            },
        );

        let reply = self
            .client
            .complete(ChatRequest {
                model: self.model.clone(),
                messages: vec![
                    Message::System { content: system },
                    Message::User { content: user },
                ],
                tools: vec![],
                temperature: Some(0.0),
                max_tokens: Some(4096),
            })
            .await?;

        let content = reply
            .content
            .ok_or_else(|| LlmError::MalformedResponse("empty applier reply".into()))?;
        let parsed: LlmApplied = parse_json(&content).ok_or_else(|| {
            LlmError::MalformedResponse(format!("applied prompt not JSON: {content}"))
        })?;

        Ok(AppliedPut {
            template_diff: diff_unicode_words(&put.template, &parsed.template),
            goals_diff: diff_unicode_words(&put.design_goals, &parsed.design_goals),
            template: parsed.template,
            design_goals: parsed.design_goals,
        })
    }
}

/// Word-level diff, returned as ordered parts for inline rendering.
fn diff_unicode_words(old: &str, new: &str) -> Vec<DiffPart> {
    let diff = TextDiff::from_unicode_words(old, new);
    diff.iter_all_changes()
        .map(|c| match c.tag() {
            ChangeTag::Equal => DiffPart::Equal(c.value().to_string()),
            ChangeTag::Insert => DiffPart::Insert(c.value().to_string()),
            ChangeTag::Delete => DiffPart::Delete(c.value().to_string()),
        })
        .collect()
}

fn parse_json<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(s.trim()).ok().or_else(|| {
        let start = s.find('{')?;
        let end = s.rfind('}')?;
        serde_json::from_str(&s[start..=end]).ok()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_marks_insertions_and_deletions() {
        let d = diff_unicode_words("the quick brown fox", "the slow brown fox");
        let inserts: Vec<_> = d
            .iter()
            .filter_map(|p| match p {
                DiffPart::Insert(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        let deletes: Vec<_> = d
            .iter()
            .filter_map(|p| match p {
                DiffPart::Delete(s) => Some(s.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(inserts, vec!["slow"]);
        assert!(deletes.iter().any(|s| s.contains("quick")));
    }

    #[test]
    fn identical_strings_produce_all_equal() {
        let d = diff_unicode_words("same text", "same text");
        assert!(d.iter().all(|p| matches!(p, DiffPart::Equal(_))));
    }
}
