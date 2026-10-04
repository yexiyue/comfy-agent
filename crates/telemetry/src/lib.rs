//! Shared span semantics. Exporter initialization is exclusively the host's job.
use opentelemetry::{
    KeyValue,
    trace::{Status, TracerProvider},
};
use opentelemetry_otlp::{WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::trace::{SpanData, SpanExporter, SpanProcessor};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use std::{sync::Arc, time::Duration};
use tracing_opentelemetry::OpenTelemetrySpanExt;
use tracing_subscriber::{EnvFilter, prelude::*};

/// Preserve all span parents, but discard dependency debug/trace events.
pub fn trace_metadata(metadata: &tracing::Metadata<'_>) -> bool {
    metadata.is_span()
        || *metadata.level() <= tracing::Level::WARN
        || ["agent", "runtime", "server", "persistence", "telemetry"]
            .iter()
            .any(|target| {
                metadata.target() == *target
                    || metadata.target().starts_with(&format!("{target}::"))
            })
}

pub fn attribute(span: &tracing::Span, key: &'static str, value: impl Into<opentelemetry::Value>) {
    span.set_attribute(key, value);
    if let Ok(project) = PROJECT.try_with(Clone::clone) {
        span.set_attribute("agent.project", project);
    }
}
pub fn error(span: &tracing::Span, category: &'static str) {
    span.set_status(Status::error(category));
}

#[derive(Clone, Debug)]
pub struct Config {
    pub enabled: bool,
    pub endpoint: String,
    pub service: String,
    pub project: String,
    pub eval_project: String,
    pub content: ContentPolicy,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            enabled: false,
            endpoint: "http://localhost:6006/v1/traces".into(),
            service: "comfy-agent-server".into(),
            project: "comfy-agent-local".into(),
            eval_project: "comfy-agent-evals".into(),
            content: ContentPolicy::default(),
        }
    }
}
impl Config {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var(key).ok())
    }
    pub fn from_lookup(get: impl Fn(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut result = Self::default();
        let boolean = |key| -> anyhow::Result<bool> {
            match get(key).as_deref() {
                None | Some("false") => Ok(false),
                Some("true") => Ok(true),
                _ => anyhow::bail!("{key} must be true or false"),
            }
        };
        result.enabled = boolean("OTEL_ENABLED")?;
        result.content.enabled = boolean("OBS_CAPTURE_CONTENT")?;
        if let Some(v) = get("OTEL_EXPORTER_OTLP_TRACES_ENDPOINT") {
            result.endpoint = v;
        }
        if let Some(v) = get("OTEL_SERVICE_NAME") {
            result.service = v;
        }
        if let Some(v) = get("PHOENIX_PROJECT_NAME") {
            result.project = v;
        }
        if let Some(v) = get("PHOENIX_EVAL_PROJECT_NAME") {
            result.eval_project = v;
        }
        if let Some(v) = get("OBS_CONTENT_MAX_BYTES") {
            result.content.max_bytes = v.parse()?;
        }
        anyhow::ensure!(
            result.content.max_bytes > 0 && result.content.max_bytes <= 1_048_576,
            "OBS_CONTENT_MAX_BYTES must be 1..=1048576"
        );
        anyhow::ensure!(
            result.endpoint.starts_with("http://") || result.endpoint.starts_with("https://"),
            "OTLP endpoint must use HTTP(S)"
        );
        let uri = url::Url::parse(&result.endpoint)?;
        anyhow::ensure!(
            uri.host_str().is_some()
                && uri.username().is_empty()
                && uri.password().is_none()
                && uri.query().is_none()
                && uri.fragment().is_none(),
            "OTLP endpoint must have a host and no credentials, query, or fragment"
        );
        anyhow::ensure!(
            !result.service.is_empty()
                && !result.project.is_empty()
                && !result.eval_project.is_empty(),
            "service and project names must not be empty"
        );
        Ok(result)
    }
}

pub fn init(config: &Config) -> anyhow::Result<Option<SdkTracerProvider>> {
    let provider = if config.enabled {
        let make_exporter = || {
            opentelemetry_otlp::SpanExporter::builder()
                .with_http()
                .with_endpoint(&config.endpoint)
                .with_timeout(Duration::from_secs(2))
                .with_headers(std::collections::HashMap::new())
                .build()
        };
        let exporter = RoutedExporter {
            interactive: make_exporter()?,
            evaluation: make_exporter()?,
            project: config.project.clone(),
            eval_project: config.eval_project.clone(),
        };
        let batch = opentelemetry_sdk::trace::BatchSpanProcessor::builder(exporter)
            .with_batch_config(
                opentelemetry_sdk::trace::BatchConfigBuilder::default()
                    .with_max_queue_size(2048)
                    .with_scheduled_delay(Duration::from_secs(1))
                    .build(),
            )
            .build();
        Some(
            SdkTracerProvider::builder()
                .with_span_processor(OpenInferenceProcessor(batch))
                .with_resource(
                    Resource::builder()
                        .with_attributes([KeyValue::new("service.name", config.service.clone())])
                        .build(),
                )
                .build(),
        )
    } else {
        None
    };
    // The exporter selects OpenInference spans. Retain dependency spans so explicit
    // parents survive when Axum/Apalis poll outside app spans; filter only events.
    let layer = provider
        .as_ref()
        .map(|p| tracing_opentelemetry::layer().with_tracer(p.tracer("comfy-agent")));
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::fmt::layer()
                .with_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into())),
        )
        .with(layer)
        .with(tracing_subscriber::filter::filter_fn(trace_metadata))
        .try_init()?;
    Ok(provider)
}

#[derive(Debug)]
pub struct OpenInferenceProcessor<P>(pub P);

impl<P: SpanProcessor> SpanProcessor for OpenInferenceProcessor<P> {
    fn on_start(&self, span: &mut opentelemetry_sdk::trace::Span, cx: &opentelemetry::Context) {
        self.0.on_start(span, cx);
    }
    fn on_end(&self, span: SpanData) {
        // Dependency spans retain context locally, but cannot consume the bounded
        // export queue reserved for the application's OpenInference span tree.
        if span
            .attributes
            .iter()
            .any(|a| a.key.as_str() == "openinference.span.kind")
        {
            self.0.on_end(span);
        }
    }
    fn force_flush(&self) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.force_flush()
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        self.0.shutdown_with_timeout(timeout)
    }
    fn set_resource(&mut self, resource: &Resource) {
        self.0.set_resource(resource);
    }
}

#[derive(Debug)]
struct RoutedExporter {
    interactive: opentelemetry_otlp::SpanExporter,
    evaluation: opentelemetry_otlp::SpanExporter,
    project: String,
    eval_project: String,
}
impl SpanExporter for RoutedExporter {
    async fn export(
        &self,
        batch: Vec<opentelemetry_sdk::trace::SpanData>,
    ) -> opentelemetry_sdk::error::OTelSdkResult {
        let (eval, interactive): (Vec<_>, Vec<_>) = batch
            .into_iter()
            .filter(|s| {
                s.attributes
                    .iter()
                    .any(|a| a.key.as_str() == "openinference.span.kind")
            })
            .partition(|s| {
                s.attributes.iter().any(|a| {
                    a.key.as_str() == "agent.project" && a.value.as_str() == self.eval_project
                })
            });
        let mut result = Ok(());
        for (exporter, batch) in [(&self.interactive, interactive), (&self.evaluation, eval)] {
            if !batch.is_empty()
                && let Err(e) = exporter.export(batch).await
            {
                tracing::warn!("OTLP export failed; completed spans dropped");
                result = Err(e);
            }
        }
        result
    }
    fn set_resource(&mut self, resource: &Resource) {
        for (exporter, project) in [
            (&mut self.interactive, &self.project),
            (&mut self.evaluation, &self.eval_project),
        ] {
            let attributes = resource
                .iter()
                .map(|(k, v)| KeyValue::new(k.clone(), v.clone()))
                .chain([KeyValue::new("openinference.project.name", project.clone())]);
            exporter.set_resource(&Resource::builder().with_attributes(attributes).build());
        }
    }
    fn shutdown_with_timeout(&self, timeout: Duration) -> opentelemetry_sdk::error::OTelSdkResult {
        let a = self.interactive.shutdown_with_timeout(timeout / 2);
        let b = self.evaluation.shutdown_with_timeout(timeout / 2);
        a.and(b)
    }
}

pub async fn shutdown(provider: Option<SdkTracerProvider>) {
    if let Some(provider) = provider {
        let task = tokio::task::spawn_blocking(move || {
            provider.shutdown_with_timeout(Duration::from_secs(4))
        });
        if !matches!(
            tokio::time::timeout(Duration::from_secs(5), task).await,
            Ok(Ok(Ok(())))
        ) {
            tracing::warn!("telemetry shutdown timed out or failed");
        }
    }
}

#[derive(Clone, Debug)]
pub struct ContentPolicy {
    pub enabled: bool,
    pub max_bytes: usize,
}
impl Default for ContentPolicy {
    fn default() -> Self {
        Self {
            enabled: false,
            max_bytes: 16384,
        }
    }
}
impl ContentPolicy {
    pub fn sanitize(&self, value: &serde_json::Value) -> Option<(String, usize, bool)> {
        if !self.enabled {
            return None;
        }
        fn clean(value: &serde_json::Value) -> serde_json::Value {
            match value {
                serde_json::Value::Object(map) => serde_json::Value::Object(
                    map.iter()
                        .map(|(key, value)| {
                            let lower = key.to_lowercase();
                            let sensitive = [
                                "key",
                                "token",
                                "password",
                                "secret",
                                "authorization",
                                "cookie",
                                "base64",
                                "binary",
                            ]
                            .iter()
                            .any(|pattern| lower.contains(pattern));
                            (
                                key.clone(),
                                if sensitive {
                                    serde_json::json!("[REDACTED]")
                                } else {
                                    clean(value)
                                },
                            )
                        })
                        .collect(),
                ),
                serde_json::Value::Array(items) => items.iter().map(clean).collect(),
                serde_json::Value::String(text) => {
                    let pattern = regex::Regex::new(r"(?i)(?:bearer\s+\S+|sk-[a-z0-9_-]+|data:[^\s]+;base64,[a-z0-9+/=]+|(?:api[_-]?key|password|secret|token)\s*[:=]\s*[^\s,;]+)").expect("static regex");
                    serde_json::json!(pattern.replace_all(text, "[REDACTED]"))
                }
                _ => value.clone(),
            }
        }
        let raw_bytes = value.to_string().len();
        let mut text = clean(value).to_string();
        let truncated = text.len() > self.max_bytes;
        if truncated {
            // Keep a valid JSON envelope even when the original structured content is cut.
            let envelope = |end: usize| {
                serde_json::json!({"truncated":true,"preview":&text[..end]}).to_string()
            };
            let mut bounded = if self.max_bytes >= 4 { "null" } else { "0" }.to_owned();
            let (mut low, mut high) = (0, text.len().min(self.max_bytes));
            while low <= high {
                let mid = low + (high - low) / 2;
                let mut end = mid;
                while !text.is_char_boundary(end) {
                    end -= 1;
                }
                let candidate = envelope(end);
                if candidate.len() <= self.max_bytes {
                    bounded = candidate;
                    low = mid + 1;
                } else {
                    if mid == 0 {
                        break;
                    }
                    high = mid - 1;
                }
            }
            text = bounded;
        }
        Some((text, raw_bytes, truncated))
    }
    pub fn record(&self, span: &tracing::Span, direction: &'static str, value: &serde_json::Value) {
        if let Some((text, bytes, truncated)) = self.sanitize(value) {
            let (key, size, cut) = if direction == "input" {
                (
                    "input.value",
                    "agent.input.original_bytes",
                    "agent.input.truncated",
                )
            } else {
                (
                    "output.value",
                    "agent.output.original_bytes",
                    "agent.output.truncated",
                )
            };
            attribute(span, key, text);
            attribute(span, size, bytes as i64);
            attribute(span, cut, truncated);
        }
    }
}

tokio::task_local! { pub static CONTENT_POLICY: Arc<ContentPolicy>; }
tokio::task_local! { pub static PROJECT: String; }
pub fn content_policy() -> Arc<ContentPolicy> {
    CONTENT_POLICY.try_with(Clone::clone).unwrap_or_default()
}

pub struct RunGuard {
    span: tracing::Span,
    done: bool,
}
impl RunGuard {
    pub fn new(span: tracing::Span) -> Self {
        Self { span, done: false }
    }
    pub fn finish(&mut self, outcome: &'static str) {
        if self.done {
            return;
        }
        self.done = true;
        attribute(&self.span, "agent.outcome", outcome);
        if matches!(
            outcome,
            "model-error" | "cancelled" | "shutdown" | "queue-overflow" | "internal-error"
        ) {
            attribute(&self.span, "agent.tokens.complete", false);
        }
        if matches!(
            outcome,
            "model-error" | "queue-overflow" | "internal-error" | "step-limit"
        ) {
            error(&self.span, outcome);
        }
        if outcome == "finished" {
            self.span.set_status(Status::Ok);
        }
        tracing::info!(parent: &self.span, outcome, "agent execution ended");
    }
}
impl Drop for RunGuard {
    fn drop(&mut self) {
        self.finish("internal-error");
    }
}
