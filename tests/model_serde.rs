//! The model layer is the user-facing input format: a PsUT +
//! investigation must deserialize from plain JSON. These tests pin
//! that contract.

use prompt_explore::model::*;
use serde_json::json;

#[test]
fn psut_roundtrip() {
    let json = json!({
        "prompts": [{
            "id": "support_agent",
            "template": "You are a support agent. Tier: {{customer_tier}}. {{complaint_text}}",
            "input_vars": {
                "customer_tier": { "kind": "constant", "value": "gold" },
                "complaint_text": { "kind": "nl_description",
                    "description": "an angry customer whose order is 3 weeks late" }
            },
            "tools": [{
                "name": "cancel_order",
                "description": "Cancel an order.",
                "parameters": { "type": "object",
                    "properties": { "order_id": { "type": "string" } } },
                "side_effect": "write",
                "example_responses": []
            }],
            "design_goals": "false positives acceptable, false negatives not; be concise"
        }],
        "topology": "single prompt",
        "design_goals": null
    });

    let psut: PromptsUnderTest = serde_json::from_value(json.clone()).unwrap();
    let put = &psut.prompts[0];
    assert_eq!(put.id, "support_agent");
    assert!(matches!(
        put.input_vars["customer_tier"],
        VarSpec::Constant { .. }
    ));
    assert!(matches!(
        put.input_vars["complaint_text"],
        VarSpec::NlDescription { .. }
    ));
    assert!(matches!(put.tools[0].side_effect, SideEffect::Write));

    // Round-trip: serialize back without data loss.
    let back = serde_json::to_value(&psut).unwrap();
    assert_eq!(back["prompts"][0]["id"], "support_agent");
}

#[test]
fn investigation_deserializes() {
    let json = json!({
        "question": "are there inputs that cause destructive tool calls?",
        "target_put": "support_agent",
        "budget": { "max_scenarios": 20, "max_steps_per_trace": 10, "max_tokens": null }
    });
    let inv: Investigation = serde_json::from_value(json).unwrap();
    assert_eq!(inv.budget.max_scenarios, 20);
}
