// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::{Path, PathBuf};

use clap::{Args, Parser, Subcommand};

#[derive(Debug, Parser)]
#[command(
    name = "stellaris",
    version,
    about = "Distributed IPv4 overlay network"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Initialize or run the coordination service and trusted relay.
    Server(ServerArgs),
    /// Run an overlay agent backed by a local TUN interface.
    Agent(AgentArgs),
    /// Validate a v2 configuration without starting a runtime.
    Config(ConfigArgs),
    /// Generate a one-time enrollment token.
    Token(TokenArgs),
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    #[command(subcommand)]
    pub command: ServerCommand,
}

#[derive(Debug, Subcommand)]
pub enum ServerCommand {
    /// Create a new node CA and empty coordinator state.
    Init(ServerConfigArgs),
    /// Run the coordination service and trusted relay.
    Run(ServerConfigArgs),
}

#[derive(Debug, Args)]
pub struct ServerConfigArgs {
    #[arg(long, env = "STELLARIS_CONFIG", default_value = "server.toml")]
    pub config: PathBuf,
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[command(subcommand)]
    pub command: AgentCommand,
}

#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Run the agent, enrolling it first when necessary.
    Run(AgentConfigArgs),
}

#[derive(Debug, Args)]
pub struct AgentConfigArgs {
    #[arg(long, env = "STELLARIS_CONFIG", default_value = "agent.toml")]
    pub config: PathBuf,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Validate a server, agent, or static-node configuration.
    Check {
        #[arg(long, env = "STELLARIS_CONFIG")]
        config: PathBuf,
    },
}

impl ConfigCommand {
    pub fn path(&self) -> &Path {
        match self {
            Self::Check { config } => config,
        }
    }
}

#[derive(Debug, Args)]
pub struct TokenArgs {
    #[command(subcommand)]
    pub command: TokenCommand,
}

#[derive(Debug, Subcommand)]
pub enum TokenCommand {
    /// Generate a token file and print its registry digest.
    Generate {
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        output: PathBuf,
    },
}

impl TokenCommand {
    pub fn values(&self) -> (&str, &Path) {
        match self {
            Self::Generate { node_id, output } => (node_id, output),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v2_command_tree_requires_explicit_run_or_init() {
        assert!(Cli::try_parse_from(["stellaris", "server", "run"]).is_ok());
        assert!(Cli::try_parse_from(["stellaris", "server", "init"]).is_ok());
        assert!(Cli::try_parse_from(["stellaris", "agent", "run"]).is_ok());
        assert!(Cli::try_parse_from(["stellaris", "server"]).is_err());
        assert!(Cli::try_parse_from(["stellaris", "agent"]).is_err());
    }

    #[test]
    fn legacy_runtime_overrides_are_rejected() {
        assert!(Cli::try_parse_from(["stellaris", "server", "run", "--backend", "quinn"]).is_err());
        assert!(
            Cli::try_parse_from([
                "stellaris",
                "agent",
                "run",
                "--server-addr",
                "127.0.0.1:7000"
            ])
            .is_err()
        );
    }
}
