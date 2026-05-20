use anyhow::Result;
use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource,
    trace::{SdkTracer, SdkTracerProvider},
};
use tracing::Subscriber;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    filter::{Filtered, LevelFilter},
    layer::Layer as _,
    registry::LookupSpan,
    reload,
};
use upload_symbols::OpenTelemetryConfig;

type InternalLayer<S> = OpenTelemetryLayer<S, SdkTracer>;
type Filter<S> = reload::Layer<LevelFilter, S>;
pub type Layer<S> = Filtered<reload::Layer<Option<InternalLayer<S>>, S>, Filter<S>, S>;

pub struct Coordinator<S> {
    layer_handle: reload::Handle<Option<InternalLayer<S>>, S>,
    filter_handle: reload::Handle<LevelFilter, S>,
    tracer_provider: Option<SdkTracerProvider>,
}

impl<S> Coordinator<S>
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    pub fn new() -> (Self, Layer<S>) {
        let (layer, layer_handle) = reload::Layer::new(None::<InternalLayer<S>>);
        let (filter, filter_handle) = reload::Layer::new(LevelFilter::OFF);
        let manager = Self {
            layer_handle,
            filter_handle,
            tracer_provider: None,
        };
        (manager, layer.with_filter(filter))
    }
}

pub trait OTelCoordinator {
    fn set_up_otlp(&mut self, config: &OpenTelemetryConfig) -> Result<()>;
    fn shutdown(&self) -> Result<()>;
}

fn build_endpoint(base: &str, path: &str) -> String {
    format!("{}{path}", base.trim_end_matches('/'))
}

impl<S> OTelCoordinator for Coordinator<S>
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    fn set_up_otlp(&mut self, config: &OpenTelemetryConfig) -> Result<()> {
        let resource = Resource::builder()
            .with_service_name("upload-symbols")
            .with_attributes(
                config
                    .resource_attributes
                    .iter()
                    .map(|(k, v)| KeyValue::new(k.clone(), v.clone())),
            )
            .build();
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
        let tracer = tracer_provider.tracer("upload-symbols");
        self.tracer_provider = Some(tracer_provider);
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);
        self.filter_handle
            .reload(LevelFilter::from_level(config.log_level))?;
        self.layer_handle.reload(Some(layer))?;
        Ok(())
    }

    fn shutdown(&self) -> Result<()> {
        if let Some(ref provider) = self.tracer_provider {
            provider.shutdown()?;
        }
        Ok(())
    }
}
