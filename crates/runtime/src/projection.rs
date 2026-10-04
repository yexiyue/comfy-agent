//! Presentation is reconstructed from valid durable events; model context stays in checkpoints.
use crate::model::{ProgressEvent, Run, RunStatus, UiMessage};
use serde_json::{Value, json};

pub fn assistant(run: &Run, events: &[ProgressEvent]) -> Option<UiMessage> {
    let mut parts: Vec<Value> = vec![];
    let mut text_blocks = std::collections::HashMap::<String, usize>::new();
    for event in events {
        let p = &event.payload;
        match p["type"].as_str().unwrap_or_default() {
            "start-step"=>parts.push(json!({"type":"step-start"})),
            "text-start"=>{
                if let Some(id)=p["id"].as_str() { text_blocks.insert(id.into(),parts.len());parts.push(json!({"type":"text","text":"","state":"streaming"})); }
            },
            "text-delta"=>{
                if let Some(index)=p["id"].as_str().and_then(|id|text_blocks.get(id)).copied() {
                    let text=parts[index]["text"].as_str().unwrap_or_default().to_owned()+p["delta"].as_str().unwrap_or_default();parts[index]["text"]=json!(text);
                }
            },
            "text-end"=>{ if let Some(index)=p["id"].as_str().and_then(|id|text_blocks.get(id)).copied() { parts[index]["state"]=json!("done"); } },
            "tool-input-available"=>parts.push(json!({"type":format!("tool-{}",p["toolName"].as_str().unwrap_or_default()),"toolCallId":p["toolCallId"],"input":p["input"],"state":"input-available"})),
            "tool-output-available"|"tool-output-error"=>{
                if let Some(part)=parts.iter_mut().find(|part|part["toolCallId"]==p["toolCallId"]) {
                    if p["type"]=="tool-output-error" { part["state"]=json!("output-error");part["errorText"]=p["errorText"].clone(); }
                    else { part["state"]=json!("output-available");part["output"]=p["output"].clone(); }
                }
            },
            _=>{},
        }
    }
    if parts.is_empty() {
        return None;
    }
    if matches!(
        run.status,
        RunStatus::Cancelled | RunStatus::Superseded | RunStatus::Failed
    ) {
        for part in &mut parts {
            if part["state"] == "input-available" {
                part["state"] = json!("output-error");
                let inflight = run.checkpoint.tool_inflight
                    && run
                        .checkpoint
                        .decision
                        .as_ref()
                        .and_then(|decision| {
                            decision
                                .tool_calls()
                                .get(run.checkpoint.next_tool)
                                .map(|call| call.call_id.clone())
                        })
                        .is_some_and(|id| part["toolCallId"] == id);
                part["errorText"] = json!(if inflight {
                    "operation interrupted; external outcome is unknown"
                } else {
                    "not executed: interrupted task"
                });
            }
            if part["state"] == "streaming" {
                part["state"] = json!("done");
            }
        }
    }
    let mut metadata = json!({"runId":run.id,"conversationId":run.conversation_id,"attemptId":run.attempt_id,"status":run.status,"steps":run.checkpoint.step,"draft":events.iter().any(|event|event.draft)});
    for event in events {
        if event.payload["type"] == "finish"
            && let Some(values) = event.payload["messageMetadata"].as_object()
        {
            metadata.as_object_mut().unwrap().extend(values.clone());
        }
    }
    Some(UiMessage {
        id: run.assistant_id.clone(),
        role: "assistant".into(),
        parts,
        metadata: Some(metadata),
    })
}
