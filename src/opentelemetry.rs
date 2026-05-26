use anyhow::Result;
use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{Resource, trace::SdkTracerProvider};
use opentelemetry_semantic_conventions::resource::SERVICE_VERSION;
use tracing::{Subscriber, level_filters::LevelFilter};
use tracing_subscriber::{filter::Targets, layer::Layer, registry::LookupSpan};
use upload_symbols::OpenTelemetryConfig;

/// Set up OpenTelemetry with the given configuration.
///
/// Returns a guard and a layer that should be added to the tracing subscriber.
pub fn set_up<S>(config: &OpenTelemetryConfig) -> Result<(Guard, impl Layer<S>)>
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    let tracer_provider = tracer_provider(config, resource(config))?;
    let filter = Targets::new()
        .with_default(config.log_level)
        // Prevent telemetry-induced telemetry which might cause infinite telemetry loops at
        // log levels above "info".
        .with_target("hyper", LevelFilter::OFF)
        .with_target("h2", LevelFilter::OFF)
        .with_target("reqwest", LevelFilter::OFF);
    let layer = tracing_opentelemetry::layer()
        .with_tracer(tracer_provider.tracer(env!("CARGO_PKG_NAME")))
        .with_filter(filter);
    let guard = Guard { tracer_provider };
    Ok((guard, layer))
}

/// Guard to hold OTel providers and facilitate ordered shutdown.
pub struct Guard {
    tracer_provider: SdkTracerProvider,
}

impl Guard {
    /// Shut down all OTel providers to flush all remaining data to the collector.
    pub fn shutdown(&self) -> Result<()> {
        self.tracer_provider.shutdown()?;
        Ok(())
    }
}

/// Add a path to the base endpoint URL.
///
/// We get a OpenTelemetry base URL from the Symbols Server. We still need to append paths like
/// `v1/traces` or `v1/metrics` for the individual services.
fn build_endpoint(base_url: &str, path: &str) -> String {
    format!("{}{path}", base_url.trim_end_matches('/'))
}

/// Return the OTel [`Resource`], a representation of the entity producing the telemetry.
///
/// The `Resource` struct uses `Arc` internally, so it can be cheaply cloned.
fn resource(config: &OpenTelemetryConfig) -> Resource {
    Resource::builder()
        .with_service_name(env!("CARGO_PKG_NAME"))
        .with_attribute(KeyValue::new(SERVICE_VERSION, env!("CARGO_PKG_VERSION")))
        .with_attributes(
            config
                .resource_attributes
                .iter()
                .map(|(k, v)| KeyValue::new(k.clone(), v.clone())),
        )
        .build()
}

/// Return the [`SdkTracerProvider`] for OTel tracing support.
fn tracer_provider(config: &OpenTelemetryConfig, resource: Resource) -> Result<SdkTracerProvider> {
    let span_exporter = SpanExporter::builder()
        .with_http()
        .with_protocol(Protocol::HttpBinary)
        .with_endpoint(build_endpoint(&config.endpoint, "/v1/traces"))
        .with_headers(config.headers.clone())
        .build()?;
    let tracer_provider = SdkTracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(span_exporter)
        .build();
    Ok(tracer_provider)
}
