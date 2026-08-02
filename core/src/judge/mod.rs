//! The judge layer: evaluates traces against predicates and design
//! goals, producing the evidence (verdicts, goal findings, divergence)
//! that witnesses and proposals are built from.

pub mod judge;
pub mod transcript;

pub use judge::Judge;
pub use transcript::render_transcript;
