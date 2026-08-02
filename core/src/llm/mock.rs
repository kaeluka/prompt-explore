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
