//! LLM access layer. Everything outside this module speaks
//! `LlmClient` + provider-neutral `types`; provider details live in
//! `genai` (the `genai` multi-provider library: z.ai, OpenRouter,
//! AWS Bedrock, …). `gcloud` supplies GCP Application Default
//! Credentials for the Vertex AI (Gemini) provider. `mock` enables
//! deterministic tests of the runtime layers.

pub mod client;
mod gcloud;
pub mod genai;
pub mod mock;
pub mod models;
pub mod parse;
pub mod track;
pub mod types;

pub use client::{LlmClient, LlmError};
pub use genai::{GenaiClient, ProviderClient, qualify_model, thinking_level_supported};
pub use mock::MockLlmClient;
pub use models::{ModelEntry, ProviderModels, catalog_pricing_map, cost_usd, list_all_map};
pub use parse::{extract_json, parse_json};
pub use track::{UsageByRole, UsageTotals, UsageTracker};
pub use types::*;
