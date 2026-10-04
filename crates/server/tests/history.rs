use serde_json::json;
use server::protocol::ChatInput;
#[test]
fn invalid_history_rejects_duplicate_calls_and_unknown_parts() {
    for part in [
        json!({"type":"reasoning","text":"hidden"}),
        json!({"type":"tool-add","state":"output-available","toolCallId":"x","input":{}}),
    ] {
        let body = json!({"messages":[{"id":"a","role":"assistant","parts":[part]},{"id":"u","role":"user","parts":[{"type":"text","text":"next"}]}]});
        assert!(
            serde_json::from_value::<ChatInput>(body)
                .unwrap()
                .into_history()
                .is_err()
        );
    }
    let tool = json!({"type":"tool-add","state":"output-available","toolCallId":"duplicate","input":{},"output":{}});
    let body = json!({"messages":[{"id":"a","role":"assistant","parts":[tool.clone(),tool]},{"id":"u","role":"user","parts":[{"type":"text","text":"next"}]}]});
    assert!(
        serde_json::from_value::<ChatInput>(body)
            .unwrap()
            .into_history()
            .unwrap_err()
            .to_string()
            .contains("unique")
    );
}
