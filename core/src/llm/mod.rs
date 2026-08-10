//! LLM access layer. Everything outside this module speaks
//! `LlmClient` + provider-neutral `types`; provider details live in
//! `genai` (the `genai` multi-provider library: z.ai, OpenRouter,
//! AWS Bedrock, …). `gcloud` supplies GCP Application Default
//! Credentials for the Vertex AI (Gemini) provider. `mock` enables
//! deterministic tests of the runtime layers.

pub mod client;
pub mod genai;
mod gcloud;
pub mod mock;
pub mod models;
pub mod parse;
pub mod track;
pub mod types;

pub use client::{LlmClient, LlmError};
pub use genai::{ProviderClient, GenaiClient};
pub use mock::MockLlmClient;
pub use models::{list_all_map, ModelEntry, ProviderModels};
pub use parse::{extract_json, parse_json};
pub use track::{UsageTotals, UsageTracker};
pub use types::*;
