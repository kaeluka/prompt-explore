//! Caller-owned interpretation of evidence. Validation checks shape and references,
//! never the truth of a judgment or whether a numeric grade is deserved.
use crate::model::simulation::TraceTurn;
use serde::{Deserialize, Serialize};

/// Maximum UTF-8 text bytes across an assessment's summary, rubric and evidence notes.
pub const MAX_ASSESSMENT_BYTES: usize = 65_536;
/// Maximum number of evidence references in one caller assessment.
pub const MAX_EVIDENCE_REFERENCES: usize = 256;

/// A caller's explanation of its judgment, not a harness verdict. Store this with
/// grades so another reader knows the scale, limitations and actual evidence.
/// PATCH replaces the whole assessment; null clears it. It is optional: a trace
/// can be useful without being graded. All annotations are lost on server restart.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct Assessment {
    /// What the caller observed and concluded, including simulation limitations.
    /// Total text across this assessment is limited to 65536 UTF-8 bytes.
    pub summary: String,
    /// Caller-defined grading scale/method, including the meaning of each axis.
    /// Empty when no grades are supplied or no rubric is applicable.
    #[serde(default)]
    pub rubric: String,
    /// References to existing conversation evidence. Indices are zero-based in
    /// GET /api/investigations/{id}/evidence `turns`. Maximum 256 references.
    #[serde(default)]
    pub evidence: Vec<EvidenceReference>,
}

/// A location the caller used to reach its conclusion. Point to an exchange to
/// discuss a simulated response, or to a whole turn for a model answer. The note
/// expresses the caller's interpretation; a valid index does not validate it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct EvidenceReference {
    pub turn: usize,
    /// Zero-based index within that turn's tool_exchanges; omit/null for the turn.
    #[serde(default)]
    pub exchange: Option<usize>,
    pub note: String,
}

impl Assessment {
    pub fn validate(&self, turns: &[TraceTurn]) -> Result<(), String> {
        if self.evidence.len() > MAX_EVIDENCE_REFERENCES {
            return Err(format!(
                "assessment exceeds {MAX_EVIDENCE_REFERENCES} evidence references"
            ));
        }
        let bytes = self.evidence.iter().fold(
            self.summary.len().saturating_add(self.rubric.len()),
            |n, item| n.saturating_add(item.note.len()),
        );
        if bytes > MAX_ASSESSMENT_BYTES {
            return Err(format!(
                "assessment text exceeds {MAX_ASSESSMENT_BYTES} UTF-8 bytes"
            ));
        }
        for reference in &self.evidence {
            let turn = turns.get(reference.turn).ok_or_else(|| {
                format!(
                    "assessment references missing turn {} ({} available)",
                    reference.turn,
                    turns.len()
                )
            })?;
            if reference
                .exchange
                .is_some_and(|i| i >= turn.tool_exchanges.len())
            {
                return Err(format!(
                    "assessment references missing exchange in turn {} ({} available)",
                    reference.turn,
                    turn.tool_exchanges.len()
                ));
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_only_bounds_not_judgment() {
        let turns = vec![TraceTurn {
            model_output: "hello".into(),
            thinking: None,
            tool_exchanges: vec![],
        }];
        let mut assessment = Assessment {
            summary: "Caller may disagree with the trace".into(),
            rubric: String::new(),
            evidence: vec![EvidenceReference {
                turn: 0,
                exchange: None,
                note: "my interpretation".into(),
            }],
        };
        assert!(assessment.validate(&turns).is_ok());
        assessment.evidence[0].exchange = Some(0);
        assert!(assessment.validate(&turns).is_err());
        assessment.evidence[0].exchange = None;
        assessment.evidence[0].turn = 1;
        assert!(assessment.validate(&turns).is_err());
        assessment.evidence.clear();
        assessment.summary = "x".repeat(MAX_ASSESSMENT_BYTES + 1);
        assert!(assessment.validate(&turns).is_err());
    }
}
