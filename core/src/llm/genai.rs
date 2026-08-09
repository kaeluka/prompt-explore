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
//! | bare name (e.g. `glm-5.2`) | resolved by [`ModelMapper`] (ZAI default — see [`ProviderClient::zai`]) | per provider |
//!
//! The `bedrock-sigv4` cargo feature pulls in `aws-config` for the full
//! AWS credential chain, so `aws sso login` works with no extra setup.

use async_trait::async_trait;
use genai::chat::{
    ChatMessage, ChatOptions, ChatRequest as GChatRequest, ChatResponse as GChatResponse,
    MessageContent, Tool as GTool,
};
use genai::adapter::AdapterKind;
use genai::resolver::{AuthData, ModelMapper, ServiceTargetResolver};
use genai::{Client, ModelIden, ModelName, ServiceTarget};
use serde_json::Value;

// Re-export so the server (which depends only on core) can build a
// listing Client without adding genai as a direct dependency.
pub use genai::Client as GenaiClient;

use super::client::{LlmClient, LlmError};
use super::types::{
    ChatRequest, ChatResponse, Message, ToolCallRequest, ToolDef, Usage,
};

/// Which provider a bare (un-namespaced) model name resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DefaultProvider {
    ZaiCoding,
    Zai,
    OpenRouter,
    Bedrock,
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
        }
    }

    /// The adapter kind the qualified name routes to.
    fn adapter_kind(self) -> AdapterKind {
        match self {
            Self::ZaiCoding | Self::Zai => AdapterKind::Zai,
            Self::OpenRouter => AdapterKind::OpenRouter,
            Self::Bedrock => AdapterKind::BedrockSigv4,
        }
    }
}

pub struct ProviderClient {
    client: Client,
}

impl ProviderClient {
    fn with_model_mapper(default: DefaultProvider) -> Self {
        let client = Client::builder()
            .with_model_mapper(ModelMapper::from_mapper_fn(move |model_ident: ModelIden| {
                // Namespaced model strings already resolved to the right
                // adapter — pass through. Bare names get the client's
                // default provider.
                if model_ident.model_name.namespace().is_some() {
                    return Ok(model_ident);
                }
                Ok(ModelIden::new(
                    default.adapter_kind(),
                    ModelName::new(default.qualify(model_ident.model_name.as_str())),
                ))
            }))
            .build();
        Self { client }
    }

    /// z.ai **coding-plan** endpoint; bare model names default to it.
    /// The adapter reads `ZAI_API_KEY` from the environment.
    pub fn zai() -> Self {
        Self::with_model_mapper(DefaultProvider::ZaiCoding)
    }

    /// z.ai standard pay-per-use endpoint.
    pub fn zai_standard() -> Self {
        Self::with_model_mapper(DefaultProvider::Zai)
    }

    /// OpenRouter gateway; adapter reads `OPENROUTER_API_KEY`.
    pub fn openrouter() -> Self {
        Self::with_model_mapper(DefaultProvider::OpenRouter)
    }

    /// AWS Bedrock via native Converse + SigV4. Credentials come from the
    /// default AWS chain (`aws sso login`, profiles, env, IMDS) — no API key.
    pub fn bedrock() -> Self {
        Self::with_model_mapper(DefaultProvider::Bedrock)
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
                .build(),
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
        if let Some(t) = req.temperature {
            options = options.with_temperature(t as f64);
        }
        if let Some(m) = req.max_tokens {
            options = options.with_max_tokens(m);
        }

        // Retry transient 429 rate limits (e.g. z.ai code 1302 "Rate limit
        // reached for requests") with exponential backoff + jitter. A
        // QUOTA-window 429 (e.g. z.ai code 1308 "Usage limit reached for
        // 5 hour") is NOT retried — backoff won't help, so fail fast.
        // Providers don't surface a usable Retry-After here (the body has
        // no duration and genai flattens response headers into the error
        // string), so we backoff ourselves.
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt: u32 = 0;
        loop {
            attempt += 1;
            match self
                .client
                .exec_chat(&req.model, chat_req.clone(), Some(&options))
                .await
            {
                Ok(resp) => return Ok(convert_response(resp)),
                Err(e) => {
                    let msg = e.to_string();
                    if attempt < MAX_ATTEMPTS && is_transient_429(&msg) {
                        let backoff = backoff_with_jitter(attempt);
                        tokio::time::sleep(backoff).await;
                        continue;
                    }
                    return Err(LlmError::Provider(msg));
                }
            }
        }
    }
}

/// A 429 that's worth retrying: a transient per-request rate limit, NOT a
/// long quota window. Detected from genai's flattened error string (which
/// includes the HTTP status and the response body).
fn is_transient_429(err: &str) -> bool {
    if !err.contains("429") {
        return false;
    }
    // Quota-window signals — backoff won't help, so don't retry.
    let quota = [
        "usage limit",
        "5 hour",
        "quota",
        "exceeded your current quota",
        "insufficient_quota",
    ];
    !quota.iter().any(|q| err.to_lowercase().contains(q))
}

/// Exponential backoff (0.5, 1, 2, 4, …s) plus up to ~40% jitter so that
/// N concurrent retries (e.g. a 10-scenario run hitting a rate limit at
/// once) don't all fire on the same tick and re-trigger it.
fn backoff_with_jitter(attempt: u32) -> std::time::Duration {
    let base_ms: u64 = 500_u64.checked_shl(attempt.saturating_sub(1)).unwrap_or(8_000);
    let cap_ms = base_ms.min(8_000);
    // Poor-man's jitter from wall-clock nanos (no rand dep): 0..=40% of cap.
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.subsec_nanos() as u64)
        .unwrap_or(0);
    let jitter_ms = cap_ms * (nanos % 5) / 10; // 0, 10, 20, 30, or 40%
    std::time::Duration::from_millis(cap_ms + jitter_ms)
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
        tool_calls,
        usage,
    }
}

fn convert_messages(messages: Vec<Message>) -> Vec<ChatMessage> {
    messages.into_iter().map(convert_message).collect()
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
                let args: Value = serde_json::from_str(&tc.arguments)
                    .unwrap_or(Value::String(tc.arguments));
                parts.push(genai::chat::ContentPart::ToolCall(
                    genai::chat::ToolCall {
                        call_id: tc.id,
                        fn_name: tc.name,
                        fn_arguments: args,
                        thought_signatures: None,
                    },
                ));
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
