mod cli;
mod fetch;
mod fs;
mod memory;
mod shell;
mod support;

use clap::Parser;
use tracing_subscriber::{EnvFilter, layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::registry()
        .with(
            EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "modelcontextprotocol=warn".into()),
        )
        .with(
            tracing_subscriber::fmt::layer()
                .with_writer(std::io::stderr)
                .with_ansi(false),
        )
        .init();

    let cli = cli::Cli::parse();

    match cli.into_command() {
        Some(cli::Command::Filesystem { dirs }) => fs::run(dirs).await,
        Some(cli::Command::Fetch(options)) => fetch::run(options).await,
        Some(cli::Command::Memory { memory_file }) => memory::run(memory_file).await,
        Some(cli::Command::Shell { dirs }) => shell::run(dirs).await,
        None => {
            cli::print_usage();
            std::process::exit(1);
        }
    }
}
