//! Applying proposals: turn a natural-language proposal into an
//! updated prompt. Application is interpretation (proposals are NL),
//! so it's LLM-driven — but the RESULT is just data: an updated
//! template and design goals, which the user reviews and owns.

use std::sync::Arc;

use serde::Deserialize;

use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::model::input::PromptUnderTest;
use crate::model::output::{Proposal, ProposalKind};

pub struct ProposalApplier {
    client: Arc<dyn LlmClient>,
    model: String,
}

/// The result of applying a proposal to a PUT.
pub struct AppliedPut {
    pub template: String,
    pub design_goals: String,
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

    /// Apply a proposal to a PUT, returning the updated template and
    /// design goals. The proposal's kind hints at what to change:
    /// reword/split/merge/data_transform edit the template;
    /// goal_revision edits the design goals.
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
        let parsed: LlmApplied = parse_json(&content)
            .ok_or_else(|| LlmError::MalformedResponse(format!("applied prompt not JSON: {content}")))?;

        Ok(AppliedPut {
            template: parsed.template,
            design_goals: parsed.design_goals,
        })
    }
}

fn parse_json<T: serde::de::DeserializeOwned>(s: &str) -> Option<T> {
    serde_json::from_str(s.trim()).ok().or_else(|| {
        let start = s.find('{')?;
        let end = s.rfind('}')?;
        serde_json::from_str(&s[start..=end]).ok()
    })
}
