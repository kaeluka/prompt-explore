//! The mock client lets runtime logic be tested deterministically:
//! scripted responses out, recorded requests in.

use prompt_explore::llm::*;

fn request() -> ChatRequest {
    ChatRequest {
        model: "test-model".into(),
        messages: vec![Message::User {
            content: "hello".into(),
        }],
        tools: vec![],
        temperature: None,
        max_tokens: None,
    }
}

#[tokio::test]
async fn mock_replays_script_and_records_requests() {
    let mock = MockLlmClient::scripted(vec![
        ChatResponse {
            content: None,
                thinking: None,
            tool_calls: vec![ToolCallRequest {
                id: "call_1".into(),
                name: "order_status".into(),
                arguments: r#"{"order_id":"A-1234"}"#.into(),
            }],
            usage: None,
        },
        ChatResponse {
            content: Some("Your order is on its way.".into()),
                thinking: None,
            tool_calls: vec![],
            usage: Some(Usage {
                input_tokens: 10,
                cache_read_tokens: 0,
                output_tokens: 5,
            }),
        },
    ]);

    let r1 = mock.complete(request()).await.unwrap();
    assert_eq!(r1.tool_calls.len(), 1);
    assert_eq!(r1.tool_calls[0].name, "order_status");

    let r2 = mock.complete(request()).await.unwrap();
    assert!(r2.content.is_some());

    assert_eq!(mock.requests.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn mock_errors_when_script_exhausted() {
    let mock = MockLlmClient::scripted(vec![]);
    assert!(mock.complete(request()).await.is_err());
}
