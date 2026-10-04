//! AI SDK UI messages are presentation history; rebuild model/tool exchanges explicitly.
use std::collections::HashSet;

use anyhow::{Context, bail, ensure};
use genai::chat::{ChatMessage, ChatRequest, ContentPart, MessageContent, ToolCall, ToolResponse};
use serde::Deserialize;
use serde_json::{Value, json};

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChatInput {
    pub id: Option<String>,
    pub messages: Vec<UiMessage>,
    pub trigger: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UiMessage {
    pub id: String,
    pub role: String,
    pub parts: Vec<Value>,
    pub metadata: Option<Value>,
}

fn field<'a>(part: &'a Value, key: &str) -> anyhow::Result<&'a str> {
    part.get(key)
        .and_then(Value::as_str)
        .with_context(|| format!("part requires string {key}"))
}

fn flush(
    history: &mut Vec<ChatMessage>,
    parts: &mut Vec<ContentPart>,
    results: &mut Vec<ToolResponse>,
) {
    if !parts.is_empty() {
        history.push(ChatMessage::assistant(MessageContent::from_parts(
            std::mem::take(parts),
        )));
    }
    history.extend(results.drain(..).map(ChatMessage::from));
}

impl ChatInput {
    pub fn into_history(self) -> anyhow::Result<ChatRequest> {
        ensure!(
            self.trigger.as_deref().unwrap_or("submit-message") == "submit-message",
            "only submit-message is supported; regeneration is not supported"
        );
        ensure!(!self.messages.is_empty(), "messages must not be empty");
        ensure!(
            self.messages
                .last()
                .is_some_and(|message| message.role == "user"),
            "last message must be a user message"
        );
        let mut history = Vec::new();
        let mut ids = HashSet::new();
        let mut call_ids = HashSet::new();
        for message in self.messages {
            ensure!(
                !message.id.trim().is_empty() && ids.insert(message.id),
                "message ids must be nonempty and unique"
            );
            ensure!(!message.parts.is_empty(), "message parts must not be empty");
            if message.role == "assistant" {
                let mut parts = Vec::new();
                let mut results = Vec::new();
                for part in message.parts {
                    let kind = field(&part, "type")?;
                    match kind {
                        "step-start" => flush(&mut history, &mut parts, &mut results),
                        "text" => {
                            // Text following tools without a step marker starts another model message.
                            if !results.is_empty() {
                                flush(&mut history, &mut parts, &mut results);
                            }
                            parts.push(ContentPart::Text(field(&part, "text")?.to_owned()));
                        }
                        kind if kind == "dynamic-tool" || kind.starts_with("tool-") => {
                            let name = if kind == "dynamic-tool" {
                                field(&part, "toolName")?
                            } else {
                                &kind[5..]
                            };
                            ensure!(!name.is_empty(), "tool name must not be empty");
                            let id = field(&part, "toolCallId")?;
                            ensure!(
                                !id.is_empty() && call_ids.insert(id.to_owned()),
                                "tool call ids must be nonempty and unique"
                            );
                            let input = part
                                .get("input")
                                .context("completed tool requires input")?
                                .clone();
                            let output = match field(&part, "state")? {
                                "output-available" => part
                                    .get("output")
                                    .context("completed tool requires output")?
                                    .clone(),
                                "output-error" => json!({"error": field(&part, "errorText")?}),
                                _ => bail!("only completed tool exchanges are supported"),
                            };
                            parts.push(ContentPart::ToolCall(ToolCall {
                                call_id: id.to_owned(),
                                fn_name: name.to_owned(),
                                fn_arguments: input,
                                thought_signatures: None,
                            }));
                            results.push(ToolResponse::new(id.to_owned(), output.to_string()));
                        }
                        _ => bail!("unsupported assistant part: {kind}"),
                    }
                }
                flush(&mut history, &mut parts, &mut results);
            } else {
                ensure!(
                    message.role == "user" || message.role == "system",
                    "unsupported message role"
                );
                let mut parts = Vec::new();
                for part in message.parts {
                    ensure!(
                        field(&part, "type")? == "text",
                        "user/system messages support text only"
                    );
                    parts.push(ContentPart::Text(field(&part, "text")?.to_owned()));
                }
                ensure!(
                    parts.iter().any(
                        |part| matches!(part, ContentPart::Text(text) if !text.trim().is_empty())
                    ),
                    "text message must not be blank"
                );
                let content = MessageContent::from_parts(parts);
                history.push(if message.role == "user" {
                    ChatMessage::user(content)
                } else {
                    ChatMessage::system(content)
                });
            }
        }
        Ok(ChatRequest::new(history))
    }
}
