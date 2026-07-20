// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{net::SocketAddr, str::FromStr, sync::Arc};

use fusen_net::{
    address::{StaticAddressAllocator, StaticBinding, TokenDigest},
    data_plane::PacketValidator,
    node::{EdgeRuntime, NodeIdentity, RelayRuntime},
    routing::{DEFAULT_ROUTE_QUEUE_CAPACITY, RouteTable},
    transport::{
        ClientTransportConfig, ServerTransportConfig, TransportBackend, make_client_endpoint,
        make_server_endpoint,
    },
    tun::{NativeRouteManager, NativeTunFactory},
};

use crate::config::{self, AgentConfigFile, Backend, ServerConfigFile};

pub async fn run_server(
    config: ServerConfigFile,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let certificate_pem = tokio::fs::read_to_string(&config.tls.cert_file).await?;
    let private_key_pem = tokio::fs::read_to_string(&config.tls.key_file).await?;
    let nodes = config::load_nodes(&config.server.nodes_file, Some(config.server.overlay_cidr))?;
    let bindings = nodes
        .nodes
        .into_iter()
        .filter(|node| node.enabled)
        .map(|node| {
            Ok(StaticBinding::new(
                node.id,
                TokenDigest::from_str(&node.token_sha256)?,
                node.ipv4,
            ))
        })
        .collect::<Result<Vec<_>, fusen_net::address::AddressError>>()?;
    let allocator = Arc::new(StaticAddressAllocator::new(
        config.server.overlay_cidr,
        bindings,
    )?);
    let routes = Arc::new(RouteTable::new(
        config.server.overlay_cidr,
        DEFAULT_ROUTE_QUEUE_CAPACITY,
    )?);
    let validator =
        PacketValidator::new(config.server.overlay_cidr, usize::from(config.server.mtu))?;
    let runtime = Arc::new(RelayRuntime::new(allocator, routes, validator)?);

    let mut listeners = Vec::with_capacity(config.listeners.len());
    for listener in config.listeners {
        let transport_config = ServerTransportConfig::new(
            listener.bind,
            config.tls.server_name.clone(),
            certificate_pem.clone(),
            private_key_pem.clone(),
        );
        listeners.push(make_server_endpoint(
            transport_backend(listener.backend),
            transport_config,
        )?);
    }

    tracing::info!(
        overlay = %config.server.overlay_cidr,
        mtu = config.server.mtu,
        listeners = listeners.len(),
        "relay started"
    );
    tokio::select! {
        result = runtime.run_multi(listeners) => result?,
        () = shutdown_signal() => tracing::info!("relay shutdown requested"),
    }
    Ok(())
}

pub async fn run_agent(
    config: AgentConfigFile,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let token = tokio::fs::read_to_string(&config.agent.token_file).await?;
    let identity = NodeIdentity::new(config.agent.node_id, token.trim_end())?;
    let ca_certificate_pem = tokio::fs::read_to_string(&config.agent.ca_file).await?;
    let bind_address: SocketAddr = if config.agent.server_addr.is_ipv4() {
        "0.0.0.0:0".parse()?
    } else {
        "[::]:0".parse()?
    };
    let endpoint = make_client_endpoint(
        transport_backend(config.agent.backend),
        ClientTransportConfig::new(bind_address, ca_certificate_pem),
    )?;
    let runtime = EdgeRuntime::new(
        identity,
        config.agent.server_addr,
        config.agent.server_name,
        config.agent.tun_name,
    )?;

    tracing::info!(server = %config.agent.server_addr, "agent started");
    let (shutdown_sender, mut shutdown_receiver) = tokio::sync::watch::channel(false);
    let run = runtime.run_with_reconnect_until(
        endpoint,
        Arc::new(NativeTunFactory),
        Arc::new(NativeRouteManager),
        &mut shutdown_receiver,
    );
    tokio::pin!(run);
    tokio::select! {
        result = &mut run => result?,
        () = shutdown_signal() => {
            tracing::info!("agent shutdown requested");
            let _ = shutdown_sender.send(true);
            run.await?;
        }
    }
    Ok(())
}

const fn transport_backend(backend: Backend) -> TransportBackend {
    match backend {
        Backend::Quinn => TransportBackend::Quinn,
        Backend::S2n => TransportBackend::S2n,
        Backend::GmQuic => TransportBackend::GmQuic,
    }
}

#[cfg(unix)]
async fn shutdown_signal() {
    use tokio::signal::unix::{SignalKind, signal};

    let terminate = signal(SignalKind::terminate());
    match terminate {
        Ok(mut terminate) => {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    if let Err(error) = result {
                        tracing::warn!(%error, "failed to install Ctrl-C handler");
                    }
                }
                _ = terminate.recv() => {}
            }
        }
        Err(error) => {
            tracing::warn!(%error, "failed to install SIGTERM handler");
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    if let Err(error) = tokio::signal::ctrl_c().await {
        tracing::warn!(%error, "failed to install Ctrl-C handler");
    }
}
