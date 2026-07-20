// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use crate::config::Backend;

#[derive(Debug, Parser)]
#[command(name = "fusen-net", version, about = "QUIC-based IPv4 overlay network")]
pub struct Cli {
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Run a central relay with one or more QUIC listeners.
    Server(ServerArgs),
    /// Run an edge node backed by a local TUN interface.
    Agent(AgentArgs),
    /// Validate configuration without starting the network runtime.
    Config(ConfigArgs),
    /// Generate node credentials.
    Token(TokenArgs),
}

#[derive(Debug, Args)]
pub struct ServerArgs {
    #[arg(long, env = "FUSEN_CONFIG", default_value = "server.toml")]
    pub config: PathBuf,
    #[arg(long, env = "FUSEN_OVERLAY_CIDR")]
    pub overlay_cidr: Option<String>,
    #[arg(long, env = "FUSEN_MTU")]
    pub mtu: Option<u16>,
    #[arg(long, env = "FUSEN_NODES_FILE")]
    pub nodes_file: Option<PathBuf>,
    #[arg(long, env = "FUSEN_CERT_FILE")]
    pub cert_file: Option<PathBuf>,
    #[arg(long, env = "FUSEN_KEY_FILE")]
    pub key_file: Option<PathBuf>,
    #[arg(long, env = "FUSEN_SERVER_NAME")]
    pub server_name: Option<String>,
    #[arg(long, env = "FUSEN_BACKEND", value_enum, requires = "bind")]
    pub backend: Option<Backend>,
    #[arg(long, env = "FUSEN_BIND", requires = "backend")]
    pub bind: Option<String>,
}

#[derive(Debug, Args)]
pub struct AgentArgs {
    #[arg(long, env = "FUSEN_CONFIG", default_value = "agent.toml")]
    pub config: PathBuf,
    #[arg(long, env = "FUSEN_NODE_ID")]
    pub node_id: Option<String>,
    #[arg(long, env = "FUSEN_SERVER_ADDR")]
    pub server_addr: Option<String>,
    #[arg(long, env = "FUSEN_BACKEND", value_enum)]
    pub backend: Option<Backend>,
    #[arg(long, env = "FUSEN_SERVER_NAME")]
    pub server_name: Option<String>,
    #[arg(long, env = "FUSEN_CA_FILE")]
    pub ca_file: Option<PathBuf>,
    #[arg(long, env = "FUSEN_TOKEN_FILE")]
    pub token_file: Option<PathBuf>,
    #[arg(long, env = "FUSEN_TUN_NAME")]
    pub tun_name: Option<String>,
}

#[derive(Debug, Args)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    Check {
        #[arg(long, env = "FUSEN_CONFIG")]
        config: PathBuf,
    },
}

impl ConfigCommand {
    pub fn path(&self) -> &PathBuf {
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
    Generate {
        #[arg(long)]
        node_id: String,
        #[arg(long)]
        output: PathBuf,
    },
}

impl TokenCommand {
    pub fn values(&self) -> (&str, &PathBuf) {
        match self {
            Self::Generate { node_id, output } => (node_id, output),
        }
    }
}
