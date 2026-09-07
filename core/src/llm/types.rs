//! Provider-neutral chat types. The rest of the codebase speaks only
//! these; provider quirks are confined to the adapter.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Provider-neutral thinking/reasoning level for one LLM role. The
/// vocabulary is deliberately small and shared across providers:
/// `none` explicitly requests NO reasoning, `minimal`…`max` scale
/// effort up. Omitting the field entirely means "provider default"
/// — which is a different thing from `none` (reasoning models come
/// with a non-trivial default, e.g. OpenAI Responses models default
/// to medium; `none` asks the provider to turn reasoning OFF).
///
/// Each provider maps what it supports (OpenAI-family endpoints send
/// `reasoning_effort`, Gemini maps to `thinkingLevel`, Bedrock
/// Anthropic models to a `thinking.budget_tokens`); combinations the
/// provider layer cannot honor are rejected at request time with a
/// clear error, not silently dropped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, utoipa::ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum ThinkingLevel {
    /// Explicitly request no reasoning (turns thinking OFF where the
    /// provider supports that — vs. omitting the field, which keeps
    /// the provider's default).
    None,
    /// Legacy minimal effort (older GPT-5 family models).
    Minimal,
    Low,
    Medium,
    High,
    /// Extra-high effort; providers without a distinct tier treat
    /// this as their highest.
    Xhigh,
    /// Maximum effort; providers without a distinct tier treat this
    /// as their highest.
    Max,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatRequest {
    pub model: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub temperature: Option<f32>,
    pub max_tokens: Option<u32>,
    /// Thinking/reasoning level for this completion. `None` (or
    /// absent) = the provider's default — no field is sent, exactly
    /// the pre-option behavior.
    #[serde(default)]
    pub thinking_level: Option<ThinkingLevel>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "role", rename_all = "snake_case")]
pub enum Message {
    System {
        content: String,
    },
    User {
        content: String,
    },
    Assistant {
        content: Option<String>,
        tool_calls: Vec<ToolCallRequest>,
    },
    /// Result of executing a tool call, fed back to the model.
    Tool {
        tool_call_id: String,
        content: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    /// JSON Schema for the parameters.
    pub parameters: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolCallRequest {
    pub id: String,
    pub name: String,
    /// Raw JSON arguments as produced by the model (may be malformed;
    /// parsing is the caller's decision).
    pub arguments: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatResponse {
    pub content: Option<String>,
    /// The model's visible reasoning ("thinking") for this completion,
    /// when the provider reports it. Surfaced in traces for transparency;
    /// never fed back into the conversation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thinking: Option<String>,
    pub tool_calls: Vec<ToolCallRequest>,
    pub usage: Option<Usage>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn thinking_level_serializes_snake_case_and_parses_back() {
        for (word, level) in [
            ("none", ThinkingLevel::None),
            ("minimal", ThinkingLevel::Minimal),
            ("low", ThinkingLevel::Low),
            ("medium", ThinkingLevel::Medium),
            ("high", ThinkingLevel::High),
            ("xhigh", ThinkingLevel::Xhigh),
            ("max", ThinkingLevel::Max),
        ] {
            assert_eq!(serde_json::to_value(level).unwrap(), serde_json::json!(word));
            let back: ThinkingLevel = serde_json::from_str(&format!("\"{word}\""))
                .unwrap_or_else(|e| panic!("{word}: {e}"));
            assert_eq!(back, level);
        }
    }

    #[test]
    fn thinking_level_rejects_unknown_words() {
        // Fail fast at parse time with serde's unknown-variant error —
        // not a transport-shaped 500 mid-run (issue #4).
        assert!(serde_json::from_str::<ThinkingLevel>("\"ultra\"").is_err());
        assert!(serde_json::from_str::<ThinkingLevel>("\"MEDIUM\"").is_err());
        assert!(serde_json::from_str::<ThinkingLevel>("3").is_err());
    }

    #[test]
    fn chat_request_without_thinking_level_still_deserializes() {
        // Back-compat: pre-option serialized requests (mock scripts,
        // fixtures) carry no thinking_level and must mean "provider
        // default".
        let req: ChatRequest = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],
               "tools":[],"temperature":null,"max_tokens":null}"#,
        )
        .unwrap();
        assert_eq!(req.thinking_level, None);
        let req: ChatRequest = serde_json::from_str(
            r#"{"model":"m","messages":[{"role":"user","content":"hi"}],
               "tools":[],"temperature":null,"max_tokens":null,
               "thinking_level":"none"}"#,
        )
        .unwrap();
        assert_eq!(req.thinking_level, Some(ThinkingLevel::None));
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct Usage {
    pub input_tokens: u64,
    /// Prompt tokens served from the provider's cache (a subset of
    /// `input_tokens`, usually billed at a lower rate). Zero when the
    /// provider doesn't report it.
    #[serde(default)]
    pub cache_read_tokens: u64,
    pub output_tokens: u64,
}
