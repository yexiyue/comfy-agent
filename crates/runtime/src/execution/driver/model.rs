//! Stream one model decision, bounding callbacks and preserving usage semantics.
use super::{ExecutionService, ExecutionTiming, event};
use crate::{
    execution::ModelDelta,
    model::{Attempt, Run, RunStatus},
};
use anyhow::{Result, ensure};
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use tokio::sync::mpsc;
use tracing::Instrument;

impl ExecutionService {
    pub(super) async fn model_step(
        &self,
        mut run: Run,
        attempt: &mut Attempt,
        step: usize,
        step_span: tracing::Span,
        timing: &mut ExecutionTiming,
    ) -> Result<Option<Run>> {
        run.checkpoint.begin_model()?;
        attempt.model_calls += 1;
        let previous_usage_complete = attempt.usage_complete;
        attempt.usage_complete = false;
        run = self
            .commit(
                run.clone(),
                attempt,
                vec![event(&run, json!({"type":"start-step"}), true)],
                vec![],
            )
            .await?;
        let (sender, mut receiver) = mpsc::channel::<ModelDelta>(128);
        let overflow = Arc::new(AtomicBool::new(false));
        let failed = overflow.clone();
        let mut on_delta = move |delta| {
            if sender.try_send(delta).is_err() {
                failed.store(true, Ordering::Relaxed);
            }
        };
        let request = run
            .checkpoint
            .history
            .clone()
            .with_tools(self.registry.definitions());
        let model = run.model.clone();
        let reasoning_effort = run.reasoning_effort.clone();
        let future = self
            .model
            .response(&model, request, reasoning_effort.as_deref(), &mut on_delta)
            .instrument(step_span.clone());
        tokio::pin!(future);
        let mut blocks = ModelBlocks::new(format!("{}-step-{step}", run.assistant_id));
        let response = loop {
            tokio::select! {
                biased;
                Some(delta)=receiver.recv()=>{
                    self.model_delta(&run, &mut blocks, delta, timing).await?;
                },
                result=&mut future=>break result,
            }
            ensure!(
                !overflow.load(Ordering::Relaxed),
                "persistence event queue exceeded"
            );
        };
        while let Ok(delta) = receiver.try_recv() {
            self.model_delta(&run, &mut blocks, delta, timing).await?;
        }
        ensure!(
            !overflow.load(Ordering::Relaxed),
            "persistence event queue exceeded"
        );
        let response = match response {
            Ok(response) => response,
            Err(error) => {
                tracing::error!(run_id=%run.id,%error,"model request failed");
                run.status = RunStatus::Failed;
                run.error = Some("model request failed".into());
                attempt.outcome = Some("model-error".into());
                attempt.usage_complete = false;
                let mut events = blocks
                    .close()
                    .into_iter()
                    .map(|p| event(&run, p, true))
                    .collect::<Vec<_>>();
                events.push(event(&run, json!({"type":"finish-step"}), true));
                events.push(event(
                    &run,
                    json!({"type":"error","errorText":"Model request failed"}),
                    false,
                ));
                self.commit(run, attempt, events, vec![]).await?;
                return Ok(None);
            }
        };
        if let Some(tokens) = response
            .usage
            .as_ref()
            .and_then(|usage| usage.total_tokens)
            .and_then(|value| u64::try_from(value).ok())
        {
            attempt.known_tokens += tokens;
            attempt.usage_complete = previous_usage_complete;
        }
        run.checkpoint.model_completed(response.content)?;
        let mut events = blocks
            .close()
            .into_iter()
            .map(|p| event(&run, p, false))
            .collect::<Vec<_>>();
        if let Some(decision) = &run.checkpoint.decision {
            for call in decision.tool_calls() {
                events.push(event(&run,json!({"type":"tool-input-available","toolCallId":call.call_id,"toolName":call.fn_name,"input":call.fn_arguments}),false));
            }
        }
        run = self.commit(run, attempt, events, vec![]).await?;
        Ok(Some(run))
    }

    async fn model_delta(
        &self,
        run: &Run,
        blocks: &mut ModelBlocks,
        delta: ModelDelta,
        timing: &mut ExecutionTiming,
    ) -> Result<()> {
        match &delta {
            ModelDelta::Text(text) => timing.record_text(text),
            ModelDelta::Reasoning(text) => timing.record_reasoning(text),
        }
        let events = blocks
            .delta(delta)
            .into_iter()
            .map(|p| event(run, p, true))
            .collect::<Vec<_>>();
        if !events.is_empty() {
            self.store
                .append(&run.id, run.generation, events, self.config.max_event_bytes)
                .await?;
            self.changed.notify_waiters();
        }
        Ok(())
    }
}

/// Close the previous block on each kind transition, preserving stream order.
struct ModelBlocks {
    prefix: String,
    next: usize,
    open: Option<(&'static str, String)>,
}
impl ModelBlocks {
    fn new(prefix: String) -> Self {
        Self {
            prefix,
            next: 0,
            open: None,
        }
    }

    fn delta(&mut self, delta: ModelDelta) -> Vec<serde_json::Value> {
        let (kind, text) = match delta {
            ModelDelta::Text(text) => ("text", text),
            ModelDelta::Reasoning(text) => ("reasoning", text),
        };
        if text.is_empty() {
            return vec![];
        }
        let mut events = vec![];
        if self.open.as_ref().map(|(previous, _)| *previous) != Some(kind) {
            events.extend(self.close());
            let id = format!("{}-block-{}", self.prefix, self.next);
            self.next += 1;
            events.push(json!({"type":format!("{kind}-start"),"id":id}));
            self.open = Some((kind, id));
        }
        let (_, id) = self.open.as_ref().unwrap();
        events.push(json!({"type":format!("{kind}-delta"),"id":id,"delta":text}));
        events
    }

    fn close(&mut self) -> Vec<serde_json::Value> {
        self.open
            .take()
            .map(|(kind, id)| vec![json!({"type":format!("{kind}-end"),"id":id})])
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interleaved_reasoning_and_text_have_distinct_closed_blocks() {
        let mut blocks = ModelBlocks::new("assistant-step-1".into());
        let mut events = blocks.delta(ModelDelta::Reasoning("Think".into()));
        assert!(blocks.delta(ModelDelta::Text(String::new())).is_empty());
        events.extend(blocks.delta(ModelDelta::Text("Answer".into())));
        events.extend(blocks.delta(ModelDelta::Reasoning("Reconsider".into())));
        events.extend(blocks.close());
        assert_eq!(
            events
                .iter()
                .map(|p| p["type"].as_str().unwrap())
                .collect::<Vec<_>>(),
            [
                "reasoning-start",
                "reasoning-delta",
                "reasoning-end",
                "text-start",
                "text-delta",
                "text-end",
                "reasoning-start",
                "reasoning-delta",
                "reasoning-end",
            ]
        );
        assert_ne!(events[0]["id"], events[6]["id"]);
        assert!(blocks.close().is_empty());
    }
}
