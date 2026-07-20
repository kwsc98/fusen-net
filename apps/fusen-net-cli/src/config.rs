// SPDX-License-Identifier: Apache-2.0 OR MIT

use std::{
    collections::HashSet,
    fs,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::{Path, PathBuf},
};

use clap::ValueEnum;
use ipnet::Ipv4Net;
use serde::Deserialize;
use thiserror::Error;

use crate::cli::{AgentArgs, ServerArgs};
use fusen_net::data_plane::{
    MAX_OVERLAY_MTU, MAX_OVERLAY_PREFIX_LEN, MIN_OVERLAY_MTU, MIN_OVERLAY_PREFIX_LEN,
};

pub const CONFIG_VERSION: u16 = 1;
pub const DEFAULT_MTU: u16 = 1100;

#[derive(Debug, Clone, Copy, Deserialize, ValueEnum, PartialEq, Eq, Hash)]
#[serde(rename_all = "kebab-case")]
pub enum Backend {
    Quinn,
    S2n,
    GmQuic,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfigFile {
    pub version: u16,
    pub server: ServerSection,
    pub tls: TlsSection,
    pub listeners: Vec<ListenerConfig>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerSection {
    pub overlay_cidr: Ipv4Net,
    #[serde(default = "default_mtu")]
    pub mtu: u16,
    pub nodes_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TlsSection {
    pub server_name: String,
    pub cert_file: PathBuf,
    pub key_file: PathBuf,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct ListenerConfig {
    pub backend: Backend,
    pub bind: SocketAddr,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentConfigFile {
    pub version: u16,
    pub agent: AgentSection,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentSection {
    pub node_id: String,
    pub server_addr: SocketAddr,
    pub backend: Backend,
    pub server_name: String,
    pub ca_file: PathBuf,
    pub token_file: PathBuf,
    #[serde(default)]
    pub tun_name: Option<String>,
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
    pub token_sha256: String,
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

pub fn load_server(args: &ServerArgs) -> Result<ServerConfigFile, ConfigError> {
    let mut value: ServerConfigFile = read_toml(&args.config)?;
    ensure_version(value.version)?;
    let base = config_base(&args.config);
    resolve_path(&base, &mut value.server.nodes_file);
    resolve_path(&base, &mut value.tls.cert_file);
    resolve_path(&base, &mut value.tls.key_file);

    if let Some(cidr) = &args.overlay_cidr {
        value.server.overlay_cidr = cidr
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("invalid overlay CIDR: {cidr}")))?;
    }
    if let Some(mtu) = args.mtu {
        value.server.mtu = mtu;
    }
    if let Some(path) = &args.nodes_file {
        value.server.nodes_file = absolute_from(&base, path);
    }
    if let Some(path) = &args.cert_file {
        value.tls.cert_file = absolute_from(&base, path);
    }
    if let Some(path) = &args.key_file {
        value.tls.key_file = absolute_from(&base, path);
    }
    if let Some(server_name) = &args.server_name {
        value.tls.server_name.clone_from(server_name);
    }
    if let (Some(backend), Some(bind)) = (args.backend, &args.bind) {
        value.listeners = vec![ListenerConfig {
            backend,
            bind: bind
                .parse()
                .map_err(|_| ConfigError::Invalid(format!("invalid bind address: {bind}")))?,
        }];
    }
    validate_server(&value)?;
    validate_server_files(&value)?;
    load_nodes(&value.server.nodes_file, Some(value.server.overlay_cidr))?;
    Ok(value)
}

pub fn load_agent(args: &AgentArgs) -> Result<AgentConfigFile, ConfigError> {
    let mut value: AgentConfigFile = read_toml(&args.config)?;
    ensure_version(value.version)?;
    let base = config_base(&args.config);
    resolve_path(&base, &mut value.agent.ca_file);
    resolve_path(&base, &mut value.agent.token_file);

    if let Some(node_id) = &args.node_id {
        value.agent.node_id.clone_from(node_id);
    }
    if let Some(server_addr) = &args.server_addr {
        value.agent.server_addr = server_addr
            .parse()
            .map_err(|_| ConfigError::Invalid(format!("invalid server address: {server_addr}")))?;
    }
    if let Some(backend) = args.backend {
        value.agent.backend = backend;
    }
    if let Some(server_name) = &args.server_name {
        value.agent.server_name.clone_from(server_name);
    }
    if let Some(path) = &args.ca_file {
        value.agent.ca_file = absolute_from(&base, path);
    }
    if let Some(path) = &args.token_file {
        value.agent.token_file = absolute_from(&base, path);
    }
    if let Some(tun_name) = &args.tun_name {
        value.agent.tun_name = Some(tun_name.clone());
    }
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
    match (
        table.contains_key("server"),
        table.contains_key("agent"),
        table.contains_key("nodes"),
    ) {
        (true, false, false) => {
            let mut value: ServerConfigFile = parse_toml(path, &source)?;
            ensure_version(value.version)?;
            let base = config_base(path);
            resolve_path(&base, &mut value.server.nodes_file);
            resolve_path(&base, &mut value.tls.cert_file);
            resolve_path(&base, &mut value.tls.key_file);
            validate_server(&value)?;
            validate_server_files(&value)?;
            load_nodes(&value.server.nodes_file, Some(value.server.overlay_cidr))?;
            Ok("server")
        }
        (false, true, false) => {
            let mut value: AgentConfigFile = parse_toml(path, &source)?;
            ensure_version(value.version)?;
            let base = config_base(path);
            resolve_path(&base, &mut value.agent.ca_file);
            resolve_path(&base, &mut value.agent.token_file);
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
            "configuration must contain exactly one of [server], [agent], or [[nodes]]".into(),
        )),
    }
}

fn validate_server(value: &ServerConfigFile) -> Result<(), ConfigError> {
    if value.listeners.is_empty() {
        return Err(ConfigError::Invalid(
            "at least one listener is required".into(),
        ));
    }
    if !(MIN_OVERLAY_MTU..=MAX_OVERLAY_MTU).contains(&usize::from(value.server.mtu)) {
        return Err(ConfigError::Invalid(format!(
            "MTU must be between {MIN_OVERLAY_MTU} and {MAX_OVERLAY_MTU}"
        )));
    }
    if !(MIN_OVERLAY_PREFIX_LEN..=MAX_OVERLAY_PREFIX_LEN)
        .contains(&value.server.overlay_cidr.prefix_len())
    {
        return Err(ConfigError::Invalid(format!(
            "overlay CIDR prefix must be between /{MIN_OVERLAY_PREFIX_LEN} and /{MAX_OVERLAY_PREFIX_LEN}"
        )));
    }
    validate_server_name(&value.tls.server_name)?;
    for listener in &value.listeners {
        ensure_backend_available(listener.backend)?;
        if listener.bind.port() == 0 {
            return Err(ConfigError::Invalid(format!(
                "listener {} must use a non-zero port",
                listener.bind
            )));
        }
        if is_invalid_socket_ip(listener.bind.ip(), true) {
            return Err(ConfigError::Invalid(format!(
                "listener {} must bind a local unicast or unspecified address",
                listener.bind
            )));
        }
    }
    for (index, listener) in value.listeners.iter().enumerate() {
        if value.listeners[index + 1..]
            .iter()
            .any(|other| listener_conflicts(listener.bind, other.bind))
        {
            return Err(ConfigError::Invalid(format!(
                "listener {} conflicts with another listener",
                listener.bind
            )));
        }
    }
    Ok(())
}

fn validate_agent(value: &AgentConfigFile) -> Result<(), ConfigError> {
    validate_node_id(&value.agent.node_id)?;
    ensure_backend_available(value.agent.backend)?;
    if value.agent.server_addr.port() == 0 {
        return Err(ConfigError::Invalid(
            "server_addr must use a non-zero port".into(),
        ));
    }
    let ip = value.agent.server_addr.ip();
    if is_invalid_socket_ip(ip, false) {
        return Err(ConfigError::Invalid(
            "server_addr must be a unicast address".into(),
        ));
    }
    validate_server_name(&value.agent.server_name)?;
    Ok(())
}

fn validate_server_name(value: &str) -> Result<(), ConfigError> {
    if !is_valid_server_name(value) {
        return Err(ConfigError::Invalid(
            "server_name must be an ASCII DNS name or IP address accepted by TLS".into(),
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

fn is_invalid_socket_ip(ip: IpAddr, allow_unspecified: bool) -> bool {
    (!allow_unspecified && ip.is_unspecified())
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(address) if address.is_broadcast())
}

fn listener_conflicts(left: SocketAddr, right: SocketAddr) -> bool {
    if left.port() != right.port() {
        return false;
    }
    if left.ip() == right.ip() {
        return true;
    }
    left.ip().is_unspecified() || right.ip().is_unspecified()
}

fn validate_server_files(value: &ServerConfigFile) -> Result<(), ConfigError> {
    ensure_regular_file(&value.server.nodes_file)?;
    ensure_pem_file(&value.tls.cert_file, "CERTIFICATE")?;
    ensure_pem_file(&value.tls.key_file, "PRIVATE KEY")?;
    ensure_secret_file(&value.tls.key_file)?;
    Ok(())
}

fn validate_agent_files(value: &AgentConfigFile) -> Result<(), ConfigError> {
    ensure_pem_file(&value.agent.ca_file, "CERTIFICATE")?;
    ensure_secret_file(&value.agent.token_file)?;
    let token =
        fs::read_to_string(&value.agent.token_file).map_err(|source| ConfigError::Read {
            path: value.agent.token_file.clone(),
            source,
        })?;
    fusen_net::address::NodeToken::parse(token.trim_end()).map_err(|_| {
        ConfigError::Invalid(format!(
            "{} does not contain a valid fsn1 token",
            value.agent.token_file.display()
        ))
    })?;
    Ok(())
}

fn ensure_backend_available(backend: Backend) -> Result<(), ConfigError> {
    let available = match backend {
        Backend::Quinn => cfg!(feature = "backend-quinn"),
        Backend::S2n => cfg!(feature = "backend-s2n"),
        Backend::GmQuic => cfg!(feature = "backend-gm-quic"),
    };
    if available {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "backend {backend:?} is not compiled into this binary"
        )))
    }
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
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(path)
            .map_err(|source| ConfigError::Read {
                path: path.to_path_buf(),
                source,
            })?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            return Err(ConfigError::Invalid(format!(
                "secret file {} must not be accessible by group or other users",
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

fn validate_nodes(value: &NodesConfigFile, overlay: Option<Ipv4Net>) -> Result<(), ConfigError> {
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
        if !is_sha256(&node.token_sha256) || !tokens.insert(node.token_sha256.clone()) {
            return Err(ConfigError::Invalid(format!(
                "invalid or duplicate token hash for node {}",
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

fn is_sha256(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
    })
}

fn ensure_version(version: u16) -> Result<(), ConfigError> {
    if version == CONFIG_VERSION {
        Ok(())
    } else {
        Err(ConfigError::Invalid(format!(
            "unsupported configuration version {version}; expected {CONFIG_VERSION}"
        )))
    }
}

fn default_mtu() -> u16 {
    DEFAULT_MTU
}

fn default_enabled() -> bool {
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
    *path = absolute_from(base, path);
}

fn absolute_from(base: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        base.join(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_id_rules_are_stable() {
        assert!(validate_node_id("edge-01.example").is_ok());
        assert!(validate_node_id("").is_err());
        assert!(validate_node_id("-edge").is_err());
        assert!(validate_node_id("edge space").is_err());
    }

    #[test]
    fn duplicate_node_addresses_are_rejected() {
        let config = NodesConfigFile {
            version: 1,
            nodes: vec![
                NodeConfig {
                    id: "edge-a".into(),
                    ipv4: "10.88.0.2".parse().expect("valid test IP"),
                    token_sha256: format!("sha256:{}", "a".repeat(64)),
                    enabled: true,
                },
                NodeConfig {
                    id: "edge-b".into(),
                    ipv4: "10.88.0.2".parse().expect("valid test IP"),
                    token_sha256: format!("sha256:{}", "b".repeat(64)),
                    enabled: true,
                },
            ],
        };
        assert!(
            validate_nodes(
                &config,
                Some("10.88.0.0/24".parse().expect("valid network"))
            )
            .is_err()
        );
    }

    #[test]
    fn token_hash_requires_lowercase_sha256() {
        assert!(is_sha256(&format!("sha256:{}", "a1".repeat(32))));
        assert!(!is_sha256(&format!("sha256:{}", "A1".repeat(32))));
        assert!(!is_sha256(&"a1".repeat(32)));
    }

    #[test]
    fn strict_toml_rejects_unknown_fields() {
        let input = r#"
            version = 1
            [agent]
            node_id = "edge-a"
            server_addr = "127.0.0.1:7000"
            backend = "quinn"
            server_name = "localhost"
            ca_file = "ca.pem"
            token_file = "edge-a.token"
            unexpected = true
        "#;
        assert!(toml::from_str::<AgentConfigFile>(input).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn secret_file_permissions_must_exclude_group_and_other_users() {
        use std::os::unix::fs::PermissionsExt as _;

        let path = std::env::temp_dir().join(format!(
            "fusen-net-secret-permissions-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        fs::write(&path, "secret").expect("create secret fixture");
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644))
            .expect("set insecure permissions");
        assert!(ensure_secret_file(&path).is_err());

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
            .expect("set private permissions");
        assert!(ensure_secret_file(&path).is_ok());
        fs::remove_file(path).expect("remove secret fixture");
    }

    #[test]
    fn listener_socket_cannot_be_reused_by_another_backend() {
        let value = ServerConfigFile {
            version: 1,
            server: ServerSection {
                overlay_cidr: "10.88.0.0/24".parse().expect("valid network"),
                mtu: 1100,
                nodes_file: "nodes.toml".into(),
            },
            tls: TlsSection {
                server_name: "localhost".into(),
                cert_file: "cert.pem".into(),
                key_file: "key.pem".into(),
            },
            listeners: vec![
                ListenerConfig {
                    backend: Backend::Quinn,
                    bind: "127.0.0.1:7000".parse().expect("valid listener"),
                },
                ListenerConfig {
                    backend: Backend::S2n,
                    bind: "127.0.0.1:7000".parse().expect("valid listener"),
                },
            ],
        };
        assert!(validate_server(&value).is_err());
    }

    #[test]
    fn wildcard_listener_conflicts_with_specific_address() {
        let mut value = valid_server_config();
        value.listeners = vec![
            ListenerConfig {
                backend: Backend::Quinn,
                bind: "0.0.0.0:7000".parse().expect("wildcard listener"),
            },
            ListenerConfig {
                backend: Backend::S2n,
                bind: "127.0.0.1:7000".parse().expect("specific listener"),
            },
        ];
        assert!(validate_server(&value).is_err());
    }

    #[test]
    fn default_route_overlay_is_rejected() {
        let mut value = valid_server_config();
        value.server.overlay_cidr = "0.0.0.0/0".parse().expect("default route");
        assert!(validate_server(&value).is_err());
    }

    #[test]
    fn mtu_above_v1_transport_limit_is_rejected() {
        let mut value = valid_server_config();
        value.server.mtu = (MAX_OVERLAY_MTU + 1) as u16;
        assert!(validate_server(&value).is_err());
    }

    #[test]
    fn unsafe_agent_endpoint_and_server_name_are_rejected() {
        let mut value = AgentConfigFile {
            version: 1,
            agent: AgentSection {
                node_id: "edge-a".into(),
                server_addr: "0.0.0.0:7000".parse().expect("address"),
                backend: Backend::Quinn,
                server_name: " localhost ".into(),
                ca_file: "ca.pem".into(),
                token_file: "token".into(),
                tun_name: None,
            },
        };
        assert!(validate_agent(&value).is_err());
        value.agent.server_addr = "127.0.0.1:7000".parse().expect("address");
        assert!(validate_agent(&value).is_err());
        value.agent.server_name = "bad/name".into();
        assert!(validate_agent(&value).is_err());
        value.agent.server_name = "0.0.0.0".into();
        assert!(validate_agent(&value).is_err());
        value.agent.server_name = "localhost".into();
        value.agent.server_addr = "255.255.255.255:7000".parse().expect("broadcast");
        assert!(validate_agent(&value).is_err());
    }

    fn valid_server_config() -> ServerConfigFile {
        ServerConfigFile {
            version: 1,
            server: ServerSection {
                overlay_cidr: "10.88.0.0/24".parse().expect("network"),
                mtu: 1100,
                nodes_file: "nodes.toml".into(),
            },
            tls: TlsSection {
                server_name: "localhost".into(),
                cert_file: "cert.pem".into(),
                key_file: "key.pem".into(),
            },
            listeners: vec![ListenerConfig {
                backend: Backend::Quinn,
                bind: "127.0.0.1:7000".parse().expect("listener"),
            }],
        }
    }
}
