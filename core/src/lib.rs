//! prompt-explore: property-based testing for agent behavior.
//!
//! The user states a behavioral question about a prompt under test;
//! the tool searches simulated scenarios for witness traces that answer
//! it, attributes the behavior to prompt instructions, and proposes
//! (unverified) fixes. The user owns everything after that.

pub mod generate;
pub mod judge;
pub mod llm;
pub mod model;
pub mod simulate;
