// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    collections::HashSet,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
    time::Duration,
};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use ipnet::Ipv4Net;
use serde::Deserialize;
use thiserror::Error;

use crate::cli::{AgentConfigArgs, ServerConfigArgs};
use stellaris::{
    data_plane::{
        MAX_OVERLAY_MTU, MAX_OVERLAY_PREFIX_LEN, MIN_OVERLAY_MTU, MIN_OVERLAY_PREFIX_LEN,
    },
    peer_manager::{MAX_P2P_IDLE_TIMEOUT, MIN_P2P_IDLE_TIMEOUT},
};

pub const CONFIG_VERSION: u16 = 2;
pub const DEFAULT_MTU: u16 = 1100;
pub const DEFAULT_P2P_IDLE_TIMEOUT_SECS: u64 = 300;
pub const MAX_STATIC_NODES: usize = 256;
const MIN_PENDING_HANDSHAKES: usize = 2;
const MAX_PENDING_HANDSHAKES: usize = 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfigFile {
    pub version: u16,
    pub network: NetworkSection,
    pub registry: RegistrySection,
    pub storage: StorageSection,
    pub tls: ServerTlsSection,
    pub listeners: ServerListenersSection,
    pub limits: LimitsSection,
    #[serde(default)]
    pub observability: Option<ObservabilitySection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NetworkSection {
    pub overlay_cidr: Ipv4Net,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RegistrySection {
    pub nodes_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageSection {
    pub coordinator_state_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerTlsSection {
    pub server_name: String,
    pub service_cert_file: PathBuf,
    pub service_key_file: PathBuf,
    pub node_ca_cert_file: PathBuf,
    pub node_ca_key_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerListenersSection {
    pub enrollment: SocketAddr,
    pub control: SocketAddr,
    pub relay: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LimitsSection {
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
    #[serde(default = "default_max_connections_per_ip")]
    pub max_connections_per_ip: usize,
    #[serde(default = "default_max_pending_handshakes")]
    pub max_pending_handshakes: usize,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigFile {
    pub version: u16,
    pub agent: AgentSection,
    pub coordinator: CoordinatorSection,
    pub identity: IdentitySection,
    pub p2p: P2pSection,
    #[serde(default)]
    pub observability: Option<ObservabilitySection>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub node_id: String,
    #[serde(default)]
    pub tun_name: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CoordinatorSection {
    pub enrollment_addr: SocketAddr,
    pub control_addr: SocketAddr,
    pub relay_addr: SocketAddr,
    pub server_name: String,
    pub deployment_ca_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentitySection {
    pub directory: PathBuf,
    #[serde(default)]
    pub enrollment_token_file: Option<PathBuf>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct P2pSection {
    pub bind: SocketAddr,
    #[serde(default = "default_p2p_idle_timeout_secs")]
    pub idle_timeout_secs: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ObservabilitySection {
    #[serde(default)]
    pub metrics_bind: Option<SocketAddr>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodesConfigFile {
    pub version: u16,
    pub nodes: Vec<NodeConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub id: String,
    pub ipv4: Ipv4Addr,
    pub enrollment_token_sha256: String,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("cannot read configuration {path}: {source}")]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid TOML in {path}: {source}")]
    Toml {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid configuration: {0}")]
    Invalid(String),
}

pub fn load_server(args: &ServerConfigArgs) -> Result<ServerConfigFile, ConfigError> {
    let value = load_server_common(&args.config)?;
    validate_initialized_server_files(&value)?;
    Ok(value)
}

pub fn load_server_for_init(args: &ServerConfigArgs) -> Result<ServerConfigFile, ConfigError> {
    load_server_common(&args.config)
}

fn load_server_common(path: &Path) -> Result<ServerConfigFile, ConfigError> {
    let mut value: ServerConfigFile = read_toml(path)?;
    ensure_version(value.version)?;
    resolve_server_paths(path, &mut value);
    validate_server(&value)?;
    validate_server_inputs(&value)?;
    let nodes = load_nodes(&value.registry.nodes_file, Some(value.network.overlay_cidr))?;
    if nodes.nodes.len() > value.limits.max_nodes {
        return Err(ConfigError::Invalid(format!(
            "node registry contains {} entries; configured maximum is {}",
            nodes.nodes.len(),
            value.limits.max_nodes
        )));
    }
    Ok(value)
}

pub fn load_agent(args: &AgentConfigArgs) -> Result<AgentConfigFile, ConfigError> {
    let mut value: AgentConfigFile = read_toml(&args.config)?;
    ensure_version(value.version)?;
    resolve_agent_paths(&args.config, &mut value);
    validate_agent(&value)?;
    validate_agent_files(&value)?;
    Ok(value)
}

pub fn load_nodes(path: &Path, overlay: Option<Ipv4Net>) -> Result<NodesConfigFile, ConfigError> {
    let value: NodesConfigFile = read_toml(path)?;
    ensure_version(value.version)?;
    validate_nodes(&value, overlay)?;
    Ok(value)
}

pub fn check_file(path: &Path) -> Result<&'static str, ConfigError> {
    let source = read_source(path)?;
    let generic: toml::Value = toml::from_str(&source).map_err(|source| ConfigError::Toml {
        path: path.to_path_buf(),
        source,
    })?;
    let table = generic
        .as_table()
        .ok_or_else(|| ConfigError::Invalid("configuration root must be a table".into()))?;
    let server = table.contains_key("network") || table.contains_key("registry");
    let agent = table.contains_key("agent") || table.contains_key("coordinator");
    let nodes = table.contains_key("nodes");

    match (server, agent, nodes) {
        (true, false, false) => {
            let mut value: ServerConfigFile = parse_toml(path, &source)?;
            ensure_version(value.version)?;
            resolve_server_paths(path, &mut value);
            validate_server(&value)?;
            validate_server_inputs(&value)?;
            let registry =
                load_nodes(&value.registry.nodes_file, Some(value.network.overlay_cidr))?;
            if registry.nodes.len() > value.limits.max_nodes {
                return Err(ConfigError::Invalid(format!(
                    "node registry contains {} entries; configured maximum is {}",
                    registry.nodes.len(),
                    value.limits.max_nodes
                )));
            }
            Ok("server")
        }
        (false, true, false) => {
            let mut value: AgentConfigFile = parse_toml(path, &source)?;
            ensure_version(value.version)?;
            resolve_agent_paths(path, &mut value);
            validate_agent(&value)?;
            validate_agent_files(&value)?;
            Ok("agent")
        }
        (false, false, true) => {
            let value: NodesConfigFile = parse_toml(path, &source)?;
            ensure_version(value.version)?;
            validate_nodes(&value, None)?;
            Ok("nodes")
        }
        _ => Err(ConfigError::Invalid(
            "configuration must be exactly one v2 server, agent, or static-node document".into(),
        )),
    }
}

fn resolve_server_paths(config_path: &Path, value: &mut ServerConfigFile) {
    let base = config_base(config_path);
    resolve_path(&base, &mut value.registry.nodes_file);
    resolve_path(&base, &mut value.storage.coordinator_state_file);
    resolve_path(&base, &mut value.tls.service_cert_file);
    resolve_path(&base, &mut value.tls.service_key_file);
    resolve_path(&base, &mut value.tls.node_ca_cert_file);
    resolve_path(&base, &mut value.tls.node_ca_key_file);
}

fn resolve_agent_paths(config_path: &Path, value: &mut AgentConfigFile) {
    let base = config_base(config_path);
    resolve_path(&base, &mut value.coordinator.deployment_ca_file);
    resolve_path(&base, &mut value.identity.directory);
    if let Some(path) = value.identity.enrollment_token_file.as_mut() {
        resolve_path(&base, path);
    }
}

fn validate_server(value: &ServerConfigFile) -> Result<(), ConfigError> {
    if !(MIN_OVERLAY_MTU..=MAX_OVERLAY_MTU).contains(&usize::from(value.network.mtu)) {
        return Err(ConfigError::Invalid(format!(
            "MTU must be between {MIN_OVERLAY_MTU} and {MAX_OVERLAY_MTU}"
        )));
    }
    if !(MIN_OVERLAY_PREFIX_LEN..=MAX_OVERLAY_PREFIX_LEN)
        .contains(&value.network.overlay_cidr.prefix_len())
    {
        return Err(ConfigError::Invalid(format!(
            "overlay CIDR prefix must be between /{MIN_OVERLAY_PREFIX_LEN} and /{MAX_OVERLAY_PREFIX_LEN}"
        )));
    }
    validate_server_name(&value.tls.server_name)?;

    let listeners = [
        ("enrollment", value.listeners.enrollment),
        ("control", value.listeners.control),
        ("relay", value.listeners.relay),
    ];
    for (name, listener) in listeners {
        validate_ipv4_socket(listener, true, name)?;
    }
    for (index, (left_name, left)) in listeners.iter().enumerate() {
        if let Some((right_name, _)) = listeners[index + 1..]
            .iter()
            .find(|(_, right)| listener_conflicts(*left, *right))
        {
            return Err(ConfigError::Invalid(format!(
                "{left_name} listener {left} conflicts with {right_name} listener"
            )));
        }
    }
    if value.limits.max_nodes == 0 || value.limits.max_nodes > MAX_STATIC_NODES {
        return Err(ConfigError::Invalid(format!(
            "max_nodes must be between 1 and {MAX_STATIC_NODES}"
        )));
    }
    for (name, limit) in [
        (
            "max_connections_per_ip",
            value.limits.max_connections_per_ip,
        ),
        ("queue_capacity", value.limits.queue_capacity),
    ] {
        if limit == 0 {
            return Err(ConfigError::Invalid(format!("{name} must be non-zero")));
        }
    }
    if value.limits.max_connections_per_ip > value.limits.max_nodes {
        return Err(ConfigError::Invalid(
            "max_connections_per_ip must not exceed max_nodes".to_owned(),
        ));
    }
    if !(MIN_PENDING_HANDSHAKES..=MAX_PENDING_HANDSHAKES)
        .contains(&value.limits.max_pending_handshakes)
    {
        return Err(ConfigError::Invalid(format!(
            "max_pending_handshakes must be between {MIN_PENDING_HANDSHAKES} and {MAX_PENDING_HANDSHAKES}"
        )));
    }
    if value.limits.queue_capacity > value.limits.max_nodes {
        return Err(ConfigError::Invalid(
            "queue_capacity must not exceed max_nodes".to_owned(),
        ));
    }
    validate_metrics_bind(value.observability.as_ref())?;

    let paths = [
        ("nodes_file", &value.registry.nodes_file),
        (
            "coordinator_state_file",
            &value.storage.coordinator_state_file,
        ),
        ("service_cert_file", &value.tls.service_cert_file),
        ("service_key_file", &value.tls.service_key_file),
        ("node_ca_cert_file", &value.tls.node_ca_cert_file),
        ("node_ca_key_file", &value.tls.node_ca_key_file),
    ];
    validate_distinct_paths(&paths)
}

fn validate_agent(value: &AgentConfigFile) -> Result<(), ConfigError> {
    validate_node_id(&value.agent.node_id)?;
    if value
        .agent
        .tun_name
        .as_deref()
        .is_some_and(|name| name.is_empty() || name.len() > 63 || name.contains('\0'))
    {
        return Err(ConfigError::Invalid(
            "agent.tun_name must contain 1 to 63 non-NUL bytes when set".into(),
        ));
    }
    validate_server_name(&value.coordinator.server_name)?;
    let coordinator_endpoints = [
        ("enrollment_addr", value.coordinator.enrollment_addr),
        ("control_addr", value.coordinator.control_addr),
        ("relay_addr", value.coordinator.relay_addr),
    ];
    for (name, endpoint) in coordinator_endpoints {
        validate_ipv4_socket(endpoint, false, name)?;
    }
    for (index, (left_name, left)) in coordinator_endpoints.iter().enumerate() {
        if let Some((right_name, _)) = coordinator_endpoints[index + 1..]
            .iter()
            .find(|(_, right)| left == right)
        {
            return Err(ConfigError::Invalid(format!(
                "{left_name} {left} must differ from {right_name}"
            )));
        }
    }
    validate_ipv4_socket(value.p2p.bind, true, "p2p.bind")?;
    let idle_timeout = Duration::from_secs(value.p2p.idle_timeout_secs);
    if !(MIN_P2P_IDLE_TIMEOUT..=MAX_P2P_IDLE_TIMEOUT).contains(&idle_timeout) {
        return Err(ConfigError::Invalid(format!(
            "p2p.idle_timeout_secs must be between {} and {}",
            MIN_P2P_IDLE_TIMEOUT.as_secs(),
            MAX_P2P_IDLE_TIMEOUT.as_secs()
        )));
    }
    if value
        .identity
        .enrollment_token_file
        .as_ref()
        .is_some_and(|path| value.identity.directory == *path)
    {
        return Err(ConfigError::Invalid(
            "identity.directory and enrollment_token_file must be different paths".into(),
        ));
    }
    validate_metrics_bind(value.observability.as_ref())
}

fn validate_server_inputs(value: &ServerConfigFile) -> Result<(), ConfigError> {
    ensure_regular_file(&value.registry.nodes_file)?;
    ensure_pem_file(&value.tls.service_cert_file, "CERTIFICATE")?;
    ensure_pem_file(&value.tls.service_key_file, "PRIVATE KEY")?;
    ensure_secret_file(&value.tls.service_key_file)
}

fn validate_initialized_server_files(value: &ServerConfigFile) -> Result<(), ConfigError> {
    ensure_pem_file(&value.tls.node_ca_cert_file, "CERTIFICATE")?;
    ensure_pem_file(&value.tls.node_ca_key_file, "PRIVATE KEY")?;
    ensure_secret_file(&value.tls.node_ca_key_file)?;
    ensure_secret_file(&value.storage.coordinator_state_file)
}

fn validate_agent_files(value: &AgentConfigFile) -> Result<(), ConfigError> {
    ensure_pem_file(&value.coordinator.deployment_ca_file, "CERTIFICATE")?;
    if let Some(path) = &value.identity.enrollment_token_file {
        ensure_secret_file(path)?;
        let token = fs::read_to_string(path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        if !is_valid_enrollment_token(token.trim_end()) {
            return Err(ConfigError::Invalid(format!(
                "{} does not contain a valid stl2_ enrollment token",
                path.display()
            )));
        }
    }
    validate_identity_directory(&value.identity.directory)
}

fn validate_identity_directory(path: &Path) -> Result<(), ConfigError> {
    let metadata = match fs::metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(source) => {
            return Err(ConfigError::Read {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    if !metadata.is_dir() {
        return Err(ConfigError::Invalid(format!(
            "identity directory {} is not a directory",
            path.display()
        )));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(ConfigError::Invalid(format!(
                "identity directory {} must be owned by the current user and inaccessible to group/other",
                path.display()
            )));
        }
    }
    Ok(())
}

fn validate_nodes(value: &NodesConfigFile, overlay: Option<Ipv4Net>) -> Result<(), ConfigError> {
    if value.nodes.len() > MAX_STATIC_NODES {
        return Err(ConfigError::Invalid(format!(
            "node registry may contain at most {MAX_STATIC_NODES} entries"
        )));
    }
    let mut ids = HashSet::new();
    let mut ips = HashSet::new();
    let mut tokens = HashSet::new();
    for node in &value.nodes {
        validate_node_id(&node.id)?;
        if !ids.insert(node.id.clone()) {
            return Err(ConfigError::Invalid(format!(
                "duplicate node id: {}",
                node.id
            )));
        }
        if !ips.insert(node.ipv4) {
            return Err(ConfigError::Invalid(format!(
                "duplicate node IP: {}",
                node.ipv4
            )));
        }
        if !is_sha256(&node.enrollment_token_sha256)
            || !tokens.insert(node.enrollment_token_sha256.clone())
        {
            return Err(ConfigError::Invalid(format!(
                "invalid or duplicate enrollment token hash for node {}",
                node.id
            )));
        }
        if let Some(network) = overlay
            && (!network.contains(&node.ipv4)
                || node.ipv4 == network.network()
                || node.ipv4 == network.broadcast())
        {
            return Err(ConfigError::Invalid(format!(
                "node {} has unusable IP {} outside {}",
                node.id, node.ipv4, network
            )));
        }
        if node.ipv4.is_unspecified() || node.ipv4.is_multicast() || node.ipv4.is_broadcast() {
            return Err(ConfigError::Invalid(format!(
                "node {} has a non-unicast IP {}",
                node.id, node.ipv4
            )));
        }
    }
    Ok(())
}

pub fn validate_node_id(value: &str) -> Result<(), ConfigError> {
    let valid = !value.is_empty()
        && value.len() <= 63
        && value.bytes().enumerate().all(|(index, byte)| {
            byte.is_ascii_alphanumeric() || (index > 0 && b"._-".contains(&byte))
        });
    if valid {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!("invalid node id: {value}")))
    }
}

fn validate_server_name(value: &str) -> Result<(), ConfigError> {
    if !is_valid_server_name(value) {
        return Err(ConfigError::Invalid(
            "server_name must be an ASCII DNS name or unicast IP accepted by TLS".into(),
        ));
    }
    Ok(())
}

fn is_valid_server_name(value: &str) -> bool {
    if value.is_empty() || value.len() > 253 || !value.is_ascii() {
        return false;
    }
    if let Ok(ip) = value.parse::<IpAddr>() {
        return !is_invalid_socket_ip(ip, false);
    }
    value.split('.').all(|label| {
        !label.is_empty()
            && label.len() <= 63
            && label
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
            && label
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && label
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric)
    })
}

fn validate_ipv4_socket(
    address: SocketAddr,
    allow_unspecified: bool,
    field: &str,
) -> Result<(), ConfigError> {
    if !address.is_ipv4()
        || address.port() == 0
        || is_invalid_socket_ip(address.ip(), allow_unspecified)
    {
        return Err(ConfigError::Invalid(format!(
            "{field} must use a non-zero IPv4 unicast{} address",
            if allow_unspecified {
                " or unspecified"
            } else {
                ""
            }
        )));
    }
    Ok(())
}

fn validate_metrics_bind(value: Option<&ObservabilitySection>) -> Result<(), ConfigError> {
    if let Some(bind) = value.and_then(|section| section.metrics_bind) {
        validate_ipv4_socket(bind, true, "observability.metrics_bind")?;
    }
    Ok(())
}

fn is_invalid_socket_ip(ip: IpAddr, allow_unspecified: bool) -> bool {
    (!allow_unspecified && ip.is_unspecified())
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(address) if address.is_broadcast())
}

fn listener_conflicts(left: SocketAddr, right: SocketAddr) -> bool {
    left.port() == right.port()
        && (left.ip() == right.ip() || left.ip().is_unspecified() || right.ip().is_unspecified())
}

fn validate_distinct_paths(paths: &[(&str, &PathBuf)]) -> Result<(), ConfigError> {
    for (index, (left_name, left)) in paths.iter().enumerate() {
        if let Some((right_name, _)) = paths[index + 1..].iter().find(|(_, right)| *left == *right)
        {
            return Err(ConfigError::Invalid(format!(
                "{left_name} and {right_name} must use different paths"
            )));
        }
    }
    Ok(())
}

fn ensure_regular_file(path: &Path) -> Result<(), ConfigError> {
    let metadata = fs::metadata(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.is_file() {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "{} is not a regular file",
            path.display()
        )))
    }
}

fn ensure_pem_file(path: &Path, marker: &str) -> Result<(), ConfigError> {
    ensure_regular_file(path)?;
    let contents = fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })?;
    let end_marker = format!("{marker}-----");
    if !contents
        .lines()
        .any(|line| line.starts_with("-----BEGIN ") && line.ends_with(&end_marker))
    {
        return Err(ConfigError::Invalid(format!(
            "{} does not contain a PEM {marker} object",
            path.display()
        )));
    }
    Ok(())
}

fn ensure_secret_file(path: &Path) -> Result<(), ConfigError> {
    ensure_regular_file(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt as _, PermissionsExt as _};

        let metadata = fs::metadata(path).map_err(|source| ConfigError::Read {
            path: path.to_path_buf(),
            source,
        })?;
        if metadata.permissions().mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err(ConfigError::Invalid(format!(
                "secret file {} must be owned by the current user and inaccessible to group/other",
                path.display()
            )));
        }
    }
    #[cfg(windows)]
    if !crate::windows_security::secret_acl_is_restricted(path).map_err(|source| {
        ConfigError::Read {
            path: path.to_path_buf(),
            source,
        }
    })? {
        return Err(ConfigError::Invalid(format!(
            "secret file {} may only grant access to its owner, Administrators, and SYSTEM",
            path.display()
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn is_valid_enrollment_token(value: &str) -> bool {
    value
        .strip_prefix("stl2_")
        .and_then(|encoded| URL_SAFE_NO_PAD.decode(encoded).ok())
        .is_some_and(|bytes| bytes.len() == 32)
}

fn ensure_version(version: u16) -> Result<(), ConfigError> {
    if version == CONFIG_VERSION {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "unsupported configuration version {version}; expected {CONFIG_VERSION}; v1 is not supported"
        )))
    }
}

const fn default_mtu() -> u16 {
    DEFAULT_MTU
}

const fn default_p2p_idle_timeout_secs() -> u64 {
    DEFAULT_P2P_IDLE_TIMEOUT_SECS
}

const fn default_max_nodes() -> usize {
    MAX_STATIC_NODES
}

const fn default_max_connections_per_ip() -> usize {
    8
}

const fn default_max_pending_handshakes() -> usize {
    64
}

const fn default_queue_capacity() -> usize {
    256
}

const fn default_enabled() -> bool {
    true
}

fn read_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<T, ConfigError> {
    let source = read_source(path)?;
    parse_toml(path, &source)
}

fn parse_toml<T: for<'de> Deserialize<'de>>(
    path: &Path,
    source_text: &str,
) -> Result<T, ConfigError> {
    toml::from_str(source_text).map_err(|source| ConfigError::Toml {
        path: path.to_path_buf(),
        source,
    })
}

fn read_source(path: &Path) -> Result<String, ConfigError> {
    fs::read_to_string(path).map_err(|source| ConfigError::Read {
        path: path.to_path_buf(),
        source,
    })
}

fn config_base(path: &Path) -> PathBuf {
    path.parent()
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn resolve_path(base: &Path, path: &mut PathBuf) {
    if !path.is_absolute() {
        *path = base.join(&*path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v1_and_legacy_agent_fields_are_rejected() {
        let input = r#"
            version = 1
            [agent]
            node_id = "edge-a"
            server_addr = "127.0.0.1:7000"
            backend = "quinn"
        "#;
        assert!(toml::from_str::<AgentConfigFile>(input).is_err());
        assert!(ensure_version(1).is_err());
    }

    #[test]
    fn strict_v2_agent_schema_parses_without_backend() {
        let input = r#"
            version = 2
            [agent]
            node_id = "edge-a"
            tun_name = "stellaris0"
            [coordinator]
            enrollment_addr = "127.0.0.1:7000"
            control_addr = "127.0.0.1:7001"
            relay_addr = "127.0.0.1:7002"
            server_name = "localhost"
            deployment_ca_file = "deployment-ca.pem"
            [identity]
            directory = "identity"
            enrollment_token_file = "edge-a.token"
            [p2p]
            bind = "0.0.0.0:7003"
        "#;
        let parsed: AgentConfigFile = toml::from_str(input).expect("valid v2 agent config");
        assert_eq!(parsed.p2p.idle_timeout_secs, 300);
    }

    #[test]
    fn enrolled_agent_schema_does_not_require_a_token_file() {
        let input = r#"
            version = 2
            [agent]
            node_id = "edge-a"
            [coordinator]
            enrollment_addr = "127.0.0.1:7000"
            control_addr = "127.0.0.1:7001"
            relay_addr = "127.0.0.1:7002"
            server_name = "localhost"
            deployment_ca_file = "deployment-ca.pem"
            [identity]
            directory = "identity"
            [p2p]
            bind = "0.0.0.0:7100"
        "#;
        let parsed: AgentConfigFile = toml::from_str(input).expect("valid enrolled agent config");
        assert!(parsed.identity.enrollment_token_file.is_none());
    }

    #[test]
    fn agent_idle_timeout_matches_runtime_bounds() {
        let mut value: AgentConfigFile = toml::from_str(
            r#"
                version = 2
                [agent]
                node_id = "edge-a"
                [coordinator]
                enrollment_addr = "127.0.0.1:7000"
                control_addr = "127.0.0.1:7001"
                relay_addr = "127.0.0.1:7002"
                server_name = "localhost"
                deployment_ca_file = "deployment-ca.pem"
                [identity]
                directory = "identity"
                enrollment_token_file = "edge-a.token"
                [p2p]
                bind = "0.0.0.0:7100"
            "#,
        )
        .expect("valid agent schema");

        value.p2p.idle_timeout_secs = MIN_P2P_IDLE_TIMEOUT.as_secs() - 1;
        assert!(validate_agent(&value).is_err());
        value.p2p.idle_timeout_secs = MIN_P2P_IDLE_TIMEOUT.as_secs();
        assert!(validate_agent(&value).is_ok());
        value.p2p.idle_timeout_secs = MAX_P2P_IDLE_TIMEOUT.as_secs();
        assert!(validate_agent(&value).is_ok());
        value.p2p.idle_timeout_secs = MAX_P2P_IDLE_TIMEOUT.as_secs() + 1;
        assert!(validate_agent(&value).is_err());
    }

    #[test]
    fn duplicate_node_addresses_and_tokens_are_rejected() {
        let config = NodesConfigFile {
            version: 2,
            nodes: vec![
                NodeConfig {
                    id: "edge-a".into(),
                    ipv4: "10.88.0.2".parse().expect("IP"),
                    enrollment_token_sha256: format!("sha256:{}", "a".repeat(64)),
                    enabled: true,
                },
                NodeConfig {
                    id: "edge-b".into(),
                    ipv4: "10.88.0.2".parse().expect("IP"),
                    enrollment_token_sha256: format!("sha256:{}", "a".repeat(64)),
                    enabled: true,
                },
            ],
        };
        assert!(validate_nodes(&config, Some("10.88.0.0/24".parse().expect("network"))).is_err());
    }

    #[test]
    fn listeners_must_be_distinct_ipv4_sockets() {
        for pair in [(0, 1), (0, 2), (1, 2)] {
            let mut value = valid_server_config();
            let addresses = [
                value.listeners.enrollment,
                value.listeners.control,
                value.listeners.relay,
            ];
            match pair.1 {
                1 => value.listeners.control = addresses[pair.0],
                2 => value.listeners.relay = addresses[pair.0],
                _ => unreachable!(),
            }
            assert!(validate_server(&value).is_err(), "pair {pair:?}");
        }

        let mut wildcard_conflict = valid_server_config();
        wildcard_conflict.listeners.enrollment = "0.0.0.0:7000".parse().expect("socket");
        wildcard_conflict.listeners.control = "127.0.0.1:7000".parse().expect("socket");
        assert!(validate_server(&wildcard_conflict).is_err());

        let mut distinct_concrete = valid_server_config();
        distinct_concrete.listeners.control = "127.0.0.2:7000".parse().expect("socket");
        assert!(validate_server(&distinct_concrete).is_ok());

        let mut ipv6 = valid_server_config();
        ipv6.listeners.relay = "[::1]:7002".parse().expect("socket");
        assert!(validate_server(&ipv6).is_err());
    }

    #[test]
    fn runtime_capacity_rules_are_checked_by_config_validation() {
        let mut value = valid_server_config();
        value.limits.max_nodes = 0;
        assert!(validate_server(&value).is_err());
        value.limits.max_nodes = MAX_STATIC_NODES + 1;
        assert!(validate_server(&value).is_err());

        value = valid_server_config();
        value.limits.max_connections_per_ip = 0;
        assert!(validate_server(&value).is_err());
        value.limits.max_nodes = 4;
        value.limits.max_connections_per_ip = 5;
        assert!(validate_server(&value).is_err());

        value.limits.max_connections_per_ip = 4;
        value.limits.queue_capacity = 4;
        value.limits.max_pending_handshakes = 1;
        assert!(validate_server(&value).is_err());
        value.limits.max_pending_handshakes = MAX_PENDING_HANDSHAKES + 1;
        assert!(validate_server(&value).is_err());

        value.limits.max_pending_handshakes = 5;
        assert!(validate_server(&value).is_ok());
        value.limits.queue_capacity = 0;
        assert!(validate_server(&value).is_err());
        value.limits.queue_capacity = 5;
        assert!(validate_server(&value).is_err());
        value.limits.queue_capacity = 4;
        assert!(validate_server(&value).is_ok());

        let mut upper_bound = valid_server_config();
        upper_bound.limits.max_connections_per_ip = MAX_STATIC_NODES;
        upper_bound.limits.max_pending_handshakes = MAX_PENDING_HANDSHAKES;
        upper_bound.limits.queue_capacity = MAX_STATIC_NODES;
        assert!(validate_server(&upper_bound).is_ok());

        let mut single_node = valid_server_config();
        single_node.limits.max_nodes = 1;
        single_node.limits.max_connections_per_ip = 1;
        single_node.limits.max_pending_handshakes = MIN_PENDING_HANDSHAKES;
        single_node.limits.queue_capacity = 1;
        assert!(validate_server(&single_node).is_ok());
    }

    #[test]
    fn agent_coordinator_addresses_must_be_distinct() {
        for pair in [(0, 1), (0, 2), (1, 2)] {
            let mut value = valid_agent_config();
            let addresses = [
                value.coordinator.enrollment_addr,
                value.coordinator.control_addr,
                value.coordinator.relay_addr,
            ];
            match pair.1 {
                1 => value.coordinator.control_addr = addresses[pair.0],
                2 => value.coordinator.relay_addr = addresses[pair.0],
                _ => unreachable!(),
            }
            assert!(validate_agent(&value).is_err(), "pair {pair:?}");
        }

        assert!(validate_agent(&valid_agent_config()).is_ok());
    }

    #[test]
    fn registry_uses_enrollment_token_field_only() {
        let old = r#"
            version = 2
            [[nodes]]
            id = "edge-a"
            ipv4 = "10.88.0.2"
            token_sha256 = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
        "#;
        assert!(toml::from_str::<NodesConfigFile>(old).is_err());
    }

    #[test]
    fn token_hash_and_enrollment_token_formats_are_strict() {
        assert!(is_sha256(&format!("sha256:{}", "a1".repeat(32))));
        assert!(!is_sha256(&format!("sha256:{}", "A1".repeat(32))));
        assert!(is_valid_enrollment_token(&format!(
            "stl2_{}",
            URL_SAFE_NO_PAD.encode([7_u8; 32])
        )));
        assert!(!is_valid_enrollment_token(&format!(
            "stl3_{}",
            URL_SAFE_NO_PAD.encode([7_u8; 32])
        )));
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_permissions_must_exclude_group_and_other_users() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!(
            "stellaris-v2-secret-permissions-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::write(&path, "secret").expect("create fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).expect("permissions");
        assert!(ensure_secret_file(&path).is_err());
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).expect("permissions");
        assert!(ensure_secret_file(&path).is_ok());
        fs::remove_file(path).expect("remove fixture");
    }

    fn valid_server_config() -> ServerConfigFile {
        ServerConfigFile {
            version: 2,
            network: NetworkSection {
                overlay_cidr: "10.88.0.0/24".parse().expect("network"),
                mtu: 1100,
            },
            registry: RegistrySection {
                nodes_file: "nodes.toml".into(),
            },
            storage: StorageSection {
                coordinator_state_file: "coordinator.json".into(),
            },
            tls: ServerTlsSection {
                server_name: "localhost".into(),
                service_cert_file: "service.pem".into(),
                service_key_file: "service-key.pem".into(),
                node_ca_cert_file: "node-ca.pem".into(),
                node_ca_key_file: "node-ca-key.pem".into(),
            },
            listeners: ServerListenersSection {
                enrollment: "127.0.0.1:7000".parse().expect("socket"),
                control: "127.0.0.1:7001".parse().expect("socket"),
                relay: "127.0.0.1:7002".parse().expect("socket"),
            },
            limits: LimitsSection {
                max_nodes: 256,
                max_connections_per_ip: 8,
                max_pending_handshakes: 64,
                queue_capacity: 256,
            },
            observability: None,
        }
    }

    fn valid_agent_config() -> AgentConfigFile {
        AgentConfigFile {
            version: 2,
            agent: AgentSection {
                node_id: "edge-a".into(),
                tun_name: Some("stellaris0".into()),
            },
            coordinator: CoordinatorSection {
                enrollment_addr: "127.0.0.1:7000".parse().expect("socket"),
                control_addr: "127.0.0.1:7001".parse().expect("socket"),
                relay_addr: "127.0.0.1:7002".parse().expect("socket"),
                server_name: "localhost".into(),
                deployment_ca_file: "deployment-ca.pem".into(),
            },
            identity: IdentitySection {
                directory: "identity".into(),
                enrollment_token_file: Some("edge-a.token".into()),
            },
            p2p: P2pSection {
                bind: "0.0.0.0:7100".parse().expect("socket"),
                idle_timeout_secs: DEFAULT_P2P_IDLE_TIMEOUT_SECS,
            },
            observability: None,
        }
    }
}
