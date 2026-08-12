//! Usage accounting: a decorator around any `LlmClient` that
//! accumulates token usage, LLM call count, and tool-call count.
//!
//! This is pure deterministic bookkeeping — counting is the harness's
//! job. Wrap a client per investigation and read `totals()` at the end.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::client::{LlmClient, LlmError};
use super::types::{ChatRequest, ChatResponse};

/// Cumulative usage across every call routed through a `UsageTracker`.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
pub struct UsageTotals {
    pub input_tokens: u64,
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
    /// Completions requested across all roles (the runner PUT and the
    /// tool simulator).
    pub llm_calls: u64,
    /// Tool calls the model requested. Only the simulated PUT has
    /// tools, so this counts tool calls in simulated traces.
    pub tool_calls: u64,
    /// Estimated USD cost of this usage, when the server knows the
    /// per-token pricing for the model that produced it (e.g.
    /// OpenRouter models). Absent for subscription / no-pricing
    /// providers and for models the catalog doesn't price. The tracker
    /// never sets this (it sees tokens, not prices); the server fills
    /// it in from the model catalog when assembling a response.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cost_usd: Option<f64>,
}

/// Token usage and call counts split by model role: the prompt under
/// test vs. the tool simulator. The two models serve very different
/// purposes (the sim is the test ENVIRONMENT, the PUT is the thing
/// under test), so their spend is never lumped together — a single
/// combined total would hide which side is expensive.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, utoipa::ToSchema)]
pub struct UsageByRole {
    /// Usage of the prompt-under-test model (the agent being tested).
    pub put: UsageTotals,
    /// Usage of the tool-simulator model (the LLM that roleplays the
    /// environment — rendering tool responses and resolving inputs).
    pub sim: UsageTotals,
}

/// Wraps an `LlmClient` and accumulates `UsageTotals`. Cheap to clone
/// into as many roles as needed; all clones share the same totals.
/// To split usage by role, wrap each role's client in its OWN tracker
/// and read each one's `totals()` separately.
pub struct UsageTracker {
    inner: Arc<dyn LlmClient>,
    totals: Mutex<UsageTotals>,
}

impl UsageTracker {
    pub fn new(inner: Arc<dyn LlmClient>) -> Self {
        Self {
            inner,
            totals: Mutex::new(UsageTotals::default()),
        }
    }

    pub fn totals(&self) -> UsageTotals {
        *self.totals.lock().unwrap()
    }
}

#[async_trait]
impl LlmClient for UsageTracker {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let res = self.inner.complete(req).await?;
        let mut t = self.totals.lock().unwrap();
        t.llm_calls += 1;
        t.tool_calls += res.tool_calls.len() as u64;
        if let Some(u) = res.usage {
            t.input_tokens += u.input_tokens;
            t.cache_read_tokens += u.cache_read_tokens;
            t.output_tokens += u.output_tokens;
        }
        drop(t);
        Ok(res)
    }
}
