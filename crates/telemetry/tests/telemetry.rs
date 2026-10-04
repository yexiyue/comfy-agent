use opentelemetry::trace::Span;
use opentelemetry::trace::TracerProvider;
use opentelemetry_sdk::trace::{InMemorySpanExporter, SdkTracerProvider};
use serde_json::json;
use std::future::IntoFuture;
use telemetry::{Config, ContentPolicy, RunGuard};
use tracing_subscriber::prelude::*;

#[test]
fn defaults_and_invalid_configuration() {
    let default = Config::from_lookup(|_| None).unwrap();
    assert!(!default.enabled && !default.content.enabled);
    for (key, value) in [
        ("OTEL_ENABLED", "yes"),
        ("OBS_CONTENT_MAX_BYTES", "0"),
        (
            "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT",
            "http://user:secret@localhost/",
        ),
    ] {
        assert!(Config::from_lookup(|k| (k == key).then(|| value.to_owned())).is_err());
    }
}

#[test]
fn content_is_opt_in_redacted_and_utf8_bounded() {
    let input = json!({"apiKey":"private", "nested":{"authorization":"Bearer secret","binary":"AAAA"},"text":"sk-secret password=hidden data:image/png;base64,AAAA 你好你好"});
    assert!(ContentPolicy::default().sanitize(&input).is_none());
    let enabled = ContentPolicy {
        enabled: true,
        max_bytes: 1000,
    };
    let (text, bytes, truncated) = enabled.sanitize(&input).unwrap();
    assert!(bytes > 0 && !truncated);
    for secret in ["private", "hidden", "sk-secret", "AAAA", "Bearer secret"] {
        assert!(!text.contains(secret));
    }
    let (text, _, truncated) = ContentPolicy {
        enabled: true,
        max_bytes: 5,
    }
    .sanitize(&json!("你好你好"))
    .unwrap();
    assert!(truncated && text.len() <= 5);
}

#[test]
fn terminal_guard_records_once_and_falls_back_on_drop() {
    let exporter = InMemorySpanExporter::default();
    let provider = SdkTracerProvider::builder()
        .with_simple_exporter(exporter.clone())
        .build();
    let subscriber = tracing_subscriber::registry()
        .with(tracing_opentelemetry::layer().with_tracer(provider.tracer("test")));
    tracing::subscriber::with_default(subscriber, || {
        for outcome in [
            "finished",
            "step-limit",
            "model-error",
            "cancelled",
            "shutdown",
            "queue-overflow",
            "internal-error",
        ] {
            let span = tracing::info_span!("run");
            let mut guard = RunGuard::new(span);
            guard.finish(outcome);
            guard.finish("finished");
        }
        let _guard = RunGuard::new(tracing::info_span!("dropped"));
    });
    provider.force_flush().unwrap();
    let spans = exporter.get_finished_spans().unwrap();
    assert_eq!(spans.len(), 8);
    for span in spans {
        assert_eq!(span.events.len(), 1);
        assert!(
            span.attributes
                .iter()
                .any(|a| a.key.as_str() == "agent.outcome")
        );
    }
}

#[tokio::test]
async fn otlp_routes_projects_and_reports_unavailable_export() {
    use axum::{Router, body::Bytes, extract::State, routing::post};
    use opentelemetry_otlp::WithExportConfig;
    use opentelemetry_sdk::{Resource, trace::SpanExporter};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    let calls = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/v1/traces", listener.local_addr().unwrap());
    let server = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route(
                    "/v1/traces",
                    post(
                        |State(calls): State<Arc<AtomicUsize>>, body: Bytes| async move {
                            assert!(!body.is_empty());
                            calls.fetch_add(1, Ordering::Relaxed);
                            axum::http::StatusCode::OK
                        },
                    ),
                )
                .with_state(calls.clone()),
        )
        .into_future(),
    );
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .with_timeout(std::time::Duration::from_secs(1))
        .build()
        .unwrap();
    let memory = InMemorySpanExporter::default();
    let p = SdkTracerProvider::builder()
        .with_simple_exporter(memory.clone())
        .build();
    use opentelemetry::trace::Tracer;
    p.tracer("test").start("request").end();
    let batch = memory.get_finished_spans().unwrap();
    let mut exporter = exporter;
    exporter.set_resource(&Resource::builder().with_service_name("test").build());
    // Blocking HTTP client is exported on a blocking task, as production batch does.
    let batch2 = batch.clone();
    tokio::task::spawn_blocking(move || futures_executor(exporter, batch2))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    server.abort();
    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint("http://127.0.0.1:1/v1/traces")
        .with_timeout(std::time::Duration::from_millis(200))
        .build()
        .unwrap();
    assert!(
        tokio::task::spawn_blocking(move || futures_executor(exporter, batch))
            .await
            .unwrap()
            .is_err()
    );
}
#[test]
fn bounded_batch_drops_overflow_without_blocking_producer() {
    use opentelemetry::trace::{Span, Tracer};
    use opentelemetry_sdk::trace::{
        BatchConfigBuilder, BatchSpanProcessor, SpanData, SpanExporter,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        },
        time::{Duration, Instant},
    };
    #[derive(Debug)]
    struct Slow(Arc<AtomicUsize>);
    impl SpanExporter for Slow {
        async fn export(&self, batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
            std::thread::sleep(Duration::from_millis(100));
            self.0.fetch_add(batch.len(), Ordering::Relaxed);
            Ok(())
        }
    }
    let exported = Arc::new(AtomicUsize::new(0));
    let processor = BatchSpanProcessor::builder(Slow(exported.clone()))
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_queue_size(8)
                .with_max_export_batch_size(4)
                .build(),
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(processor)
        .build();
    let tracer = provider.tracer("overflow");
    let start = Instant::now();
    for _ in 0..1000 {
        tracer.start("span").end();
    }
    assert!(start.elapsed() < Duration::from_secs(1));
    provider
        .shutdown_with_timeout(Duration::from_secs(2))
        .unwrap();
    assert!(exported.load(Ordering::Relaxed) < 1000);
}

#[tokio::test]
async fn shutdown_is_bounded_when_exporter_is_stuck() {
    use opentelemetry::trace::{Span, Tracer};
    use opentelemetry_sdk::trace::{
        BatchConfigBuilder, BatchSpanProcessor, SpanData, SpanExporter,
    };
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::{Duration, Instant},
    };
    #[derive(Debug)]
    struct Stuck(Arc<AtomicBool>);
    impl SpanExporter for Stuck {
        async fn export(&self, _batch: Vec<SpanData>) -> opentelemetry_sdk::error::OTelSdkResult {
            self.0.store(true, Ordering::Relaxed);
            std::thread::sleep(Duration::from_secs(6));
            Ok(())
        }
    }
    let started = Arc::new(AtomicBool::new(false));
    let batch = BatchSpanProcessor::builder(Stuck(started.clone()))
        .with_batch_config(
            BatchConfigBuilder::default()
                .with_max_export_batch_size(1)
                .build(),
        )
        .build();
    let provider = SdkTracerProvider::builder()
        .with_span_processor(batch)
        .build();
    provider.tracer("shutdown").start("span").end();
    let wait = Instant::now();
    while !started.load(Ordering::Relaxed) {
        assert!(wait.elapsed() < Duration::from_secs(2));
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let wait = Instant::now();
    telemetry::shutdown(Some(provider)).await;
    assert!(wait.elapsed() < Duration::from_secs(5));
}

fn futures_executor(
    exporter: impl opentelemetry_sdk::trace::SpanExporter,
    batch: Vec<opentelemetry_sdk::trace::SpanData>,
) -> opentelemetry_sdk::error::OTelSdkResult {
    // The HTTP exporter future is polled outside the async runtime.
    futures::executor::block_on(exporter.export(batch))
}
