//! Listing available models per provider, for `GET /models`.
//!
//! Returns, per provider, either its models (each in the full namespaced
//! form you'd paste into a request's `model` field) or an `error`
//! explaining why it couldn't be listed (no auth, network, region-gated,
//! …). Listing is best-effort and per-provider: one provider failing
//! never breaks the others.
//!
//! Pricing: the optional `pricing` map (per-token USD strings) is
//! populated where the provider exposes it. The keys (`prompt` = input,
//! `completion` = output, `input_cache_read` = cached input) follow
//! OpenRouter's conventions; if future pricing sources are wired in,
//! they should reuse these same keys so consumers need one vocabulary.

use std::collections::BTreeMap;

use genai::adapter::AdapterKind;
use genai::resolver::{Endpoint, ProviderConfig};
use genai::Client;
use serde::{Deserialize, Serialize};

/// One model the caller can put in a request's `model` field. `name` is
/// the full namespaced, pastable string (e.g.
/// `open_router::deepseek/deepseek-v4-flash-0731`).
#[derive(Debug, Clone, Serialize, utoipa::ToSchema)]
pub struct ModelEntry {
    pub name: String,
    /// Per-token USD pricing as reported by the provider. Keys follow
    /// OpenRouter's conventions: `prompt` (input), `completion`
    /// (output), `input_cache_read` (cached input). Present only when
    /// the provider exposes pricing — absent for subscription endpoints
    /// (z.ai coding plan) and providers that don't report it (Bedrock).
    /// If future pricing sources are added, reuse these same keys.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pricing: Option<BTreeMap<String, String>>,
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
pub async fn list_all(client: &Client) -> Vec<(String, ProviderModels)> {
    vec![
        // z.ai coding-plan endpoint (our default); auth falls back to
        // ZAI_API_KEY. Subscription — no per-token pricing.
        list_via_genai(
            client,
            "zai_coding",
            AdapterKind::Zai,
            ProviderConfig {
                endpoint: Some(Endpoint::from_static(
                    "https://api.z.ai/api/coding/paas/v4/",
                )),
                auth: None,
            },
        )
        .await,
        // OpenRouter: direct /models call so we keep pricing + context
        // (genai's all_model_names returns names only). Auth from
        // OPEN_ROUTER_API_KEY.
        list_openrouter().await,
        // AWS Bedrock via native Converse + SigV4; auth from the default
        // AWS credential chain. Bedrock's list API exposes no pricing.
        list_via_genai(
            client,
            "bedrock_sigv4",
            AdapterKind::BedrockSigv4,
            ProviderConfig::default(),
        )
        .await,
    ]
}

/// Name-only listing via genai's adapter (providers where we have no
/// richer metadata source, or where pricing doesn't apply).
async fn list_via_genai(
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
                    pricing: None,
                })
                .collect();
            models.sort_by(|a, b| a.name.cmp(&b.name));
            (key.into(), ProviderModels::Available { models })
        }
        Err(e) => (key.into(), ProviderModels::Error { error: e.to_string() }),
    }
}

const OPENROUTER_API_KEY_ENV: &str = "OPEN_ROUTER_API_KEY";

async fn list_openrouter() -> (String, ProviderModels) {
    let api_key = match std::env::var(OPENROUTER_API_KEY_ENV) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return (
                "open_router".into(),
                ProviderModels::Error {
                    error: format!("no API key in ${OPENROUTER_API_KEY_ENV}"),
                },
            )
        }
    };
    match fetch_openrouter(&api_key).await {
        Ok(mut models) => {
            models.sort_by(|a, b| a.name.cmp(&b.name));
            ("open_router".into(), ProviderModels::Available { models })
        }
        Err(e) => ("open_router".into(), ProviderModels::Error { error: e }),
    }
}

#[derive(Deserialize)]
struct OpenRouterResp {
    data: Vec<OpenRouterModel>,
}
#[derive(Deserialize)]
struct OpenRouterModel {
    id: String,
    pricing: Option<OpenRouterPricing>,
}
#[derive(Deserialize)]
struct OpenRouterPricing {
    prompt: Option<String>,
    completion: Option<String>,
    input_cache_read: Option<String>,
}

async fn fetch_openrouter(api_key: &str) -> Result<Vec<ModelEntry>, String> {
    let resp: OpenRouterResp = reqwest::Client::new()
        .get("https://openrouter.ai/api/v1/models")
        .header("Authorization", format!("Bearer {api_key}"))
        .send()
        .await
        .map_err(|e| e.to_string())?
        .json()
        .await
        .map_err(|e| e.to_string())?;

    Ok(resp
        .data
        .into_iter()
        .map(|m| {
            // Keys follow OpenRouter's conventions; reuse them for any
            // future pricing source so consumers keep one vocabulary.
            let pricing = m.pricing.and_then(|p| {
                let mut map = BTreeMap::new();
                if let Some(v) = p.prompt {
                    map.insert("prompt".into(), v);
                }
                if let Some(v) = p.completion {
                    map.insert("completion".into(), v);
                }
                if let Some(v) = p.input_cache_read {
                    map.insert("input_cache_read".into(), v);
                }
                (!map.is_empty()).then_some(map)
            });
            ModelEntry {
                name: format!("open_router::{}", m.id),
                pricing,
            }
        })
        .collect())
}

/// Convenience: build the map shape the endpoint serializes to.
pub async fn list_all_map(client: &Client) -> BTreeMap<String, ProviderModels> {
    list_all(client).await.into_iter().collect()
}
