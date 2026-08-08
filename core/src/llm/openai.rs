//! Adapter for OpenAI-compatible chat-completions APIs.
//!
//! Both z.ai (`https://api.z.ai/api/paas/v4`) and OpenRouter
//! (`https://openrouter.ai/api/v1`) speak this protocol, so provider
//! selection is pure configuration.

use async_openai::{
    Client,
    config::OpenAIConfig,
    types::chat::{
        ChatCompletionMessageToolCall, ChatCompletionMessageToolCalls,
        ChatCompletionRequestAssistantMessage, ChatCompletionRequestAssistantMessageContent,
        ChatCompletionRequestMessage, ChatCompletionRequestSystemMessage,
        ChatCompletionRequestSystemMessageContent, ChatCompletionRequestToolMessage,
        ChatCompletionRequestToolMessageContent, ChatCompletionRequestUserMessage,
        ChatCompletionRequestUserMessageContent, ChatCompletionTool, ChatCompletionTools,
        CreateChatCompletionRequest, FunctionCall, FunctionObject,
    },
};
use async_trait::async_trait;

use super::client::{LlmClient, LlmError};
use super::types::{ChatRequest, ChatResponse, Message, ToolCallRequest, ToolDef, Usage};

pub struct OpenAiCompatibleClient {
    client: Client<OpenAIConfig>,
}

impl OpenAiCompatibleClient {
    pub fn new(api_base: &str, api_key: &str) -> Self {
        let config = OpenAIConfig::new()
            .with_api_base(api_base)
            .with_api_key(api_key);
        Self {
            client: Client::with_config(config),
        }
    }

    /// z.ai coding plan subscription endpoint.
    pub fn zai(api_key: &str) -> Self {
        Self::new("https://api.z.ai/api/coding/paas/v4", api_key)
    }

    /// z.ai standard pay-per-use endpoint.
    pub fn zai_standard(api_key: &str) -> Self {
        Self::new("https://api.z.ai/api/paas/v4", api_key)
    }

    pub fn openrouter(api_key: &str) -> Self {
        Self::new("https://openrouter.ai/api/v1", api_key)
    }
}

#[async_trait]
impl LlmClient for OpenAiCompatibleClient {
    async fn complete(&self, req: ChatRequest) -> Result<ChatResponse, LlmError> {
        let request = build_request(req)?;
        let response = self
            .client
            .chat()
            .create(request)
            .await
            .map_err(|e| LlmError::Provider(e.to_string()))?;

        let choice = response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| LlmError::MalformedResponse("no choices".into()))?;

        let tool_calls = choice
            .message
            .tool_calls
            .unwrap_or_default()
            .into_iter()
            .filter_map(|tc| match tc {
                ChatCompletionMessageToolCalls::Function(f) => Some(ToolCallRequest {
                    id: f.id,
                    name: f.function.name,
                    arguments: f.function.arguments,
                }),
                _ => None,
            })
            .collect();

        Ok(ChatResponse {
            content: choice.message.content,
            tool_calls,
            usage: response.usage.map(|u| Usage {
                input_tokens: u.prompt_tokens as u64,
                output_tokens: u.completion_tokens as u64,
            }),
        })
    }
}

fn build_request(req: ChatRequest) -> Result<CreateChatCompletionRequest, LlmError> {
    let messages = req.messages.into_iter().map(convert_message).collect();
    let tools: Vec<ChatCompletionTools> = req.tools.into_iter().map(convert_tool).collect();

    Ok(CreateChatCompletionRequest {
        model: req.model,
        messages,
        tools: if tools.is_empty() { None } else { Some(tools) },
        temperature: req.temperature,
        max_completion_tokens: req.max_tokens,
        ..Default::default()
    })
}

fn convert_message(m: Message) -> ChatCompletionRequestMessage {
    match m {
        Message::System { content } => {
            ChatCompletionRequestMessage::System(ChatCompletionRequestSystemMessage {
                content: ChatCompletionRequestSystemMessageContent::Text(content),
                ..Default::default()
            })
        }
        Message::User { content } => {
            ChatCompletionRequestMessage::User(ChatCompletionRequestUserMessage {
                content: ChatCompletionRequestUserMessageContent::Text(content),
                ..Default::default()
            })
        }
        Message::Assistant {
            content,
            tool_calls,
        } => ChatCompletionRequestMessage::Assistant(ChatCompletionRequestAssistantMessage {
            content: content.map(ChatCompletionRequestAssistantMessageContent::Text),
            tool_calls: if tool_calls.is_empty() {
                None
            } else {
                Some(
                    tool_calls
                        .into_iter()
                        .map(|tc| {
                            ChatCompletionMessageToolCalls::Function(
                                ChatCompletionMessageToolCall {
                                    id: tc.id,
                                    function: FunctionCall {
                                        name: tc.name,
                                        arguments: tc.arguments,
                                    },
                                },
                            )
                        })
                        .collect(),
                )
            },
            ..Default::default()
        }),
        Message::Tool {
            tool_call_id,
            content,
        } => ChatCompletionRequestMessage::Tool(ChatCompletionRequestToolMessage {
            content: ChatCompletionRequestToolMessageContent::Text(content),
            tool_call_id,
        }),
    }
}

fn convert_tool(t: ToolDef) -> ChatCompletionTools {
    ChatCompletionTools::Function(ChatCompletionTool {
        function: FunctionObject {
            name: t.name,
            description: Some(t.description),
            parameters: Some(t.parameters),
            strict: None,
        },
    })
}
