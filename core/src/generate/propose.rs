//! Proposal generation: given a witness (or a failed search), suggest
//! prompt changes. Always UNVERIFIED — the user owns everything after.
//!
//! The verifier is the user re-asking the question; this layer only
//! hypothesizes fixes. To keep it honest, proposals are ranked by how
//! directly they address the attributed instructions vs. speculative
//! restructuring, and every proposal carries an explicit confidence
//! note stating it is unverified.

use std::sync::Arc;

use serde::Deserialize;

use crate::judge::render_transcript;
use crate::llm::{ChatRequest, LlmClient, LlmError, Message};
use crate::model::input::PromptUnderTest;
use crate::model::output::{Attribution, Proposal, ProposalKind};
use crate::model::simulation::Scenario;

pub struct ProposalGenerator {
    client: Arc<dyn LlmClient>,
    model: String,
}

#[derive(Deserialize)]
struct LlmProposals {
    proposals: Vec<LlmProposal>,
}

#[derive(Deserialize)]
struct LlmProposal {
    kind: String,
    content: String,
    addresses: Vec<String>,
    rationale: String,
}

impl ProposalGenerator {
    pub fn new(client: Arc<dyn LlmClient>, model: impl Into<String>) -> Self {
        Self {
            client,
            model: model.into(),
        }
    }

    /// Propose fixes for a witnessed behavior. `attribution` says which
    /// instructions are implicated; the transcript shows what happened;
    /// the PUT gives the full context to edit.
    pub async fn propose(
        &self,
        put: &PromptUnderTest,
        attribution: &Attribution,
        witness_transcript: &str,
        scenario: Option<&Scenario>,
    ) -> Result<Vec<Proposal>, LlmError> {
        let system = "You are a prompt engineer reviewing a bug in an AI agent. \
                      You are given the agent's prompt, a witness trace showing the \
                      unwanted behavior, and the instructions implicated by it. \
                      Propose CONCRETE changes that would prevent the behavior. \
                      Prefer targeted rewording of specific instructions over broad \
                      restructuring; suggest splits, merges, or data transforms only \
                      when clearly justified. Every proposal is a HYPOTHESIS for the \
                      user to verify — never claim a proposal is proven to work.\n\n\
                      Proposal kinds: \"reword\", \"split\", \"merge\", \"data_transform\", \
                      \"goal_revision\" (use goal_revision only when a stated design goal \
                      is itself unachievable or self-contradictory).\n\n\
                      Respond with a single JSON object: {\"proposals\": [{\"kind\": \
                      \"...\", \"content\": \"<the specific change>\", \"addresses\": \
                      \"[<implicated instruction strings this targets>]\", \"rationale\": \
                      \"<why this should help>\"}]}. Rank most-targeted first."
            .to_string();

        let mut user = format!(
            "AGENT PROMPT:\n{template}\n\nDESIGN GOALS:\n{goals}\n\nIMPLICATED INSTRUCTIONS:\n{spans}\n\nATTRIBUTION EVIDENCE:\n{evidence}\n\nWITNESS TRANSCRIPT:\n{witness_transcript}",
            template = put.template,
            goals = put.design_goals,
            spans = attribution
                .instruction_spans
                .iter()
                .map(|s| format!("  - {s}"))
                .collect::<Vec<_>>()
                .join("\n"),
            evidence = attribution.evidence,
        );
        if let Some(s) = scenario {
            user.push_str(&format!("\n\nSCENARIO: user said {:?}", s.user_message));
        }

        let reply = self.call(&system, &user).await?;
        let parsed: LlmProposals = parse_json(&reply)
            .ok_or_else(|| LlmError::MalformedResponse(format!("proposals not JSON: {reply}")))?;

        Ok(parsed
            .proposals
            .into_iter()
            .map(|p| Proposal {
                kind: parse_kind(&p.kind),
                content: p.content,
                addresses: p.addresses,
                confidence_note: format!(
                    "Unverified hypothesis. Rationale: {}. Apply, then re-ask the \n                     question to check.",
                    p.rationale
                ),
            })
            .collect())
    }

    async fn call(&self, system: &str, user: &str) -> Result<String, LlmError> {
        let reply = self
            .client
            .complete(ChatRequest {
                model: self.model.clone(),
                messages: vec![
                    Message::System {
                        content: system.into(),
                    },
                    Message::User {
                        content: user.into(),
                    },
                ],
                tools: vec![],
                temperature: Some(0.0),
                max_tokens: Some(2048),
            })
            .await?;
        reply
            .content
            .ok_or_else(|| LlmError::MalformedResponse("empty proposal-generator reply".into()))
    }
}

fn parse_kind(s: &str) -> ProposalKind {
    match s.trim().to_ascii_lowercase().as_str() {
        "split" => ProposalKind::Split,
        "merge" => ProposalKind::Merge,
        "data_transform" => ProposalKind::DataTransform,
        "goal_revision" => ProposalKind::GoalRevision,
        _ => ProposalKind::Reword,
    }
}

/// Helper: render the witness trace for the proposal prompt.
pub fn witness_transcript(witness_traces: &[crate::model::simulation::Trace]) -> String {
    witness_traces
        .iter()
        .map(render_transcript)
        .collect::<Vec<_>>()
        .join("\n---\n")
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
    fn parse_kind_maps_known_and_defaults_to_reword() {
        assert!(matches!(parse_kind("reword"), ProposalKind::Reword));
        assert!(matches!(parse_kind("Split"), ProposalKind::Split));
        assert!(matches!(
            parse_kind("goal_revision"),
            ProposalKind::GoalRevision
        ));
        assert!(matches!(parse_kind("nonsense"), ProposalKind::Reword));
    }
}
