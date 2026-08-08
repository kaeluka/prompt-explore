//! LLM access layer. Everything outside this module speaks
//! `LlmClient` + provider-neutral `types`; provider details live in
//! `genai` (the `genai` multi-provider library: z.ai, OpenRouter,
//! AWS Bedrock, …). `mock` enables deterministic tests of the runtime
//! layers.

pub mod client;
pub mod genai;
pub mod mock;
pub mod parse;
pub mod track;
pub mod types;

pub use client::{LlmClient, LlmError};
pub use genai::ProviderClient;
pub use mock::MockLlmClient;
pub use parse::{extract_json, parse_json};
pub use track::{UsageTotals, UsageTracker};
pub use types::*;
