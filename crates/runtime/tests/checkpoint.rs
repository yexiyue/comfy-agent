use genai::chat::{ChatMessage, ChatRequest, ContentPart, MessageContent, ToolCall, ToolResponse};
use runtime::model::{Checkpoint, RunStatus};
use serde_json::json;

#[test]
fn checkpoint_roundtrip_preserves_model_and_tool_content() {
    let decision = MessageContent::from_parts(vec![
        ContentPart::Text("first step".into()),
        ContentPart::ToolCall(ToolCall {
            call_id: "call-original".into(),
            fn_name: "add".into(),
            fn_arguments: json!({"a": 1, "b": 2}),
            thought_signatures: Some(vec!["provider-signature".into()]),
        }),
    ]);
    let history = ChatRequest::new(vec![
        ChatMessage::user("calculate"),
        ChatMessage::assistant(decision.clone()),
        ChatMessage::from(ToolResponse::new(
            "call-original",
            "{\"error\":\"overflow\"}",
        )),
        ChatMessage::assistant("second step with recovered tool error"),
    ]);
    let mut checkpoint = Checkpoint::new(history, 6).unwrap();
    checkpoint.step = 1;
    checkpoint.decision = Some(decision);
    checkpoint.next_tool = 1;
    let encoded = serde_json::to_value(&checkpoint).unwrap();
    let restored = Checkpoint::decode(encoded.clone()).unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), encoded);
}

#[test]
fn incomplete_model_segment_is_a_draft_not_history() {
    let mut checkpoint = Checkpoint::new(ChatRequest::from_user("hello"), 2).unwrap();
    checkpoint.step = 1;
    checkpoint.model_inflight = true;
    let encoded = serde_json::to_value(&checkpoint).unwrap();
    let restored = Checkpoint::decode(encoded).unwrap();
    assert!(restored.model_inflight);
    assert!(restored.decision.is_none());
    assert_eq!(restored.history.messages.len(), 1);
}

#[test]
fn checkpoint_rejects_incompatible_versions_and_invalid_bounds() {
    let checkpoint = Checkpoint::new(ChatRequest::from_user("hello"), 2).unwrap();
    let mut encoded = serde_json::to_value(&checkpoint).unwrap();
    encoded["codec_version"] = json!(99);
    assert!(Checkpoint::decode(encoded).is_err());
    let mut encoded = serde_json::to_value(checkpoint).unwrap();
    encoded["next_tool"] = json!(1);
    assert!(Checkpoint::decode(encoded).is_err());
}

#[test]
fn paused_and_attention_states_are_nonterminal() {
    assert!(!RunStatus::Paused.is_terminal());
    assert!(!RunStatus::NeedsAttention.is_terminal());
    assert!(RunStatus::Cancelled.is_terminal());
    assert!(RunStatus::Superseded.is_terminal());
}
