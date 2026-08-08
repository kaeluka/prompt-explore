//! The predicate: the investigation question, used directly as the
//! judge's criterion.

use serde::{Deserialize, Serialize};

/// The investigation question, used directly as the judge criterion.
/// No translation to logic; the LLM judge evaluates traces against
/// this natural-language criterion, and the transcript is the evidence.
#[derive(Debug, Clone, Serialize, Deserialize, utoipa::ToSchema)]
pub struct Predicate {
    pub criterion: String,
    pub success_mode: SuccessMode,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum SuccessMode {
    /// One trace demonstrating the behavior (existential questions).
    Witness,
    /// Two traces diverging on the same scenario class (differential
    /// questions, e.g. "sometimes cancels, sometimes asks").
    WitnessPair,
}
