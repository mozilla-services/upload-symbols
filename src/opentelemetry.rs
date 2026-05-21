use anyhow::Result;
use opentelemetry::{KeyValue, trace::TracerProvider};
use opentelemetry_otlp::{Protocol, SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{
    Resource,
    trace::{SdkTracer, SdkTracerProvider},
};
use opentelemetry_semantic_conventions::resource::SERVICE_VERSION;
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

/// Coordinate setting up OpenTelemetry exporters.
///
/// The tracing library is set up at the start of the program, but we only retrieve the
/// [`OpenTelemetryConfig`] during the preflight request to the Symbols Server. For this reason
/// we need to add a placeholder layer to the tracing subscriber during start-up, which we can
/// replace with the actual [`OpenTelemetryLayer`] once we have the configuration. We need to
/// wrap both the OpenTelemetry layer and the [`LevelFilter`] for that layer in a
/// [`reload::Layer`] to make them dynamically configurable.
///
/// The type parameter `S` represents the subscriber type.
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
    /// Create a new [`Coordinator`] instance.
    ///
    /// The constructor also returns the placeholder layer that should be added to the tracing
    /// subscriber.
    pub fn new() -> (Self, Layer<S>) {
        // Create the placeholder layer. The [`tracing_subscriber::layer::Layer`] trait is
        // implemented for `Option<impl Layer>` as well, with `None` representing a no-op
        // layer.
        let (layer, layer_handle) = reload::Layer::new(None::<InternalLayer<S>>);
        // Create a filter that disables the layer (which doesn't do anything anyway).
        let (filter, filter_handle) = reload::Layer::new(LevelFilter::OFF);
        let manager = Self {
            layer_handle,
            filter_handle,
            tracer_provider: None,
        };
        (manager, layer.with_filter(filter))
    }
}

/// Trait to erase the subscriber type parameter from [`Coordinator`].
///
/// We want to be able to return a [`Coordinator`] instance from functions, but it's often hard
/// or impossible to name the subscriber type `S`. Using this trait, we can return `impl
/// OTelCoordinator` instead, so we don't have to explicitly name the subscriber type, without
/// the need for any dynamic dispatch.
pub trait OTelCoordinator {
    fn set_up_otlp(&mut self, config: &OpenTelemetryConfig) -> Result<()>;
    fn shutdown(&self) -> Result<()>;
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

impl<S> OTelCoordinator for Coordinator<S>
where
    S: Subscriber,
    for<'a> S: LookupSpan<'a>,
{
    /// Replace the placeholder layer and filter with the actual instances.
    ///
    /// This function sets up an [`OpenTelementryLayer`] and a [`LevelFilter`] based on the
    /// given configuration and replaces the placeholders with them.
    fn set_up_otlp(&mut self, config: &OpenTelemetryConfig) -> Result<()> {
        let resource = resource(config);
        let tracer_provider = tracer_provider(config, resource.clone())?;
        let tracer = tracer_provider.tracer(env!("CARGO_PKG_NAME"));
        self.tracer_provider = Some(tracer_provider);
        let layer = tracing_opentelemetry::layer().with_tracer(tracer);
        self.filter_handle
            .reload(LevelFilter::from_level(config.log_level))?;
        self.layer_handle.reload(Some(layer))?;
        Ok(())
    }

    /// Shut down all OTel providers to flush all remaining data to the collector.
    fn shutdown(&self) -> Result<()> {
        if let Some(ref provider) = self.tracer_provider {
            provider.shutdown()?;
        }
        Ok(())
    }
}
