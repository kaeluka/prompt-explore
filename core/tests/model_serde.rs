//! The model layer is the user-facing input format: a prompt under
//! test + investigation must deserialize from plain JSON. These tests
//! pin that contract.

use prompt_explore::model::*;
use serde_json::json;

#[test]
fn put_roundtrip() {
    let json = json!({
        "id": "support_agent",
        "template": "You are a support agent. Tier: {{customer_tier}}.",
        "tools": [{
            "name": "cancel_order",
            "description": "Cancel an order.",
            "parameters": { "type": "object",
                "properties": { "order_id": { "type": "string" } } },
            "side_effect": "write",
            "example_responses": []
        }],
        "design_goals": "false positives acceptable, false negatives not; be concise"
    });

    let put: PromptUnderTest = serde_json::from_value(json).unwrap();
    assert_eq!(put.id, "support_agent");
    assert!(put.template.contains("{{customer_tier}}"));
    assert!(matches!(put.tools[0].side_effect, SideEffect::Write));

    // Round-trip: serialize back without data loss.
    let back = serde_json::to_value(&put).unwrap();
    assert_eq!(back["id"], "support_agent");
}

#[test]
fn scenario_deserializes() {
    // A scenario is a pure value: world + input_domain + opening turn.
    // No id, no resolved_inputs (the simulator picks those).
    let json = json!({
        "world": "One order ORD-2002, owner C-502 (a different customer).",
        "input_domain": {
            "customer_tier": "standard or premium; premium cancels without a fee"
        },
        "user_message": "Cancel ORD-2002, it's mine."
    });
    let s: Scenario = serde_json::from_value(json).unwrap();
    assert_eq!(s.world, "One order ORD-2002, owner C-502 (a different customer).");
    assert_eq!(s.input_domain["customer_tier"], "standard or premium; premium cancels without a fee");
    assert_eq!(s.user_message.as_deref(), Some("Cancel ORD-2002, it's mine."));
}

#[test]
fn investigation_deserializes() {
    let json = json!({
        "reason": "baseline before adding the explicit-confirmation rule",
        "budget": { "max_steps_per_trace": 10, "max_tokens": null }
    });
    let inv: Investigation = serde_json::from_value(json).unwrap();
    assert_eq!(inv.budget.max_steps_per_trace, 10);
    assert_eq!(inv.reason.as_deref(), Some("baseline before adding the explicit-confirmation rule"));
}

#[test]
fn investigation_reason_is_optional() {
    // The reason is advisory framing for the caller, not an oracle —
    // it may be omitted entirely (just observe behavior).
    let json = json!({
        "budget": { "max_steps_per_trace": 6, "max_tokens": null }
    });
    let inv: Investigation = serde_json::from_value(json).unwrap();
    assert!(inv.reason.is_none());
    assert_eq!(inv.budget.max_steps_per_trace, 6);

    // And it round-trips without the key (skip_serializing_if = None).
    let back = serde_json::to_value(&inv).unwrap();
    assert!(back.get("reason").is_none());
}
