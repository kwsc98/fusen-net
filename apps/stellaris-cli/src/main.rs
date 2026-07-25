// SPDX-License-Identifier: Apache-2.0 OR MIT

mod cli;
mod config;
mod observability;
mod runtime;
mod token;
#[cfg(windows)]
mod windows_security;

use clap::Parser;
use cli::{AgentCommand, Cli, Command, ServerCommand};
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() {
    init_logging();
    if let Err(error) = run(Cli::parse()).await {
        tracing::error!(error = %error, "command failed");
        std::process::exit(1);
    }
}

async fn run(cli: Cli) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    match cli.command {
        Command::Server(args) => match args.command {
            ServerCommand::Init(args) => {
                runtime::init_server(config::load_server_for_init(&args)?)?
            }
            ServerCommand::Run(args) => runtime::run_server(config::load_server(&args)?).await?,
        },
        Command::Agent(args) => match args.command {
            AgentCommand::Run(args) => runtime::run_agent(config::load_agent(&args)?).await?,
        },
        Command::Config(args) => {
            let kind = config::check_file(args.command.path())?;
            println!(
                "valid {kind} configuration: {}",
                args.command.path().display()
            );
        }
        Command::Token(args) => {
            let (node_id, output) = args.command.values();
            let generated = token::generate(node_id, output)?;
            println!("node_id = {:?}", generated.node_id);
            println!(
                "enrollment_token_sha256 = {:?}",
                generated.enrollment_token_sha256
            );
        }
    }
    Ok(())
}

fn init_logging() {
    let filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .compact()
        .init();
}
