use anyhow::Result;
use clap::{
    Parser,
    builder::{Styles, styling::AnsiColor},
};
use std::{path::PathBuf, process::ExitCode};
use upload_symbols::ClientBuilder;

/// Upload symbols files to the Mozilla Symbols Server.
///
/// All symbols files in the source directory are discovered and uploaded to the Mozilla
/// Symbols Server. You need an authentication token with upload permissions for the server you
/// are uploading to and sore it in the `SYMBOLS_AUTH_TOKEN` environment variable.
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

#[tokio::main]
async fn main() -> Result<ExitCode> {
    let args = Args::parse();
    let client = args.client_builder.build()?;
    println!("Uploading symbols files in {}...", args.directory.display());
    let summary = client.upload_directory(args.directory).await?;

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
