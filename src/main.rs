use clap::{Parser, Subcommand};

#[allow(dead_code)]
mod config;

#[derive(Parser)]
#[command(name = "vaultkeeper", version, about)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Validate configuration, database, and required tools
    CheckConfig,
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().init();
    let cli = Cli::parse();
    match cli.command {
        Command::CheckConfig => {
            println!("check-config: not yet implemented");
            Ok(())
        }
    }
}
