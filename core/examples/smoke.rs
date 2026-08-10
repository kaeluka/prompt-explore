//! Live smoke test for the LLM layer against a real provider.
//!
//! Usage:
//!   ZAI_API_KEY=...        cargo run --example smoke -- zai glm-4.5
//!   OPENROUTER_API_KEY=... cargo run --example smoke -- openrouter openai/gpt-4o-mini
//!   (after `gcloud auth application-default login`)
//!                          cargo run --example smoke -- gemini gemini-2.5-flash
//!
//! Sends a chat request with a tool defined; prints the model's
//! response and whether it called the tool.

use prompt_explore::llm::{ChatRequest, LlmClient, Message, ProviderClient, ToolDef};

#[tokio::main]
async fn main() {
    let provider = std::env::args().nth(1).unwrap_or_else(|| "zai".into());
    let model = std::env::args().nth(2).unwrap_or_else(|| "glm-4.5".into());

    let client: ProviderClient = match provider.as_str() {
        "zai" => ProviderClient::zai(),
        "zai_standard" => ProviderClient::zai_standard(),
        "openrouter" => ProviderClient::openrouter(),
        "bedrock" => ProviderClient::bedrock(),
        "gemini" => ProviderClient::gemini(),
        other => {
            eprintln!("unknown provider {other:?}; expected zai | zai_standard | openrouter | bedrock | gemini");
            std::process::exit(1);
        }
    };

    let req = ChatRequest {
        model: model.clone(),
        messages: vec![
            Message::System {
                content: "You are a support agent. Use the order_status tool \
                          when the customer asks about an order."
                    .into(),
            },
            Message::User {
                content: "where is my order #A-1234? it's two weeks late!".into(),
            },
        ],
        tools: vec![ToolDef {
            name: "order_status".into(),
            description: "Look up the status of an order by id.".into(),
            parameters: serde_json::json!({
                "type": "object",
                "properties": { "order_id": { "type": "string" } },
                "required": ["order_id"]
            }),
        }],
        temperature: Some(0.7),
        max_tokens: Some(512),
    };

    println!(">>> {provider} / {model}");
    match client.complete(req).await {
        Ok(resp) => {
            if let Some(content) = &resp.content {
                println!("content: {content}");
            }
            for tc in &resp.tool_calls {
                println!("tool_call: {}({})", tc.name, tc.arguments);
            }
            if let Some(u) = resp.usage {
                println!("usage: {} in / {} out", u.input_tokens, u.output_tokens);
            }
        }
        Err(e) => {
            eprintln!("request failed: {e}");
            std::process::exit(1);
        }
    }
}
