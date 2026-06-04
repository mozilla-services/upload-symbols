use anyhow::Result;
use clap::{
    Parser,
    builder::{Styles, styling::AnsiColor},
};
use std::{env::VarError, path::PathBuf, process::ExitCode};
use tracing_subscriber::{
    EnvFilter,
    fmt::format::FmtSpan,
    layer::{Layer as _, SubscriberExt},
    util::SubscriberInitExt,
};
use upload_symbols::{ClientBuilder, OpenTelemetryConfig};

/// Upload symbols files to the Mozilla Symbols Server.
///
/// All symbols files in the source directory are discovered and uploaded to the Mozilla
/// Symbols Server. You need an authentication token with upload permissions for the server you
/// are uploading to and store it in the `SYMBOLS_AUTH_TOKEN` environment variable.
#[derive(Debug, Parser)]
#[command(styles = CLAP_STYLES)]
struct Args {
    /// The directory containting the symbols files to be uploaded.
    #[arg(required = true, value_name = "DIRECTORY")]
    directory: PathBuf,

    #[command(flatten)]
    client_builder: ClientBuilder,
}

const CLAP_STYLES: Styles = Styles::styled()
    .header(AnsiColor::BrightGreen.on_default().bold())
    .usage(AnsiColor::BrightGreen.on_default().bold())
    .literal(AnsiColor::BrightCyan.on_default().bold())
    .placeholder(AnsiColor::Cyan.on_default())
    .error(AnsiColor::BrightRed.on_default().bold())
    .valid(AnsiColor::BrightCyan.on_default().bold())
    .invalid(AnsiColor::Yellow.on_default().bold());

fn main() -> Result<ExitCode> {
    let _guard = setup_sentry();
    let args = Args::parse();
    upload_directory(args)
}

fn setup_sentry() -> Result<Option<sentry::ClientInitGuard>> {
    let dsn = match std::env::var("SENTRY_DSN") {
        Ok(dsn) => Some(dsn),
        Err(VarError::NotPresent) => option_env!("SENTRY_DSN").map(String::from),
        Err(err @ VarError::NotUnicode(_)) => Err(err)?,
    };
    let guard = dsn.map(|dsn| {
        sentry::init((
            dsn,
            sentry::ClientOptions {
                release: sentry::release_name!(),
                ..Default::default()
            },
        ))
    });
    Ok(guard)
}

fn setup_tracing(config: Option<&OpenTelemetryConfig>) -> Result<Option<opentelemetry::Guard>> {
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_span_events(FmtSpan::NEW | FmtSpan::CLOSE)
        .with_filter(EnvFilter::from_env("UPLOAD_SYMBOLS_LOG"));
    if let Some(config) = config {
        let (otel_guard, otel_layer) = opentelemetry::set_up(config)?;
        tracing_subscriber::registry()
            .with(fmt_layer)
            .with(otel_layer)
            .init();
        Ok(Some(otel_guard))
    } else {
        tracing_subscriber::registry().with(fmt_layer).init();
        Ok(None)
    }
}

#[tokio::main]
async fn upload_directory(args: Args) -> Result<ExitCode> {
    let client = args.client_builder.build().await?;
    let otel_guard = setup_tracing(client.auth_info().opentelemetry.as_ref())?;
    println!("Uploading symbols files in {}...", args.directory.display());
    let summary = client.upload_directory(args.directory).await?;
    if let Some(Err(error)) = otel_guard.map(|guard| guard.shutdown()) {
        eprintln!("error: shutting down OTLP submission failed: {error}");
    }
    if !summary.upload_errors.is_empty() {
        eprintln!("\nerror: the following keys failed to upload:");
        for key in &summary.failed_keys {
            eprintln!("    {key}");
        }
        eprintln!("\nErrors during upload:");
        for error in &summary.upload_errors {
            eprintln!("{error}");
        }
    }
    if !summary.discovery_errors.is_empty() {
        eprintln!("\nErrors during symbols file discovery:");
        for error in &summary.discovery_errors {
            eprintln!("{error}");
        }
    }
    if summary.success() {
        println!(
            "{} files uploaded, {} skipped.",
            summary.uploaded_keys.len(),
            summary.skipped_keys.len()
        );
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::FAILURE)
    }
}

mod opentelemetry;
