//! Execute or reconcile one saved tool call, then commit its result.
use super::{ExecutionService, event};
use crate::model::{Attempt, Run, RunStatus, ToolExecution};
use anyhow::Result;
use genai::chat::ToolCall;
use serde_json::json;
use std::sync::Arc;
use tracing::Instrument;

impl ExecutionService {
    pub(super) async fn tool_step(
        &self,
        mut run: Run,
        attempt: &mut Attempt,
        call: ToolCall,
        step_span: tracing::Span,
    ) -> Result<Option<Run>> {
        let resumed = run.checkpoint.tool_inflight;
        let policy = self.registry.recovery_policy(&call.fn_name);
        let previous = self.store.tool(&run.id, &call.call_id).await?;
        let reconcile = resumed && policy == tools::RecoveryPolicy::Reconcilable;
        if resumed
            && (policy == tools::RecoveryPolicy::Conservative
                || (reconcile
                    && previous
                        .as_ref()
                        .and_then(|tool| tool.external_id.as_ref())
                        .is_none()))
        {
            run.status = RunStatus::NeedsAttention;
            run.error = Some("external operation outcome needs reconciliation".into());
            attempt.outcome = Some("needs-attention".into());
            self.commit(run, attempt, vec![], vec![]).await?;
            return Ok(None);
        }
        run.checkpoint.begin_tool()?;
        attempt.tool_calls += 1;
        let mut tool = previous.unwrap_or_else(|| ToolExecution {
            call_id: call.call_id.clone(),
            step: run.checkpoint.step,
            name: call.fn_name.clone(),
            arguments: call.fn_arguments.clone(),
            recovery: policy.as_str().into(),
            operation_key: format!("{}/{}", run.id, call.call_id),
            external_id: None,
            output: None,
            is_error: false,
        });
        run = self
            .commit(run, attempt, vec![], vec![tool.clone()])
            .await?;
        let store = self.store.clone();
        let id = run.id.clone();
        let call_id = call.call_id.clone();
        let generation = run.generation;
        let context = tools::ExecutionContext::new(
            tool.operation_key.clone(),
            tool.external_id.clone(),
            Arc::new(move |external_id| {
                let store = store.clone();
                let id = id.clone();
                let call_id = call_id.clone();
                Box::pin(async move {
                    store
                        .record_external(&id, generation, &call_id, external_id)
                        .await?;
                    Ok(())
                })
            }),
        );
        let tool_result = {
            let span = tracing::info_span!(parent:&step_span,"tool",tool_name=%call.fn_name);
            telemetry::attribute(&span, "openinference.span.kind", "TOOL");
            telemetry::attribute(&span, "tool.name", call.fn_name.clone());
            telemetry::content_policy().record(&span, "input", &call.fn_arguments);
            let result = self
                .registry
                .execute_with_context(&call, &context, reconcile)
                .instrument(span.clone())
                .await;
            if let Ok(value) = &result {
                telemetry::content_policy().record(&span, "output", value);
            } else {
                telemetry::error(&span, "tool-error");
            }
            result
        };
        let (value, is_error) = match tool_result {
            Ok(value) => (value, false),
            Err(error) if reconcile => {
                tracing::error!(run_id=%run.id,%error,"external operation reconciliation failed");
                run.status = RunStatus::NeedsAttention;
                run.error = Some("external operation outcome needs reconciliation".into());
                attempt.outcome = Some("needs-attention".into());
                self.commit(run, attempt, vec![], vec![]).await?;
                return Ok(None);
            }
            Err(error) => (json!({"error":error.to_string()}), true),
        };
        run.checkpoint
            .tool_completed(&call.call_id, value.clone())?;
        if let Some(updated) = self.store.tool(&run.id, &call.call_id).await? {
            tool.external_id = updated.external_id;
        }
        tool.output = Some(value.clone());
        tool.is_error = is_error;
        let payload = if is_error {
            json!({"type":"tool-output-error","toolCallId":call.call_id,"errorText":value["error"]})
        } else {
            json!({"type":"tool-output-available","toolCallId":call.call_id,"output":value})
        };
        let events = vec![event(&run, payload, false)];
        run = self.commit(run, attempt, events, vec![tool]).await?;
        Ok(Some(run))
    }
}
