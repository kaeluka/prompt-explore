//! The LLM client abstraction. Runtime layers (`simulate`, `generate`,
//! `judge`) depend on this trait, never on a concrete provider, so
//! tests can inject `MockLlmClient`.

use async_trait::async_trait;

use super::types::{ChatRequest, ChatResponse};

#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError>;
}

#[derive(Debug, thiserror::Error)]
pub enum LlmError {
    #[error("provider request failed: {0}")]
    Provider(String),
    #[error("response could not be interpreted: {0}")]
    MalformedResponse(String),
}
