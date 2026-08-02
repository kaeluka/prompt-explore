//! Derived at the start of a run: the operationalized predicate and
//! the hypotheses that steer scenario generation.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::input::VarSpec;

/// The investigation question, used directly as the judge criterion.
/// No translation to logic; the LLM judge evaluates traces against
/// this natural-language criterion, and the transcript is the evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Predicate {
    pub criterion: String,
    pub success_mode: SuccessMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SuccessMode {
    /// One trace demonstrating the behavior (existential questions).
    Witness,
    /// Two traces diverging on the same scenario class (differential
    /// questions, e.g. "sometimes cancels, sometimes asks").
    WitnessPair,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Hypothesis {
    pub id: String,
    pub claim: String,
    /// Spans of the prompt template implicated by this hypothesis.
    pub target_instructions: Vec<String>,
    /// Per-var overrides, e.g. sharpen an NlDescription adversarially.
    /// May replace a Constant with a description and vice versa.
    pub input_overrides: HashMap<String, VarSpec>,
    /// Guidance for the scenario generator.
    pub scenario_strategy: String,
}
