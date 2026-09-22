//! Usage accounting: a decorator around any `LlmClient` that
//! accumulates token usage, LLM call count, and tool-call count.
//!
//! This is pure deterministic bookkeeping — counting is the harness's
//! job. Wrap a client per investigation and read `totals()` at the end.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;

use super::client::{LlmClient, LlmError};
use super::types::{ChatRequest, ChatResponse};

/// Cumulative usage across every call routed through a `UsageTracker`.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
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

/// Token usage and call counts split by model role: the application side (all
/// workflow agent models, serialized under the historical `put` role name) vs.
/// the tool simulator. These roles serve very different purposes: the simulator
/// is the test ENVIRONMENT and the workflow is the application under test, so a
/// single combined total would hide which side is expensive.
#[derive(Debug, Default, Clone, Copy, serde::Serialize, serde::Deserialize, utoipa::ToSchema)]
pub struct UsageByRole {
    /// Usage of ALL agent invocations in the system under test, including
    /// repeated stages and different models. USD cost is summed per actual
    /// model and unavailable if any used model lacks pricing.
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
    usage: Mutex<TrackedUsage>,
}

#[derive(Default)]
struct TrackedUsage {
    totals: UsageTotals,
    by_model: BTreeMap<String, UsageTotals>,
}

impl UsageTracker {
    pub fn new(inner: Arc<dyn LlmClient>) -> Self {
        Self {
            inner,
            usage: Mutex::new(TrackedUsage::default()),
        }
    }

    pub fn totals(&self) -> UsageTotals {
        self.usage.lock().unwrap().totals
    }

    /// Actual per-request models, not a nominal role model. One orchestration
    /// may call several models; never price their summed tokens as one model.
    pub fn by_model(&self) -> BTreeMap<String, UsageTotals> {
        self.usage.lock().unwrap().by_model.clone()
    }

    /// Unknown pricing for ANY used model makes the role total unavailable.
    /// An empty role has incurred no generation cost.
    pub fn priced_totals(&self, pricing: &super::PricingMap) -> UsageTotals {
        let snapshot = self.usage.lock().unwrap();
        let mut totals = snapshot.totals;
        totals.cost_usd = snapshot
            .by_model
            .iter()
            .try_fold(0.0, |sum, (model, usage)| {
                super::cost_usd(
                    usage.input_tokens,
                    usage.cache_read_tokens,
                    usage.output_tokens,
                    pricing.get(model)?,
                )
                .map(|cost| sum + cost)
            });
        totals
    }
}

#[async_trait]
impl LlmClient for UsageTracker {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let model = req.model.clone();
        let res = self.inner.complete(req).await?;
        let accumulate = |t: &mut UsageTotals| {
            t.llm_calls += 1;
            t.tool_calls += res.tool_calls.len() as u64;
            if let Some(u) = res.usage {
                t.input_tokens += u.input_tokens;
                t.cache_read_tokens += u.cache_read_tokens;
                t.output_tokens += u.output_tokens;
            }
        };
        let mut usage = self.usage.lock().unwrap();
        accumulate(&mut usage.totals);
        accumulate(usage.by_model.entry(model).or_default());
        Ok(res)
    }
}
