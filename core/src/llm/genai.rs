//! Provider client built on the `genai` multi-provider library.
//!
//! One implementation of [`LlmClient`] serves every provider genai knows.
//! Which provider a request hits is decided by the request's **model
//! namespace**, not by the client:
//!
//! | model string | provider | auth |
//! |---|---|---|
//! | `zai_coding::glm-5.2` | z.ai coding-plan endpoint | `ZAI_API_KEY` env |
//! | `zai::glm-4.6` | z.ai standard endpoint | `ZAI_API_KEY` env |
//! | `open_router::<model>` | OpenRouter | `OPENROUTER_API_KEY` env |
//! | `bedrock_sigv4::<model>` | AWS Bedrock (native Converse, SigV4) | default AWS credential chain (env / profile / SSO / IMDS) |
//! | `vertex::gemini-2.5-pro` | Google Vertex AI (Gemini) | GCP Application Default Credentials (`gcloud auth application-default login`) |
//! | bare name (e.g. `glm-5.2`) | resolved by [`ModelMapper`] (ZAI default — see [`ProviderClient::zai`]) | per provider |
//!
//! The `bedrock-sigv4` cargo feature pulls in `aws-config` for the full
//! AWS credential chain, so `aws sso login` works with no extra setup.
//! Likewise `gcp_auth` implements the GCP ADC chain, so
//! `gcloud auth application-default login` is all Gemini needs.

use async_trait::async_trait;
use genai::adapter::AdapterKind;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest as GChatRequest, ChatResponse as GChatResponse,
    MessageContent, Tool as GTool,
};
use genai::resolver::{AuthData, ModelMapper, ServiceTargetResolver};
use genai::{Client, ModelIden, ModelName, ServiceTarget};
use serde_json::Value;

// Re-export so the server (which depends only on core) can build a
// listing Client without adding genai as a direct dependency.
pub use genai::Client as GenaiClient;

/// Default retry count per completion for transient rate limits, HTTP
/// timeouts/server errors, and transport failures (21 total attempts).
/// Override with `PROMPT_EXPLORE_MAX_RETRIES` (set 0 to disable retries).
pub const DEFAULT_MAX_RETRIES: u32 = 20;
/// Default initial linear-backoff interval in milliseconds. Override with
/// `PROMPT_EXPLORE_RETRY_BASE_DELAY_MS`.
pub const DEFAULT_RETRY_BASE_DELAY_MS: u64 = 5_000;
/// Default wall-clock deadline for ONE provider HTTP attempt. Without it a
/// stalled socket is bounded by nothing: the retry budget counts attempts (not
/// time) and the step/token budgets only advance on completions, so a wedged
/// request can hold a job `running` indefinitely. A deadline that expires is
/// treated exactly like a transport failure — retryable, with the same attempt
/// budget and backoff. Override with `PROMPT_EXPLORE_REQUEST_TIMEOUT_MS`; `0`
/// disables the deadline (a caller who prefers waiting forever).
pub const DEFAULT_REQUEST_TIMEOUT_MS: u64 = 60_000;
/// Default maximum positive retry jitter percentage. Override with
/// `PROMPT_EXPLORE_RETRY_JITTER_PERCENT`.
pub const DEFAULT_RETRY_JITTER_PERCENT: u64 = 10;

use super::client::{LlmClient, LlmError};
use super::types::{
    ChatRequest, ChatResponse, Message, ThinkingLevel, ToolCallRequest, ToolDef, Usage,
};

/// Which provider a bare (un-namespaced) model name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProvider {
    ZaiCoding,
    Zai,
    OpenRouter,
    Bedrock,
    Baseten,
    Gemini,
}

impl DefaultProvider {
    /// Namespaces a bare model name; namespaced names pass through.
    fn qualify(self, model: &str) -> String {
        if model.contains("::") {
            return model.to_string();
        }
        match self {
            Self::ZaiCoding => format!("zai_coding::{model}"),
            Self::Zai => format!("zai::{model}"),
            Self::OpenRouter => format!("open_router::{model}"),
            Self::Bedrock => format!("bedrock_sigv4::{model}"),
            Self::Baseten => format!("baseten::{model}"),
            Self::Gemini => format!("vertex::{model}"),
        }
    }

    /// The adapter kind the qualified name routes to.
    fn adapter_kind(self) -> AdapterKind {
        match self {
            Self::ZaiCoding | Self::Zai => AdapterKind::Zai,
            Self::OpenRouter => AdapterKind::OpenRouter,
            Self::Bedrock => AdapterKind::BedrockSigv4,
            Self::Baseten => AdapterKind::OpenAI,
            Self::Gemini => AdapterKind::Vertex,
        }
    }
}

pub struct ProviderClient {
    client: Client,
    /// Which provider bare (un-namespaced) model names resolve to. Kept
    /// alongside the client so call sites can recompute the effective
    /// namespace of a model string (e.g. the Bedrock temperature rule in
    /// [`effective_temperature`]) exactly the way the ModelMapper does.
    default: DefaultProvider,
}

impl ProviderClient {
    fn build(default: DefaultProvider) -> Self {
        // Baseten env config (OpenAI-compatible; endpoint overridable).
        // Default is the serverless Model-APIs endpoint — api.baseten.co
        // is the control plane and serves neither /models nor chat
        // completions for these models.
        let baseten_endpoint = std::env::var("BASETEN_ENDPOINT")
            .unwrap_or_else(|_| "https://inference.baseten.co/v1/".into());
        let baseten_key = std::env::var("BASETEN_API_KEY").unwrap_or_default();

        let client = Client::builder()
            .with_model_mapper(ModelMapper::from_mapper_fn(
                move |model_ident: ModelIden| {
                    // `baseten::` is OUR namespace — genai has no Baseten
                    // adapter, so its name-based guess falls back to Ollama
                    // (Baseten model ids like `deepseek-ai/...` match no
                    // known prefix). Re-pin it to OpenAI; the service-target
                    // resolver below supplies endpoint + auth.
                    if model_ident.model_name.namespace_is("baseten") {
                        return Ok(ModelIden::new(AdapterKind::OpenAI, model_ident.model_name));
                    }
                    // Other namespaced model strings already resolved to the
                    // right adapter — pass through. Bare names get the
                    // client's default provider.
                    if model_ident.model_name.namespace().is_some() {
                        return Ok(model_ident);
                    }
                    Ok(ModelIden::new(
                        default.adapter_kind(),
                        ModelName::new(default.qualify(model_ident.model_name.as_str())),
                    ))
                },
            ))
            // Per-request routing overrides, composable with any default
            // provider in a single client:
            // - baseten:: models go to the Baseten endpoint with its API key.
            // - vertex:: models (Gemini) get a fresh OAuth2 token from GCP
            //   Application Default Credentials (`gcloud auth
            //   application-default login`) and the aiplatform endpoint for
            //   the resolved project/region — genai's Vertex adapter only
            //   reads a static VERTEX_API_KEY, so we supply the ADC token
            //   ourselves, like aws-config does for Bedrock SigV4.
            .with_service_target_resolver(ServiceTargetResolver::from_resolver_async_fn(
                move |mut st: ServiceTarget| -> std::pin::Pin<
                    Box<
                        dyn std::future::Future<Output = genai::resolver::Result<ServiceTarget>>
                            + Send,
                    >,
                > {
                    let baseten_endpoint = baseten_endpoint.clone();
                    let baseten_key = baseten_key.clone();
                    Box::pin(async move {
                        if st.model.model_name.namespace_is("baseten") {
                            st.endpoint = genai::resolver::Endpoint::from_owned(baseten_endpoint);
                            if !baseten_key.is_empty() {
                                st.auth = AuthData::from_single(baseten_key);
                            }
                        }
                        if st.model.model_name.namespace_is("vertex") {
                            let token = super::gcloud::access_token()
                                .await
                                .map_err(genai::resolver::Error::Custom)?;
                            st.auth = AuthData::from_single(token);
                            st.endpoint = genai::resolver::Endpoint::from_owned(
                                super::gcloud::vertex_endpoint()
                                    .await
                                    .map_err(genai::resolver::Error::Custom)?,
                            );
                        }
                        Ok(st)
                    })
                },
            ))
            .build()
            .expect("failed to initialize LLM HTTP client");
        Self { client, default }
    }

    /// The model string, namespace-qualified the same way this client's
    /// ModelMapper qualifies it (bare names get the default provider;
    /// namespaced names pass through).
    fn effective_model(&self, model: &str) -> String {
        if model.contains("::") {
            model.to_string()
        } else {
            self.default.qualify(model)
        }
    }

    /// z.ai **coding-plan** endpoint; bare model names default to it.
    /// The adapter reads `ZAI_API_KEY` from the environment.
    pub fn zai() -> Self {
        Self::build(DefaultProvider::ZaiCoding)
    }

    /// z.ai standard pay-per-use endpoint.
    pub fn zai_standard() -> Self {
        Self::build(DefaultProvider::Zai)
    }

    /// OpenRouter gateway; adapter reads `OPENROUTER_API_KEY`.
    pub fn openrouter() -> Self {
        Self::build(DefaultProvider::OpenRouter)
    }

    /// AWS Bedrock via native Converse + SigV4. Credentials come from the
    /// default AWS chain (`aws sso login`, profiles, env, IMDS) — no API key.
    pub fn bedrock() -> Self {
        Self::build(DefaultProvider::Bedrock)
    }

    /// Google Gemini via Vertex AI. Credentials come from GCP Application
    /// Default Credentials (`gcloud auth application-default login`,
    /// `GOOGLE_APPLICATION_CREDENTIALS`, GCE metadata) — no API key.
    /// Project from `VERTEX_PROJECT_ID` / `GOOGLE_CLOUD_PROJECT` or the
    /// gcloud config; region from `VERTEX_LOCATION` (default: `global`).
    pub fn gemini() -> Self {
        Self::build(DefaultProvider::Gemini)
    }

    /// Baseten (OpenAI-compatible). Reads `BASETEN_API_KEY`; endpoint
    /// defaults to `https://inference.baseten.co/v1/`, overridable via
    /// `BASETEN_ENDPOINT`. Bare model names default to this provider.
    pub fn baseten() -> Self {
        Self::build(DefaultProvider::Baseten)
    }

    /// Any OpenAI-compatible endpoint, keyed explicitly. Requests must use a
    /// **namespaced** model string (e.g. `zai_coding::glm-5.2` or
    /// `open_router::<model>`); the resolver overrides that provider's
    /// endpoint and auth with the values given here.
    pub fn new(api_base: &str, api_key: &str) -> Self {
        let api_base = api_base.to_string();
        let api_key = api_key.to_string();
        let resolver = ServiceTargetResolver::from_resolver_fn(move |st: ServiceTarget| {
            let mut st = st;
            st.endpoint = genai::resolver::Endpoint::from_owned(api_base.clone());
            st.auth = AuthData::from_single(api_key.clone());
            Ok(st)
        });
        Self {
            client: Client::builder()
                .with_service_target_resolver(resolver)
                .build()
                .expect("failed to initialize LLM HTTP client"),
            // Only namespaced strings are meaningful here (see doc); the
            // default exists so the temperature heuristic below still has
            // an answer for a bare name.
            default: DefaultProvider::OpenRouter,
        }
    }
}

#[async_trait]
impl LlmClient for ProviderClient {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let mut chat_req = GChatRequest::from_messages(convert_messages(req.messages));
        if !req.tools.is_empty() {
            let tools = req.tools.into_iter().map(convert_tool).collect::<Vec<_>>();
            chat_req = chat_req.with_tools(tools);
        }

        let mut options = ChatOptions::default();
        // Model string, namespace-qualified the way the ModelMapper will
        // qualify it, so provider-specific decisions below see the real
        // target.
        let model = self.effective_model(&req.model);
        if let Some(t) = effective_temperature(&model, req.temperature) {
            options = options.with_temperature(t as f64);
        }
        if let Some(level) = req.thinking_level {
            options = options.with_reasoning_effort(genai_reasoning_effort(level));
        }
        if let Some(m) = req.max_tokens {
            options = options.with_max_tokens(m);
        }
        // Extract `<think>...</think>` blocks into the response's reasoning
        // content for providers that emit them inline instead of as a
        // separate field, so thinking is captured uniformly.
        options.normalize_reasoning_content = Some(true);

        // Retry only this completion, with identical messages/options. Tool
        // execution is outside this boundary, so a network failure never
        // replays an accepted tool batch or restarts the investigation.
        retry_provider_call(&model, RetrySettings::from_env(), || {
            self.client
                .exec_chat(&model, chat_req.clone(), Some(&options))
        })
        .await
        .map(convert_response)
    }
}

/// Temperature to actually send for a (namespace-qualified) model
/// string. Newer Bedrock models — the OpenAI GPT-5.x family, the current
/// Claude line — hard-reject ANY request carrying `temperature` in
/// inferenceConfig (400: "This model doesn't support the temperature
/// field" / "`temperature` is deprecated for this model"), and Bedrock
/// offers no signal for which models accept it, so the whole namespace
/// loses the field and the model's own default applies. This was the
/// server's bug, not genai's: the adapter omits `temperature` when unset
/// (issue #3), but the runner and simulator historically always passed 0.7.
/// Every other provider passes the requested value through unchanged.
fn effective_temperature(model: &str, requested: Option<f32>) -> Option<f32> {
    if model.starts_with("bedrock_sigv4::") {
        None
    } else {
        requested
    }
}

/// Our provider-neutral [`ThinkingLevel`] → genai's `ReasoningEffort`,
/// which the adapters map per provider: OpenAI-family endpoints
/// (zai, zai_coding, open_router, baseten) send `reasoning_effort`,
/// Gemini maps to `generationConfig.thinkingConfig.thinkingLevel`
/// (or a thinking budget), Anthropic-family and Bedrock-Anthropic to a
/// `thinking` budget. 1:1 on keywords (`none` is genai's `Zero`, the
/// explicit no-reasoning request).
fn genai_reasoning_effort(level: ThinkingLevel) -> genai::chat::ReasoningEffort {
    use genai::chat::ReasoningEffort;
    match level {
        ThinkingLevel::None => ReasoningEffort::Zero,
        ThinkingLevel::Minimal => ReasoningEffort::Minimal,
        ThinkingLevel::Low => ReasoningEffort::Low,
        ThinkingLevel::Medium => ReasoningEffort::Medium,
        ThinkingLevel::High => ReasoningEffort::High,
        ThinkingLevel::Xhigh => ReasoningEffort::XHigh,
        ThinkingLevel::Max => ReasoningEffort::Max,
    }
}

/// Whether a thinking level can be honored for a (namespace-qualified)
/// model string. Returns `Err(reason)` when the provider layer maps
/// NOTHING for that model — callers reject the request up front (a
/// clear 400) instead of silently running at the provider default or
/// failing mid-run.
///
/// Mirrors the pinned genai Bedrock adapter's publisher detection. It maps
/// reasoning for Anthropic, Amazon Nova, and OpenAI (flat `reasoning_effort`
/// for GPT-OSS; nested `reasoning.effort` for newer OpenAI models).
/// Other publishers would silently drop the setting, so reject them here.
/// This checks serialization support, not each model's accepted effort
/// vocabulary: unsupported keywords remain explicit provider errors.
pub fn thinking_level_supported(model: &str) -> Result<(), String> {
    let Some(id) = model
        .strip_prefix("bedrock_sigv4::")
        .or_else(|| model.strip_prefix("bedrock_api::"))
    else {
        return Ok(());
    };
    let mut segments = id.split('.');
    let publisher = match segments.next().unwrap_or_default() {
        "us" | "eu" | "apac" | "global" | "in" => segments.next().unwrap_or_default(),
        publisher => publisher,
    };
    match publisher {
        "anthropic" | "amazon" | "openai" => Ok(()),
        other => Err(format!(
            "thinking_level is not supported for bedrock model '{id}': the Bedrock \
             adapter maps reasoning only for anthropic-, amazon-, and \
             openai-publisher models, not '{other}' (the level would be \
             silently ignored)"
        )),
    }
}

/// Namespace-qualify a model name the way the server's default
/// provider does (`PROMPT_EXPLORE_PROVIDER`, e.g. "zai" →
/// `zai_coding::`, "bedrock" → `bedrock_sigv4::`); namespaced names
/// pass through. Shared so request validation and the provider layer
/// agree on what a bare name resolves to.
pub fn qualify_model(model: &str, default_provider: &str) -> String {
    if model.contains("::") {
        return model.to_string();
    }
    let ns = match default_provider {
        "zai" | "zai_coding" => "zai_coding",
        "zai_standard" | "zai::" => "zai",
        "openrouter" | "open_router" => "open_router",
        "bedrock" | "bedrock_sigv4" => "bedrock_sigv4",
        "baseten" => "baseten",
        "gemini" | "vertex" => "vertex",
        other => return format!("{other}::{model}"),
    };
    format!("{ns}::{model}")
}

/// Retry transient HTTP/transport failures with linear backoff and positive
/// jitter, honoring a longer provider Retry-After. A per-attempt deadline
/// (see `DEFAULT_REQUEST_TIMEOUT_MS`) turns a stalled request into one of those
/// transient failures. The budget is per completion,
/// not per investigation; authentication, validation, and hard quota failures
/// still fail fast. Keep raw prompts, responses, and headers out of retry logs.
async fn retry_provider_call<T, F, Fut>(
    model: &str,
    retry: RetrySettings,
    mut call: F,
) -> Result<T, LlmError>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = genai::Result<T>>,
{
    let mut retries = 0;
    loop {
        match deadline_attempt(retry.request_timeout, call()).await {
            Ok(response) => return Ok(response),
            Err(err) if retries < retry.max_retries && attempt_is_retryable(&err) => {
                retries += 1;
                let backoff = retry.backoff_attempt(retries, &err, std::time::SystemTime::now());
                eprintln!(
                    "LLM {model}: retry {retries}/{} in {:.3}s ({})",
                    retry.max_retries,
                    backoff.as_secs_f64(),
                    attempt_kind(&err),
                );
                tokio::time::sleep(backoff).await;
            }
            Err(err) => return Err(LlmError::Provider(attempt_message(&err, retries + 1))),
        }
    }
}

/// Run one attempt under the configured deadline. Cancelling the future on
/// expiry drops the in-flight request and its connection, so a stalled socket
/// cannot outlive the attempt.
async fn deadline_attempt<T, Fut>(
    limit: Option<std::time::Duration>,
    attempt: Fut,
) -> Result<T, AttemptError>
where
    Fut: std::future::Future<Output = genai::Result<T>>,
{
    match limit {
        None => attempt.await.map_err(AttemptError::Provider),
        Some(limit) => match tokio::time::timeout(limit, attempt).await {
            Ok(result) => result.map_err(AttemptError::Provider),
            Err(_elapsed) => Err(AttemptError::Timeout(limit)),
        },
    }
}

fn attempt_is_retryable(err: &AttemptError) -> bool {
    match err {
        AttemptError::Timeout(_) => true,
        AttemptError::Provider(err) => is_retryable(err),
    }
}

/// Short retry-log label. Never includes prompt or response bytes.
fn attempt_kind(err: &AttemptError) -> String {
    match err {
        AttemptError::Timeout(limit) => format!("no response within {}", format_deadline(*limit)),
        AttemptError::Provider(err) => err.status().map_or_else(
            || "transport/response failure".to_string(),
            |status| format!("HTTP {status}"),
        ),
    }
}

/// Whole-second deadlines read as `60s`; sub-second ones keep their unit so a
/// configured `15` does not print as `0s`.
fn format_deadline(limit: std::time::Duration) -> String {
    let millis = limit.as_millis();
    if millis % 1000 == 0 {
        format!("{}s", limit.as_secs())
    } else {
        format!("{millis}ms")
    }
}

/// Terminal message after the budget is spent. The attempt count matters
/// because a timeout is indistinguishable from a hung provider without it.
fn attempt_message(err: &AttemptError, attempts: u32) -> String {
    match err {
        AttemptError::Timeout(limit) => format!(
            "provider request timed out with no response within {} on each of \
             {attempts} attempt(s)",
            format_deadline(*limit)
        ),
        AttemptError::Provider(err) => provider_error_message(err),
    }
}

/// Use typed status/transport errors, never numbers or error-like text in a
/// provider body (a validation error can quote a prompt containing "503").
fn is_retryable(err: &genai::Error) -> bool {
    if let Some(status) = err.status() {
        return match status.as_u16() {
            408 => true,
            429 => is_retryable_429(&err.to_string()),
            // Not Implemented / HTTP Version Not Supported require a client
            // or endpoint change, not waiting for an overloaded server.
            501 | 505 => false,
            500..=599 => true,
            _ => false,
        };
    }
    match err {
        genai::Error::WebModelCall { webc_error, .. }
        | genai::Error::WebAdapterCall { webc_error, .. } => match webc_error {
            genai::webc::Error::Reqwest(err) => {
                err.is_timeout()
                    || err.is_connect()
                    || err.is_request()
                    || err.is_body()
                    || err.is_decode()
            }
            // A gateway can return a truncated JSON envelope or an HTML error
            // page with HTTP 200. These are transport responses, NOT malformed
            // JSON in the model's content (the simulator repairs that itself).
            genai::webc::Error::ResponseFailedInvalidJson { .. }
            | genai::webc::Error::ResponseFailedNotJson { .. } => true,
            _ => false,
        },
        _ => false,
    }
}

/// genai's Display stops at reqwest's outer message ("error sending request").
/// Preserve the underlying timeout/reset/TLS cause when retries are exhausted,
/// without dumping Debug payloads or request headers containing credentials.
fn provider_error_message(err: &genai::Error) -> String {
    use std::error::Error;
    let mut message = err.to_string();
    if let genai::Error::WebModelCall {
        webc_error: genai::webc::Error::Reqwest(request_error),
        ..
    }
    | genai::Error::WebAdapterCall {
        webc_error: genai::webc::Error::Reqwest(request_error),
        ..
    } = err
    {
        let mut cause = request_error.source();
        while let Some(error) = cause {
            message.push_str(": ");
            message.push_str(&error.to_string());
            cause = error.source();
        }
    }
    message
}

/// A 429 that's worth retrying: a transient per-request or shared-pool
/// rate limit, NOT a long quota window or a billing limit. Detected from
/// genai's flattened error string (which includes the HTTP status and the
/// response body).
fn is_retryable_429(err: &str) -> bool {
    if !err.contains("429") {
        return false;
    }
    let lower = err.to_lowercase();
    // OpenRouter's upstream shared-pool 429 carries the provider error code
    // `insufficient_quota` — which otherwise reads as a billing limit — but
    // its body explicitly says "temporarily rate-limited upstream. Please
    // retry shortly". That one is transient: retry it.
    if lower.contains("please retry shortly") || lower.contains("temporarily rate-limited upstream")
    {
        return true;
    }
    // Quota-window / billing signals — backoff won't help, so don't retry.
    // ("quota" also covers `insufficient_quota` absent a retry hint, e.g.
    // OpenAI's "You exceeded your current quota, please check your plan and
    // billing details".)
    let hard_quota = [
        "usage limit",
        "5 hour",
        "quota",
        "insufficient balance",
        "no resource package",
        "please recharge",
    ];
    !hard_quota.iter().any(|q| lower.contains(q))
}

#[derive(Debug, Clone, Copy)]
struct RetrySettings {
    max_retries: u32,
    base_delay_ms: u64,
    jitter_percent: u64,
    /// `None` = no per-attempt deadline (disables the timeout).
    request_timeout: Option<std::time::Duration>,
}

/// Deadline for one attempt; `0` means "no deadline" rather than "expire
/// immediately", because a zero-ms timeout would cancel every call.
fn attempt_deadline(millis: u64) -> Option<std::time::Duration> {
    (millis > 0).then(|| std::time::Duration::from_millis(millis))
}

/// Why one attempt failed. A timeout is not a provider answer, but it is
/// retryable for the same reason a dropped connection is.
enum AttemptError {
    Provider(genai::Error),
    Timeout(std::time::Duration),
}

impl RetrySettings {
    fn backoff(
        self,
        attempt: u32,
        err: &genai::Error,
        now: std::time::SystemTime,
    ) -> std::time::Duration {
        let delay = retry_delay(attempt, self.base_delay_ms)
            .max(provider_retry_after(err, now).unwrap_or_default());
        jittered(delay, self.jitter_percent)
    }

    /// Backoff for either kind of attempt failure. A timeout carries no
    /// provider Retry-After, so only our own delay applies; a provider answer
    /// may ask for a longer wait.
    fn backoff_attempt(
        self,
        attempt: u32,
        err: &AttemptError,
        now: std::time::SystemTime,
    ) -> std::time::Duration {
        match err {
            AttemptError::Timeout(_) => jittered(
                retry_delay(attempt, self.base_delay_ms),
                self.jitter_percent,
            ),
            AttemptError::Provider(err) => self.backoff(attempt, err, now),
        }
    }

    fn from_env() -> Self {
        Self {
            max_retries: env_number("PROMPT_EXPLORE_MAX_RETRIES", DEFAULT_MAX_RETRIES),
            base_delay_ms: env_number(
                "PROMPT_EXPLORE_RETRY_BASE_DELAY_MS",
                DEFAULT_RETRY_BASE_DELAY_MS,
            ),
            jitter_percent: env_number(
                "PROMPT_EXPLORE_RETRY_JITTER_PERCENT",
                DEFAULT_RETRY_JITTER_PERCENT,
            ),
            request_timeout: attempt_deadline(env_number(
                "PROMPT_EXPLORE_REQUEST_TIMEOUT_MS",
                DEFAULT_REQUEST_TIMEOUT_MS,
            )),
        }
    }
}

/// Provider delays are minimum waits. Accept Retry-After seconds/HTTP-date
/// and the millisecond variant; if both are present, respect the longer one.
fn provider_retry_after(
    err: &genai::Error,
    now: std::time::SystemTime,
) -> Option<std::time::Duration> {
    use std::time::Duration;
    let headers = err.headers()?;
    let milliseconds = headers
        .get("retry-after-ms")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_millis);
    let standard = headers
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| {
            v.trim()
                .parse::<u64>()
                .ok()
                .and_then(|seconds| seconds.checked_mul(1000))
                .map(Duration::from_millis)
                .or_else(|| {
                    httpdate::parse_http_date(v)
                        .ok()
                        .map(|date| date.duration_since(now).unwrap_or_default())
                })
        });
    milliseconds.max(standard)
}

fn env_number<T: std::str::FromStr>(name: &str, default: T) -> T {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse().ok())
        .unwrap_or(default)
}

/// Linear backoff for retry n, in caller-configured millisecond steps.
fn retry_delay(retry: u32, base_delay_ms: u64) -> std::time::Duration {
    std::time::Duration::from_millis(base_delay_ms.saturating_mul(u64::from(retry)))
}

/// Positive jitter on top of the base delay, so concurrent retries do not
/// all fire on the same tick. Poor-man's jitter from wall-clock nanos (no
/// rand dep).
fn jittered(base: std::time::Duration, jitter_percent: u64) -> std::time::Duration {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let sampled_percent = nanos % jitter_percent.saturating_add(1).max(1);
    let jitter_ms =
        (base.as_millis() * u128::from(sampled_percent) / 100).min(u128::from(u64::MAX)) as u64;
    base.saturating_add(std::time::Duration::from_millis(jitter_ms))
}

#[cfg(test)]
mod retry_tests;

#[cfg(test)]
mod tests {
    use super::*;

    // The exact OpenRouter upstream shared-pool 429 seen in production:
    // tagged `insufficient_quota` but explicitly retryable.
    const OPENROUTER_SHARED_POOL_429: &str = "Web call failed for model \
        'open_router::qwen/qwen3.7-flash (adapter: OpenRouter)'. Cause: \
        Request failed with status code '429 Too Many Requests'. Response \
        body: {\"error\":{\"message\":\"Provider returned error\",\"code\":429,
        \"metadata\":{\"raw\":\"qwen/qwen3.7-flash is temporarily \
        rate-limited upstream. Please retry shortly, or add your own key to \
        accumulate your rate limits: https://openrouter.ai/settings/integrations
        \",\"provider_name\":\"Alibaba\",\"is_byok\":false,
        \"provider_error_code\":\"insufficient_quota\",
        \"limit_source\":\"upstream_provider_shared_pool\",
        \"remedy_hint\":\"Retry shortly, add your own provider key \
        (https://openrouter.ai/settings/integrations), or route to another \
        provider with provider routing: https://openrouter.ai/docs/features/provider-routing\"}}}";

    #[test]
    fn consecutive_tool_messages_merge_into_one() {
        // One assistant completion with two parallel tool calls, then the
        // two results the runner emits (one Message::Tool per call). They
        // must land in ONE tool message so the Bedrock adapter renders a
        // single user message carrying both toolResult blocks.
        let messages = vec![
            Message::System {
                content: "sys".into(),
            },
            Message::User {
                content: "hi".into(),
            },
            Message::Assistant {
                content: Some("calling".into()),
                tool_calls: vec![
                    ToolCallRequest {
                        id: "call_a".into(),
                        name: "f".into(),
                        arguments: "{}".into(),
                    },
                    ToolCallRequest {
                        id: "call_b".into(),
                        name: "g".into(),
                        arguments: "{}".into(),
                    },
                ],
            },
            Message::Tool {
                tool_call_id: "call_a".into(),
                content: "one".into(),
            },
            Message::Tool {
                tool_call_id: "call_b".into(),
                content: "two".into(),
            },
        ];
        let out = convert_messages(messages);
        assert_eq!(out.len(), 4, "system, user, assistant, one merged tool");
        let tool_msg = out.last().unwrap();
        assert_eq!(tool_msg.role, genai::chat::ChatRole::Tool);
        let responses: Vec<_> = tool_msg
            .content
            .iter()
            .filter_map(|p| p.as_tool_response().cloned())
            .collect();
        assert_eq!(responses.len(), 2);
        assert_eq!(responses[0].call_id, "call_a");
        assert_eq!(responses[0].content, "one");
        assert_eq!(responses[1].call_id, "call_b");
        assert_eq!(responses[1].content, "two");
    }

    #[test]
    fn tool_results_from_separate_turns_stay_separate() {
        // Two assistant turns, each with its own tool call: the merge must
        // not span turns — each turn's result stays in its own message,
        // separated by the next assistant message.
        let messages = vec![
            Message::Assistant {
                content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "call_a".into(),
                    name: "f".into(),
                    arguments: "{}".into(),
                }],
            },
            Message::Tool {
                tool_call_id: "call_a".into(),
                content: "one".into(),
            },
            Message::Assistant {
                content: None,
                tool_calls: vec![ToolCallRequest {
                    id: "call_b".into(),
                    name: "f".into(),
                    arguments: "{}".into(),
                }],
            },
            Message::Tool {
                tool_call_id: "call_b".into(),
                content: "two".into(),
            },
        ];
        let out = convert_messages(messages);
        assert_eq!(out.len(), 4);
        for (i, expected_id) in ["call_a", "call_b"].iter().enumerate() {
            assert_eq!(out[i * 2].role, genai::chat::ChatRole::Assistant);
            assert_eq!(out[i * 2 + 1].role, genai::chat::ChatRole::Tool);
            let responses: Vec<_> = out[i * 2 + 1]
                .content
                .iter()
                .filter_map(|p| p.as_tool_response().cloned())
                .collect();
            assert_eq!(responses.len(), 1);
            assert_eq!(responses[0].call_id, *expected_id);
        }
    }

    #[test]
    fn bedrock_requests_omit_temperature() {
        // Issue #3's exact models: newer Bedrock rejects any temperature
        // field, so the namespace never sends one — requested or not.
        for m in [
            "bedrock_sigv4::global.openai.gpt-5.6-luna",
            "bedrock_sigv4::eu.openai.gpt-5.6-luna",
            "bedrock_sigv4::global.anthropic.claude-opus-5",
            "bedrock_sigv4::anthropic.claude-sonnet-4-5-20250929-v1:0",
            "bedrock_sigv4::amazon.nova-pro-v1:0",
        ] {
            assert_eq!(effective_temperature(m, Some(0.7)), None, "{m}");
            assert_eq!(effective_temperature(m, None), None, "{m}");
        }
    }

    #[test]
    fn non_bedrock_providers_keep_requested_temperature() {
        for m in [
            "zai_coding::glm-5.2",
            "zai::glm-4.6",
            "open_router::qwen/qwen3.7-flash",
            "vertex::gemini-2.5-pro",
            "baseten::deepseek-ai/deepseek-v4-flash",
        ] {
            assert_eq!(effective_temperature(m, Some(0.7)), Some(0.7), "{m}");
            assert_eq!(effective_temperature(m, None), None, "{m}");
        }
    }

    #[test]
    fn bare_name_under_bedrock_default_qualifies_to_bedrock() {
        // PROMPT_EXPLORE_PROVIDER=bedrock makes bare names bedrock
        // models — they must hit the temperature rule too.
        let qualified = DefaultProvider::Bedrock.qualify("foo");
        assert_eq!(qualified, "bedrock_sigv4::foo");
        assert_eq!(effective_temperature(&qualified, Some(0.7)), None);
        // And a non-bedrock default keeps steering bare names.
        let qualified = DefaultProvider::ZaiCoding.qualify("foo");
        assert_eq!(effective_temperature(&qualified, Some(0.7)), Some(0.7));
    }

    #[test]
    fn thinking_level_maps_one_to_one_to_genai_effort() {
        use genai::chat::ReasoningEffort;
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::None),
            ReasoningEffort::Zero
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::Minimal),
            ReasoningEffort::Minimal
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::Low),
            ReasoningEffort::Low
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::Medium),
            ReasoningEffort::Medium
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::High),
            ReasoningEffort::High
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::Xhigh),
            ReasoningEffort::XHigh
        ));
        assert!(matches!(
            genai_reasoning_effort(ThinkingLevel::Max),
            ReasoningEffort::Max
        ));
    }

    #[test]
    fn thinking_level_supported_everywhere_except_non_publisher_bedrock() {
        // OpenAI-family, zai, gemini, baseten: mapped by their adapters.
        for m in [
            "open_router::openai/gpt-5.6-luna",
            "zai_coding::glm-5.3",
            "zai::glm-4.6",
            "vertex::gemini-3-pro",
            "baseten::deepseek-ai/deepseek-v4-flash",
        ] {
            assert!(thinking_level_supported(m).is_ok(), "{m}");
        }
        for adapter in ["bedrock_sigv4", "bedrock_api"] {
            for prefix in ["", "us.", "eu.", "apac.", "global.", "in."] {
                for model in [
                    "anthropic.claude-sonnet-5",
                    "amazon.nova-pro-v1:0",
                    "openai.gpt-oss-20b-1:0",
                    "openai.gpt-oss-120b-1:0",
                    "openai.gpt-5.6-luna",
                    "openai.gpt-5.6-terra",
                    "openai.gpt-5.6-sol",
                    "openai.gpt-6-astra",
                ] {
                    let model = format!("{adapter}::{prefix}{model}");
                    assert!(thinking_level_supported(&model).is_ok(), "{model}");
                }
                for model in ["meta.llama3-1-70b", "custom.openai.gpt-5.6-luna"] {
                    let model = format!("{adapter}::{prefix}{model}");
                    let err = thinking_level_supported(&model).expect_err("unmapped publisher");
                    assert!(err.contains("not supported"), "{err}");
                    assert!(err.contains(&model.split_once("::").unwrap().1), "{err}");
                }
            }
        }
    }

    #[test]
    fn qualify_model_matches_server_provider_names() {
        // The names PROMPT_EXPLORE_PROVIDER accepts (server docs) map to
        // the same namespaces the ModelMapper produces.
        assert_eq!(qualify_model("glm-5.3", "zai"), "zai_coding::glm-5.3");
        assert_eq!(qualify_model("glm-4.6", "zai_standard"), "zai::glm-4.6");
        assert_eq!(qualify_model("x", "openrouter"), "open_router::x");
        assert_eq!(qualify_model("x", "bedrock"), "bedrock_sigv4::x");
        assert_eq!(qualify_model("x", "gemini"), "vertex::x");
        // Namespaced names pass through untouched.
        assert_eq!(
            qualify_model("bedrock_sigv4::global.anthropic.claude-opus-5", "zai"),
            "bedrock_sigv4::global.anthropic.claude-opus-5"
        );
    }

    #[test]
    fn openrouter_shared_pool_429_is_retryable() {
        assert!(is_retryable_429(OPENROUTER_SHARED_POOL_429));
    }

    #[test]
    fn plain_rate_limit_429_is_retryable() {
        // z.ai code 1302 — transient per-request rate limit.
        let err = "Web call failed. Cause: statusCode=429, body: {\"error\":{\"code\":\"1302\",\"message\":\"Rate limit reached for requests\"}}";
        assert!(is_retryable_429(err));
    }

    #[test]
    fn quota_window_429_is_not_retryable() {
        // z.ai code 1308 — 5-hour usage window; backoff won't cross it.
        let err = "Web call failed. Cause: statusCode=429, body: {\"error\":{\"code\":\"1308\",\"message\":\"Usage limit reached for 5 hour window, please top up\"}}";
        assert!(!is_retryable_429(err));
    }

    #[test]
    fn exhausted_subscription_balance_429_is_not_retryable() {
        let err = "Web call failed. Cause: statusCode=429, body: {\"error\":{\"code\":\"1113\",\"message\":\"Insufficient balance or no resource package. Please recharge.\"}}";
        assert!(!is_retryable_429(err));
    }

    #[test]
    fn billing_quota_429_is_not_retryable() {
        // OpenAI billing limit — `insufficient_quota` without a retry hint.
        let err = "Web call failed. Cause: statusCode=429, body: {\"error\":{\"message\":\"You exceeded your current quota, please check your plan and billing details\",\"type\":\"insufficient_quota\"}}";
        assert!(!is_retryable_429(err));
    }

    #[test]
    fn non_429_errors_are_not_retried() {
        assert!(!is_retryable_429(
            "Web call failed. Cause: statusCode=500, body: internal error"
        ));
        assert!(!is_retryable_429("connection reset by peer"));
    }

    #[test]
    fn backoff_is_linear_five_second_steps() {
        assert_eq!(
            retry_delay(1, DEFAULT_RETRY_BASE_DELAY_MS),
            std::time::Duration::from_secs(5)
        );
        assert_eq!(
            retry_delay(2, DEFAULT_RETRY_BASE_DELAY_MS),
            std::time::Duration::from_secs(10)
        );
        assert_eq!(
            retry_delay(3, DEFAULT_RETRY_BASE_DELAY_MS),
            std::time::Duration::from_secs(15)
        );
        assert_eq!(
            retry_delay(10, DEFAULT_RETRY_BASE_DELAY_MS),
            std::time::Duration::from_secs(50)
        );
        // Jitter only ever adds, never subtracts.
        for n in 1..=DEFAULT_MAX_RETRIES {
            assert!(
                jittered(
                    retry_delay(n, DEFAULT_RETRY_BASE_DELAY_MS),
                    DEFAULT_RETRY_JITTER_PERCENT
                ) >= retry_delay(n, DEFAULT_RETRY_BASE_DELAY_MS)
            );
        }
    }
}

fn convert_response(resp: GChatResponse) -> ChatResponse {
    let tool_calls = resp
        .content
        .tool_calls()
        .into_iter()
        .map(|tc| ToolCallRequest {
            id: tc.call_id.clone(),
            name: tc.fn_name.clone(),
            arguments: tc.fn_arguments.to_string(),
        })
        .collect();

    // Reasoning lives as a sibling field on genai's response (the
    // OpenAI-family adapters put `/message/reasoning` there), but some
    // adapters may carry it as content parts — take whichever is set.
    let thinking = resp
        .reasoning_content
        .or_else(|| resp.content.joined_reasoning_content());
    let content = resp.content.into_first_text();

    let usage = {
        let u = &resp.usage;
        let has_any = u.prompt_tokens.is_some() || u.completion_tokens.is_some();
        has_any.then(|| Usage {
            input_tokens: u.prompt_tokens.unwrap_or(0).max(0) as u64,
            cache_read_tokens: u
                .prompt_tokens_details
                .as_ref()
                .and_then(|d| d.cached_tokens)
                .unwrap_or(0)
                .max(0) as u64,
            output_tokens: u.completion_tokens.unwrap_or(0).max(0) as u64,
        })
    };

    ChatResponse {
        content,
        thinking,
        tool_calls,
        usage,
    }
}

fn convert_messages(messages: Vec<Message>) -> Vec<ChatMessage> {
    // Consecutive tool results are merged into a single tool message so
    // every adapter sees all of an assistant turn's tool responses
    // together. Bedrock Converse requires every toolResult for one
    // assistant message's tool_use blocks to live in ONE user message;
    // one message per result produced one user message per result and
    // Bedrock rejected the follow-up completion with 400 "Expected
    // toolResult blocks at messages.N.content for the following Ids".
    // The other adapters iterate the parts and are unchanged: OpenAI
    // re-emits one tool message per part, Anthropic/Gemini/Bedrock
    // collect the parts into a single user message.
    let mut out: Vec<ChatMessage> = Vec::with_capacity(messages.len());
    let mut pending: Vec<genai::chat::ToolResponse> = Vec::new();
    for m in messages {
        match m {
            Message::Tool {
                tool_call_id,
                content,
            } => pending.push(genai::chat::ToolResponse::new(tool_call_id, content)),
            other => {
                if !pending.is_empty() {
                    let parts = pending
                        .drain(..)
                        .map(genai::chat::ContentPart::ToolResponse)
                        .collect::<Vec<_>>();
                    out.push(ChatMessage::tool(MessageContent::from_parts(parts)));
                }
                out.push(convert_message(other));
            }
        }
    }
    if !pending.is_empty() {
        let parts = pending
            .drain(..)
            .map(genai::chat::ContentPart::ToolResponse)
            .collect::<Vec<_>>();
        out.push(ChatMessage::tool(MessageContent::from_parts(parts)));
    }
    out
}

fn convert_message(m: Message) -> ChatMessage {
    match m {
        Message::System { content } => ChatMessage::system(content),
        Message::User { content } => ChatMessage::user(content),
        Message::Assistant {
            content,
            tool_calls,
        } => {
            let mut parts = Vec::new();
            if let Some(text) = content {
                if !text.is_empty() {
                    parts.push(genai::chat::ContentPart::Text(text));
                }
            }
            for tc in tool_calls {
                let args: Value =
                    serde_json::from_str(&tc.arguments).unwrap_or(Value::String(tc.arguments));
                parts.push(genai::chat::ContentPart::ToolCall(genai::chat::ToolCall {
                    call_id: tc.id,
                    fn_name: tc.name,
                    fn_arguments: args,
                    thought_signatures: None,
                }));
            }
            ChatMessage::assistant(MessageContent::from_parts(parts))
        }
        Message::Tool {
            tool_call_id,
            content,
        } => ChatMessage::tool(genai::chat::ToolResponse::new(tool_call_id, content)),
    }
}

fn convert_tool(t: ToolDef) -> GTool {
    GTool::new(t.name)
        .with_description(t.description)
        .with_schema(t.parameters)
}
