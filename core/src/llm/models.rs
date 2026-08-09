//! Listing available models per provider, for `GET /models`.
//!
//! Returns, per provider, either its model names (each in the full
//! namespaced form you'd paste into a request's `model` field) or an
//! `error` explaining why it couldn't be listed (no auth, network,
//! region-gated, …). Listing is best-effort and per-provider: one
//! provider failing never breaks the others.
//!
//! Today this returns names only (genai's `all_model_names` gives
//! `Vec<String>`); context/pricing would need a per-provider metadata
//! call and is a future enrichment. The model *name* is the thing
//! callers can never remember, so it comes first.

use std::collections::BTreeMap;

use genai::adapter::AdapterKind;
use genai::resolver::{Endpoint, ProviderConfig};
use genai::Client;
use serde::Serialize;

/// One model the caller can put in a request's `model` field. `name` is
/// the full namespaced, pastable string (e.g.
/// `open_router::deepseek/deepseek-v4-flash-0731`).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ModelEntry {
    pub name: String,
}

/// A provider's listing result.
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ProviderModels {
    /// The provider was queried successfully.
    Available { models: Vec<ModelEntry> },
    /// The provider could not be queried — e.g. no API key in the
    /// environment, no AWS credentials, network error, region-gated.
    Error { error: String },
}

/// Stable, ordered set of providers reported by `GET /models`. Each key
/// is the namespace prefix used in the request `model` field.
fn providers() -> Vec<(&'static str, AdapterKind, ProviderConfig)> {
    vec![
        // z.ai coding-plan endpoint (our default); auth falls back to
        // ZAI_API_KEY from the adapter.
        (
            "zai_coding",
            AdapterKind::Zai,
            ProviderConfig {
                endpoint: Some(Endpoint::from_static(
                    "https://api.z.ai/api/coding/paas/v4/",
                )),
                auth: None,
            },
        ),
        // OpenRouter; auth from OPEN_ROUTER_API_KEY.
        ("open_router", AdapterKind::OpenRouter, ProviderConfig::default()),
        // AWS Bedrock via native Converse + SigV4; auth from the default
        // AWS credential chain (aws sso login, profiles, IMDS).
        ("bedrock_sigv4", AdapterKind::BedrockSigv4, ProviderConfig::default()),
    ]
}

/// List models for all known providers. Sequential (three quick calls);
/// a slow/unreachable provider still completes the others since each is
/// independent and maps to its own `Error` entry.
pub async fn list_all(client: &Client) -> Vec<(String, ProviderModels)> {
    let mut out = Vec::new();
    for (key, kind, config) in providers() {
        out.push(list_provider_models(client, key, kind, config.clone()).await);
    }
    out
}

async fn list_provider_models(
    client: &Client,
    key: &str,
    kind: AdapterKind,
    config: ProviderConfig,
) -> (String, ProviderModels) {
    match client.all_model_names(kind, config).await {
        Ok(names) => {
            let mut models: Vec<ModelEntry> = names
                .into_iter()
                .map(|n| ModelEntry {
                    name: format!("{key}::{n}"),
                })
                .collect();
            models.sort_by(|a, b| a.name.cmp(&b.name));
            (key.into(), ProviderModels::Available { models })
        }
        Err(e) => (key.into(), ProviderModels::Error { error: e.to_string() }),
    }
}

/// Convenience: build the map shape the endpoint serializes to.
pub async fn list_all_map(client: &Client) -> BTreeMap<String, ProviderModels> {
    list_all(client).await.into_iter().collect()
}
