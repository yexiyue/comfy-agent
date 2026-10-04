use agent::{Checkpoint, phase::Action};
use genai::chat::{ChatRequest, ContentPart, MessageContent, ToolCall};
use serde_json::json;

fn call(id: &str) -> ToolCall {
    ToolCall {
        call_id: id.into(),
        fn_name: "add".into(),
        fn_arguments: json!({"a":1,"b":2}),
        thought_signatures: Some(vec!["signature".into()]),
    }
}

#[test]
fn complete_model_and_each_tool_are_separate_checkpoints() {
    let mut state = Checkpoint::new(ChatRequest::from_user("calculate"), 2).unwrap();
    state.begin_model().unwrap();
    state
        .model_completed(MessageContent::from_parts(vec![
            ContentPart::ThoughtSignature("signed-block".into()),
            ContentPart::ReasoningContent("Use both tools.".into()),
            ContentPart::ToolCall(call("first")),
            ContentPart::ToolCall(call("second")),
        ]))
        .unwrap();
    let model = Checkpoint::decode(serde_json::to_value(&state).unwrap()).unwrap();
    assert!(matches!(model.next(),Action::Tool(call) if call.call_id=="first"));
    state.begin_tool().unwrap();
    state.tool_completed("first", json!({"sum":3})).unwrap();
    let mut restored = Checkpoint::decode(serde_json::to_value(&state).unwrap()).unwrap();
    assert!(matches!(restored.next(),Action::Tool(call) if call.call_id=="second"));
    restored.begin_tool().unwrap();
    restored
        .tool_completed("second", json!({"error":"overflow"}))
        .unwrap();
    assert!(matches!(restored.next(), Action::StepComplete { step: 1 }));
    restored.step_completed().unwrap();
    assert_eq!(
        serde_json::to_value(&restored.history.messages[1].content).unwrap(),
        serde_json::to_value(&model.history.messages[1].content).unwrap()
    );
    assert!(matches!(restored.next(), Action::Model { step: 2 }));
    restored.begin_model().unwrap();
    restored
        .model_completed(MessageContent::from_text("answer"))
        .unwrap();
    restored.step_completed().unwrap();
    assert!(matches!(restored.next(), Action::Finished { steps: 2, .. }));
}

#[test]
fn resumed_model_reuses_step_and_never_extends_budget() {
    let mut state = Checkpoint::new(ChatRequest::from_user("hello"), 1).unwrap();
    state.begin_model().unwrap();
    for _ in 0..3 {
        state = Checkpoint::decode(serde_json::to_value(&state).unwrap()).unwrap();
        state.begin_model().unwrap();
        assert_eq!(state.step, 1);
    }
    state
        .model_completed(MessageContent::from_tool_calls(vec![call("first")]))
        .unwrap();
    state.begin_tool().unwrap();
    state.tool_completed("first", json!({"sum":3})).unwrap();
    state.step_completed().unwrap();
    assert!(matches!(state.next(), Action::StepLimit { steps: 1 }));
}

#[test]
fn superseding_closes_pending_calls_without_faking_success() {
    let mut state = Checkpoint::new(ChatRequest::from_user("hello"), 2).unwrap();
    state.begin_model().unwrap();
    state
        .model_completed(MessageContent::from_tool_calls(vec![
            call("first"),
            call("second"),
        ]))
        .unwrap();
    state.begin_tool().unwrap();
    state.tool_completed("first", json!({"sum":3})).unwrap();
    let history = state.superseded_history();
    let encoded = serde_json::to_value(&history).unwrap().to_string();
    assert!(encoded.contains("not executed"));
    assert!(encoded.contains("second"));
    assert_eq!(history.messages.len(), 4);
    assert_eq!(state.history.messages.len(), 3);
}

#[test]
fn ordinary_answer_closes_the_step_before_finishing() {
    let mut state = Checkpoint::new(ChatRequest::from_user("hello"), 2).unwrap();
    state.begin_model().unwrap();
    state
        .model_completed(MessageContent::from_text("hello back"))
        .unwrap();
    assert!(matches!(state.next(), Action::StepComplete { step: 1 }));
    state.step_completed().unwrap();
    assert!(matches!(state.next(),Action::Finished { answer,steps:1 } if answer=="hello back"));
}
