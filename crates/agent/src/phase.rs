//! Transport-neutral resumable state machine shared by all Agent drivers.

use anyhow::{Result, ensure};
use genai::chat::{ChatMessage, ChatRequest, MessageContent, ToolCall, ToolResponse};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Checkpoint {
    pub codec_version: u32,
    pub history: ChatRequest,
    pub step: usize,
    pub max_steps: usize,
    pub model_inflight: bool,
    pub decision: Option<MessageContent>,
    pub next_tool: usize,
    pub tool_inflight: bool,
    pub answer: Option<String>,
}

#[derive(Debug, Clone)]
pub enum Action {
    Model { step: usize },
    Tool(ToolCall),
    StepComplete { step: usize },
    Finished { answer: String, steps: usize },
    StepLimit { steps: usize },
}

impl Checkpoint {
    pub fn new(history: ChatRequest, max_steps: usize) -> Result<Self> {
        ensure!(max_steps > 0, "max_steps must be greater than 0");
        Ok(Self {
            codec_version: 1,
            history,
            step: 0,
            max_steps,
            model_inflight: false,
            decision: None,
            next_tool: 0,
            tool_inflight: false,
            answer: None,
        })
    }

    pub fn decode(value: Value) -> Result<Self> {
        let checkpoint: Self = serde_json::from_value(value)?;
        ensure!(
            checkpoint.codec_version == 1,
            "unsupported checkpoint codec"
        );
        ensure!(
            checkpoint.max_steps > 0 && checkpoint.step <= checkpoint.max_steps,
            "invalid step budget"
        );
        let tools = checkpoint
            .decision
            .as_ref()
            .map_or(0, |content| content.tool_calls().len());
        ensure!(checkpoint.next_tool <= tools, "invalid tool checkpoint");
        ensure!(
            !checkpoint.model_inflight || (checkpoint.step > 0 && checkpoint.decision.is_none()),
            "invalid model phase"
        );
        ensure!(
            !checkpoint.tool_inflight
                || (checkpoint.decision.is_some()
                    && checkpoint.next_tool < tools
                    && !checkpoint.model_inflight),
            "invalid tool phase"
        );
        Ok(checkpoint)
    }

    pub fn next(&self) -> Action {
        if let Some(decision) = &self.decision {
            if let Some(call) = decision.tool_calls().get(self.next_tool) {
                return Action::Tool((*call).clone());
            }
            return Action::StepComplete { step: self.step };
        }
        if let Some(answer) = &self.answer {
            return Action::Finished {
                answer: answer.clone(),
                steps: self.step,
            };
        }
        if self.model_inflight {
            return Action::Model { step: self.step };
        }
        if self.step >= self.max_steps {
            return Action::StepLimit { steps: self.step };
        }
        Action::Model {
            step: self.step + 1,
        }
    }

    pub fn begin_model(&mut self) -> Result<()> {
        ensure!(
            matches!(self.next(), Action::Model { .. }),
            "not a model phase"
        );
        if !self.model_inflight {
            self.step += 1;
        }
        self.model_inflight = true;
        Ok(())
    }

    pub fn model_completed(&mut self, content: MessageContent) -> Result<()> {
        ensure!(
            self.model_inflight && self.decision.is_none(),
            "model phase not started"
        );
        let mut ids: std::collections::HashSet<String> = self
            .history
            .messages
            .iter()
            .flat_map(|message| message.content.tool_calls())
            .map(|call| call.call_id.clone())
            .collect();
        for call in content.tool_calls() {
            ensure!(
                !call.call_id.is_empty()
                    && !call.fn_name.is_empty()
                    && ids.insert(call.call_id.clone()),
                "invalid or duplicate model tool call ID"
            );
        }
        let answer = if content.tool_calls().is_empty() {
            let text = content.texts().join("");
            ensure!(!text.trim().is_empty(), "model returned no answer");
            Some(text)
        } else {
            None
        };
        self.history
            .messages
            .push(ChatMessage::assistant(content.clone()));
        self.decision = Some(content);
        self.answer = answer;
        self.model_inflight = false;
        self.next_tool = 0;
        Ok(())
    }

    pub fn begin_tool(&mut self) -> Result<ToolCall> {
        let Action::Tool(call) = self.next() else {
            anyhow::bail!("not a tool phase");
        };
        self.tool_inflight = true;
        Ok(call)
    }

    pub fn tool_completed(&mut self, call_id: &str, value: Value) -> Result<()> {
        let Action::Tool(call) = self.next() else {
            anyhow::bail!("not a tool phase");
        };
        ensure!(
            self.tool_inflight && call.call_id == call_id,
            "tool result does not match pending call"
        );
        self.history
            .messages
            .push(ChatMessage::from(ToolResponse::new(
                call_id,
                value.to_string(),
            )));
        self.next_tool += 1;
        self.tool_inflight = false;
        Ok(())
    }

    pub fn step_completed(&mut self) -> Result<()> {
        ensure!(
            matches!(self.next(), Action::StepComplete { .. }),
            "step still has pending work"
        );
        self.decision = None;
        self.next_tool = 0;
        Ok(())
    }

    /// Finish unexecuted calls explicitly when a paused task is replaced.
    pub fn superseded_history(&self) -> ChatRequest {
        let mut history = self.history.clone();
        if let Some(decision) = &self.decision {
            for (index, call) in decision
                .tool_calls()
                .into_iter()
                .enumerate()
                .skip(self.next_tool)
            {
                history.messages.push(ChatMessage::from(ToolResponse::new(
                    &call.call_id,
                    if self.tool_inflight && index==self.next_tool {
                        serde_json::json!({"error":"operation interrupted; external outcome is unknown","executed":null})
                    } else { json_error_superseded() }.to_string(),
                )));
            }
        }
        history
    }
}

fn json_error_superseded() -> Value {
    serde_json::json!({"error":"not executed: superseded by additional user instruction","executed":false})
}
