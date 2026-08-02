//! LLM access layer. Everything outside this module speaks
//! `LlmClient` + provider-neutral `types`; provider details live in
//! `openai`. `mock` enables deterministic tests of the runtime layers.

pub mod client;
pub mod mock;
pub mod openai;
pub mod types;

pub use client::{LlmClient, LlmError};
pub use mock::MockLlmClient;
pub use openai::OpenAiCompatibleClient;
pub use types::*;
