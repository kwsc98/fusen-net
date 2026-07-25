// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    fs,
    path::{Path, PathBuf},
    str::FromStr,
    sync::Arc,
    time::Duration,
};

use stellaris::{
    agent_runtime::{AgentRuntime, AgentRuntimeConfig, AgentRuntimeError},
    registry::{EnrollmentTokenDigest, RegistryError, StaticNode, StaticNodeRegistry},
    server_runtime::{
        ServerRuntime, ServerRuntimeConfig, ServerRuntimeError, ServerStatePaths,
        initialize_server_state,
    },
};
use thiserror::Error;

use crate::config::{self, AgentConfigFile, ServerConfigFile};

#[derive(Debug, Error)]
pub enum RuntimeError {
    #[error("cannot read {kind} file {path}: {source}")]
    Read {
        kind: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error(transparent)]
    Config(#[from] config::ConfigError),
    #[error(transparent)]
    Registry(#[from] RegistryError),
    #[error(transparent)]
    Server(#[from] ServerRuntimeError),
    #[error(transparent)]
    Agent(#[from] AgentRuntimeError),
    #[error("metrics endpoint failed: {0}")]
    Metrics(#[from] std::io::Error),
}

pub fn init_server(config: ServerConfigFile) -> Result<(), RuntimeError> {
    let paths = server_state_paths(&config);
    initialize_server_state(&paths, config.network.overlay_cidr, config.limits.max_nodes)?;
    Ok(())
}

pub async fn run_server(config: ServerConfigFile) -> Result<(), RuntimeError> {
    let metrics_bind = config
        .observability
        .as_ref()
        .and_then(|section| section.metrics_bind);
    let nodes = config::load_nodes(
        &config.registry.nodes_file,
        Some(config.network.overlay_cidr),
    )?;
    let static_nodes = nodes
        .nodes
        .into_iter()
        .map(|node| {
            Ok(StaticNode::new(
                node.id,
                node.ipv4,
                EnrollmentTokenDigest::from_str(&node.enrollment_token_sha256)?,
                node.enabled,
            ))
        })
        .collect::<Result<Vec<_>, RegistryError>>()?;
    let registry = Arc::new(StaticNodeRegistry::new(
        config.network.overlay_cidr,
        static_nodes,
    )?);
    let service_certificate = read_text("service certificate", &config.tls.service_cert_file)?;
    let service_private_key = read_text("service private key", &config.tls.service_key_file)?;
    let runtime_config = ServerRuntimeConfig {
        overlay: config.network.overlay_cidr,
        registry,
        state_paths: server_state_paths(&config),
        server_name: config.tls.server_name,
        deployment_certificate_pem: service_certificate,
        deployment_private_key_pem: service_private_key,
        enrollment_bind: config.listeners.enrollment,
        control_bind: config.listeners.control,
        relay_bind: config.listeners.relay,
        max_nodes: config.limits.max_nodes,
        max_control_sessions: config.limits.max_nodes,
        max_connections_per_ip: config.limits.max_connections_per_ip,
        max_pending_control_events: config.limits.max_pending_handshakes,
        max_concurrent_enrollments: config.limits.max_pending_handshakes,
        route_queue_capacity: config.limits.queue_capacity,
        mtu: config.network.mtu,
    };
    let runtime = ServerRuntime::bind(runtime_config).await?;
    if let Some(bind) = metrics_bind {
        let metrics = runtime.metrics();
        tokio::select! {
            result = runtime.run() => result?,
            result = crate::observability::serve(bind, metrics) => result?,
        }
    } else {
        runtime.run().await?;
    }
    Ok(())
}

pub async fn run_agent(config: AgentConfigFile) -> Result<(), RuntimeError> {
    let metrics_bind = config
        .observability
        .as_ref()
        .and_then(|section| section.metrics_bind);
    let deployment_ca = read_text(
        "deployment CA certificate",
        &config.coordinator.deployment_ca_file,
    )?;
    let enrollment_token = config
        .identity
        .enrollment_token_file
        .as_deref()
        .map(|path| read_text("enrollment token", path))
        .transpose()?;
    let mut runtime_config = AgentRuntimeConfig::new(
        config.agent.node_id,
        config.identity.directory,
        deployment_ca,
        config.coordinator.server_name,
        config.coordinator.enrollment_addr,
        config.coordinator.control_addr,
        config.coordinator.relay_addr,
        config.p2p.bind,
    );
    runtime_config.tun_name = config.agent.tun_name;
    runtime_config.enrollment_token = enrollment_token.map(|token| token.trim_end().to_owned());
    runtime_config.p2p_idle_timeout = Duration::from_secs(config.p2p.idle_timeout_secs);
    let runtime = AgentRuntime::new(runtime_config)?;
    if let Some(bind) = metrics_bind {
        let metrics = runtime.metrics();
        tokio::select! {
            result = runtime.run() => result?,
            result = crate::observability::serve(bind, metrics) => result?,
        }
    } else {
        runtime.run().await?;
    }
    Ok(())
}

fn server_state_paths(config: &ServerConfigFile) -> ServerStatePaths {
    ServerStatePaths::new(
        &config.tls.node_ca_cert_file,
        &config.tls.node_ca_key_file,
        &config.storage.coordinator_state_file,
    )
}

fn read_text(kind: &'static str, path: &Path) -> Result<String, RuntimeError> {
    fs::read_to_string(path).map_err(|source| RuntimeError::Read {
        kind,
        path: path.to_path_buf(),
        source,
    })
}
