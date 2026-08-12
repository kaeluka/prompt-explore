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

use super::gcloud;

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
        // Google Vertex AI (Gemini); auth from GCP Application Default
        // Credentials (`gcloud auth application-default login`). Live
        // Model Garden query; no pricing.
        list_vertex().await,
        // Baseten (OpenAI-compatible Model APIs): direct /models call so
        // we keep pricing (same key conventions as OpenRouter). Auth
        // from BASETEN_API_KEY; endpoint from BASETEN_ENDPOINT (default
        // https://inference.baseten.co/v1/ — api.baseten.co is the
        // control plane and does not serve the OpenAI-style listing).
        list_baseten().await,
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

/// Live Vertex AI listing via the Model Garden publisher-models
/// endpoint — the same one `gcloud ai model-garden models list` calls
/// (genai's own Vertex list is a hardcoded, stale snapshot, so we don't
/// use it). The endpoint needs a quota project (`x-goog-user-project`)
/// where the aiplatform API + billing are enabled and the caller has
/// `serviceusage.services.use`; we try the ADC quota project and the
/// resolved project in turn, so whichever one is set up works. On
/// failure the returned error is Google's own message, which names the
/// exact missing piece (enable the API / billing / grant the role).
async fn list_vertex() -> (String, ProviderModels) {
    let token = match gcloud::access_token().await {
        Ok(t) => t,
        Err(e) => return ("vertex".into(), ProviderModels::Error { error: e }),
    };
    let mut candidates: Vec<String> = Vec::new();
    if let Ok(q) = gcloud::adc_quota_project().await {
        candidates.push(q);
    }
    if let Ok(p) = gcloud::project_id().await {
        candidates.push(p);
    }
    candidates.dedup();
    if candidates.is_empty() {
        return (
            "vertex".into(),
            ProviderModels::Error {
                error: "no GCP project for the quota header".into(),
            },
        );
    }
    let mut last_err = String::new();
    for quota_project in &candidates {
        match fetch_vertex_models(&token, quota_project).await {
            Ok(mut models) => {
                models.sort_by(|a, b| a.name.cmp(&b.name));
                return ("vertex".into(), ProviderModels::Available { models });
            }
            Err(e) => last_err = e,
        }
    }
    ("vertex".into(), ProviderModels::Error { error: last_err })
}

#[derive(Deserialize)]
struct VertexListResp {
    #[serde(default, rename = "publisherModels")]
    publisher_models: Vec<VertexPublisherModel>,
    #[serde(rename = "nextPageToken")]
    next_page_token: Option<String>,
}
#[derive(Deserialize)]
struct VertexPublisherModel {
    name: String,
}

/// Model Garden inventory covers every publisher and modality; keep
/// only what genai's Vertex adapter can serve chat for: gemini-* and
/// claude-*, minus non-chat specialties. (Any id the API accepts can be
/// used in a request even if filtered out here.)
fn is_vertex_chat_model(id: &str) -> bool {
    let chat_family = id.starts_with("gemini") || id.starts_with("claude");
    let non_chat = ["embedding", "robotics", "tts"];
    chat_family && !non_chat.iter().any(|s| id.contains(s))
}

async fn fetch_vertex_models(
    token: &str,
    quota_project: &str,
) -> Result<Vec<ModelEntry>, String> {
    // Regional host, like `gcloud ai model-garden models list`
    // (us-central1 is its default); the inventory is not region-scoped.
    let url = "https://us-central1-aiplatform.googleapis.com/v1beta1/publishers/*/models";
    let http = reqwest::Client::new();
    let mut ids: std::collections::BTreeSet<String> = Default::default();
    let mut page_token: Option<String> = None;
    loop {
        let mut query = vec![
            ("listAllVersions", "true".to_string()),
            ("filter", "is_hf_wildcard(false)".to_string()),
            ("pageSize", "500".to_string()),
        ];
        if let Some(t) = &page_token {
            query.push(("pageToken", t.clone()));
        }
        let resp = http
            .get(url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-goog-user-project", quota_project)
            .query(&query)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            // Surface Google's message (it names the missing permission
            // / API / billing), trimmed of JSON noise.
            return Err(format!(
                "Model Garden list failed ({status}) with quota project \
                 {quota_project}: {}",
                google_error_message(&body).unwrap_or(body)
            ));
        }
        let page: VertexListResp = resp.json().await.map_err(|e| e.to_string())?;
        for m in page.publisher_models {
            // `name` is `publishers/<pub>/models/<id>`; versioned
            // variants share the id, so a set dedupes them.
            if let Some(id) = m.name.rsplit('/').next() {
                let id = id.split('@').next().unwrap_or(id);
                if is_vertex_chat_model(id) {
                    ids.insert(id.to_string());
                }
            }
        }
        match page.next_page_token {
            Some(t) if !t.is_empty() => page_token = Some(t),
            _ => break,
        }
    }
    Ok(ids
        .into_iter()
        .map(|id| ModelEntry {
            name: format!("vertex::{id}"),
            pricing: None,
        })
        .collect())
}

/// Pulls `error.message` out of a Google JSON error body.
fn google_error_message(body: &str) -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(body).ok()?;
    Some(v.get("error")?.get("message")?.as_str()?.to_string())
}

const BASETEN_API_KEY_ENV: &str = "BASETEN_API_KEY";

async fn list_baseten() -> (String, ProviderModels) {
    let api_key = match std::env::var(BASETEN_API_KEY_ENV) {
        Ok(k) if !k.is_empty() => k,
        _ => {
            return (
                "baseten".into(),
                ProviderModels::Error {
                    error: format!("no API key in ${BASETEN_API_KEY_ENV}"),
                },
            )
        }
    };
    match fetch_baseten(&api_key).await {
        Ok(mut models) => {
            models.sort_by(|a, b| a.name.cmp(&b.name));
            ("baseten".into(), ProviderModels::Available { models })
        }
        Err(e) => ("baseten".into(), ProviderModels::Error { error: e }),
    }
}

#[derive(Deserialize)]
struct BasetenResp {
    data: Vec<BasetenModel>,
}
#[derive(Deserialize)]
struct BasetenModel {
    id: String,
    pricing: Option<OpenRouterPricing>,
}

async fn fetch_baseten(api_key: &str) -> Result<Vec<ModelEntry>, String> {
    let base = std::env::var("BASETEN_ENDPOINT")
        .unwrap_or_else(|_| "https://inference.baseten.co/v1/".into());
    let url = format!("{}/models", base.trim_end_matches('/'));
    let resp: BasetenResp = reqwest::Client::new()
        .get(url)
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
            // Baseten reports pricing with the same keys as OpenRouter
            // (prompt / completion / input_cache_read) — reuse the
            // shared helper so consumers keep one vocabulary.
            ModelEntry {
                name: format!("baseten::{}", m.id),
                pricing: m.pricing.and_then(pricing_map),
            }
        })
        .collect())
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
        .map(|m| ModelEntry {
            name: format!("open_router::{}", m.id),
            pricing: m.pricing.and_then(pricing_map),
        })
        .collect())
}

/// Keys follow OpenRouter's conventions; reuse them for any pricing
/// source so consumers keep one vocabulary.
fn pricing_map(p: OpenRouterPricing) -> Option<BTreeMap<String, String>> {
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
}

/// Convenience: build the map shape the endpoint serializes to.
pub async fn list_all_map(client: &Client) -> BTreeMap<String, ProviderModels> {
    list_all(client).await.into_iter().collect()
}

/// Build a `model name → pricing` map from a provider catalog, keeping
/// only entries that report per-token pricing. Keys are the full
/// namespaced model names — the same strings callers paste into a
/// request's `model` field — so a job's stored model name can be looked
/// up directly.
pub fn catalog_pricing_map(
    providers: &BTreeMap<String, ProviderModels>,
) -> BTreeMap<String, BTreeMap<String, String>> {
    let mut out = BTreeMap::new();
    for pm in providers.values() {
        if let ProviderModels::Available { models } = pm {
            for m in models {
                if let Some(p) = &m.pricing {
                    out.insert(m.name.clone(), p.clone());
                }
            }
        }
    }
    out
}

/// Estimated USD cost of one usage total, from a per-token pricing map
/// (the `prompt` / `completion` / `input_cache_read` vocabulary).
///
/// `cache_read_tokens` is a subset of `input_tokens` (see `Usage`), so
/// the full `prompt` rate applies only to the uncached remainder; cached
/// tokens are billed at `input_cache_read`, falling back to `prompt`
/// when the provider quotes no cache rate. Returns `None` when the map
/// lacks the `prompt` or `completion` rate needed to price the run —
/// the caller then leaves cost unset rather than guessing.
pub fn cost_usd(
    input_tokens: u64,
    cache_read_tokens: u64,
    output_tokens: u64,
    pricing: &BTreeMap<String, String>,
) -> Option<f64> {
    let rate = |k: &str| pricing.get(k)?.parse::<f64>().ok();
    let prompt = rate("prompt")?;
    let completion = rate("completion")?;
    let cache = rate("input_cache_read").unwrap_or(prompt);
    let uncached = input_tokens.saturating_sub(cache_read_tokens) as f64;
    let usd =
        uncached * prompt + cache_read_tokens as f64 * cache + output_tokens as f64 * completion;
    // Round to nano-dollars so float noise never shows in the output.
    Some((usd * 1e9).round() / 1e9)
}
