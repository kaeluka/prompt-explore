//! A scripted client for deterministic tests. Each call pops the next
//! scripted response; a request inspector can assert on what the
//! system under test actually sent.

use std::sync::Mutex;

use async_trait::async_trait;

use super::client::{LlmClient, LlmError};
use super::types::{ChatRequest, ChatResponse};

pub struct MockLlmClient {
    responses: Mutex<Vec<ChatResponse>>,
    pub requests: Mutex<Vec<ChatRequest>>,
}

impl MockLlmClient {
    /// Responses are returned in the order given.
    pub fn scripted(responses: Vec<ChatResponse>) -> Self {
        Self {
            responses: Mutex::new(responses),
            requests: Mutex::new(Vec::new()),
        }
    }
}

#[async_trait]
impl LlmClient for MockLlmClient {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        self.requests.lock().unwrap().push(req);
        let mut responses = self.responses.lock().unwrap();
        if responses.is_empty() {
            return Err(LlmError::Provider("mock script exhausted".into()));
        }
        Ok(responses.remove(0))
    }
}

/// A client for runs that must not call a provider: any completion is a
/// readable error. Used when a probe is fully implemented in Lua (or declares
/// no inputs) on a server started without provider credentials, so developing a
/// simulation offline is possible — while any delegation fails loudly instead
/// of silently returning something invented.
pub struct UnavailableClient;

#[async_trait::async_trait]
impl LlmClient for UnavailableClient {
    async fn complete(&self, _request: ChatRequest) -> Result<ChatResponse, LlmError> {
        Err(LlmError::Provider(
            "no LLM client is configured on this server (no provider API key): this response \
             would need the simulator model. Implement the tool in Lua, or configure a provider"
                .into(),
        ))
    }
}
