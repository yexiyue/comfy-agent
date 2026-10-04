//! Stream one model decision, bounding callbacks and preserving usage semantics.
use super::{ExecutionService, ExecutionTiming, event};
use crate::model::{Attempt, Run, RunStatus};
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
        let (sender, mut receiver) = mpsc::channel::<String>(128);
        let overflow = Arc::new(AtomicBool::new(false));
        let failed = overflow.clone();
        let mut on_text = move |text| {
            if sender.try_send(text).is_err() {
                failed.store(true, Ordering::Relaxed);
            }
        };
        let request = run
            .checkpoint
            .history
            .clone()
            .with_tools(self.registry.definitions());
        let model = run.model.clone();
        let future = self
            .model
            .response(&model, request, &mut on_text)
            .instrument(step_span.clone());
        tokio::pin!(future);
        let mut text_started = false;
        let text_id = format!("{}-step-{step}", run.assistant_id);
        let response = loop {
            tokio::select! {
                biased;
                Some(text)=receiver.recv()=>{
                    timing.record_text(&text);
                    self.text(&run,&text_id,&mut text_started,text).await?;
                },
                result=&mut future=>break result,
            }
            ensure!(
                !overflow.load(Ordering::Relaxed),
                "persistence event queue exceeded"
            );
        };
        while let Ok(text) = receiver.try_recv() {
            timing.record_text(&text);
            self.text(&run, &text_id, &mut text_started, text).await?;
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
                let mut events = vec![];
                if text_started {
                    events.push(event(&run, json!({"type":"text-end","id":text_id}), true));
                }
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
        let mut events = vec![];
        if text_started {
            events.push(event(&run, json!({"type":"text-end","id":text_id}), false));
        }
        if let Some(decision) = &run.checkpoint.decision {
            for call in decision.tool_calls() {
                events.push(event(&run,json!({"type":"tool-input-available","toolCallId":call.call_id,"toolName":call.fn_name,"input":call.fn_arguments}),false));
            }
        }
        run = self.commit(run, attempt, events, vec![]).await?;
        Ok(Some(run))
    }
}
