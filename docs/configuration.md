# 配置参考

Fusen Net 0.1 使用严格的版本化 TOML。未知字段、重复字段和无效组合会导致启动
失败。可以先运行：

```bash
fusen-net config check --config /etc/fusen-net/server.toml
fusen-net config check --config /etc/fusen-net/agent.toml
```

## 优先级和路径

最终值按以下顺序覆盖：

```text
安全默认值 < TOML < FUSEN_* 环境变量 < CLI 参数
```

运行命令用 `--config` 选择基线配置，并可通过下文列出的参数覆盖普通值和秘密
**文件路径**。不提供 `--token` 或明文秘密环境变量，秘密内容本身不能通过
环境变量或命令行传递。

TOML、环境变量和 CLI 中的相对资源路径均以配置文件所在目录为基准，不以当前
工作目录为基准。建议生产环境全部使用绝对路径。

## Server

```toml
version = 1

[server]
overlay_cidr = "10.88.0.0/24"
mtu = 1100
nodes_file = "nodes.example.toml"

[tls]
server_name = "localhost"
cert_file = "certs/server.pem"
key_file = "certs/server-key.pem"

[[listeners]]
backend = "quinn"
bind = "0.0.0.0:7000"

[[listeners]]
backend = "s2n"
bind = "0.0.0.0:7001"

[[listeners]]
backend = "gm-quic"
bind = "0.0.0.0:7002"
```

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `version` | 是 | 配置 schema 版本，0.1 只接受整数 `1` |
| `server.overlay_cidr` | 是 | 单个 IPv4 CIDR；不得包含节点注册地址冲突 |
| `server.mtu` | 否 | IPv4 包上限，默认 `1100`，v1 有效范围 576..=1100 |
| `server.nodes_file` | 是 | 静态节点注册表路径 |
| `tls.server_name` | 是 | Relay 对该证书提供服务的 SNI 名称；连接时必须出现在证书 SAN 中 |
| `tls.cert_file` | 是 | PEM 服务端证书链；叶证书在前，后接中间证书，通常不包含根证书 |
| `tls.key_file` | 是 | 与叶证书匹配的 PEM 私钥；匹配关系在 Server 启动时验证 |
| `listeners[].backend` | 是 | `quinn`、`s2n` 或 `gm-quic` |
| `listeners[].bind` | 是 | UDP SocketAddr；同一地址和端口不能重复 |

Server 至少需要一个 listener。配置了未编译进二进制的后端时必须启动失败，不能
回退到其他后端。

可覆盖的 Server 环境变量：

```text
FUSEN_CONFIG
FUSEN_OVERLAY_CIDR
FUSEN_MTU
FUSEN_NODES_FILE
FUSEN_CERT_FILE
FUSEN_KEY_FILE
FUSEN_SERVER_NAME
FUSEN_BACKEND
FUSEN_BIND
```

`FUSEN_BACKEND` 和 `FUSEN_BIND` 必须同时提供；二者会将配置文件中的 listener
列表整体替换为一个 listener。复杂的多 listener 配置请使用配置文件。对应 CLI
参数为 `--overlay-cidr`、`--mtu`、`--nodes-file`、`--cert-file`、`--key-file`、
`--server-name`、`--backend` 和 `--bind`，优先级高于同名环境变量。

## 节点注册表

```toml
version = 1

[[nodes]]
id = "edge-a"
ipv4 = "10.88.0.2"
token_sha256 = "sha256:<64 lowercase hex characters>"
enabled = true

[[nodes]]
id = "edge-b"
ipv4 = "10.88.0.3"
token_sha256 = "sha256:<64 lowercase hex characters>"
enabled = true
```

- `id`、`ipv4` 和 `token_sha256` 必须唯一；`enabled` 省略时默认为 `true`。
- 地址必须是 Server overlay CIDR 内可分配的单播 IPv4 地址，不能是网络地址或
  广播地址。
- `token_sha256` 是 token 文件内容的 SHA-256 摘要，不是明文 token。
- 重复在线 node ID 采用 `reject-new`，不会踢掉旧连接。

生成 token 时文件以安全权限原子创建；已存在的输出路径不会被覆盖：

```bash
fusen-net token generate --node-id edge-a --output /etc/fusen-net/edge-a.token
```

命令向终端输出 node ID 和可写入注册表的 SHA-256 摘要，不输出明文 token。
摘要是对 token 文件中去除末尾换行后的完整 `fsn1_...` 字符串计算 SHA-256。

## Agent

```toml
version = 1

[agent]
node_id = "edge-a"
server_addr = "203.0.113.10:7000"
backend = "quinn"
server_name = "relay.example.com"
ca_file = "certs/ca.pem"
token_file = "secrets/edge-a.token"
tun_name = "fusen0"
```

| 字段 | 必需 | 说明 |
| --- | --- | --- |
| `version` | 是 | 配置 schema 版本，必须为 `1` |
| `agent.node_id` | 是 | 与注册表完全一致，长度 1..=63，首字符为 ASCII 字母或数字 |
| `agent.server_addr` | 是 | Relay UDP `IP:port`；0.1 配置解析不接受主机名 |
| `agent.backend` | 是 | 必须匹配目标 listener 的后端 |
| `agent.server_name` | 是 | TLS SNI 和 SAN 校验名，不从 `server_addr` 猜测 |
| `agent.ca_file` | 是 | 含一个或多个受信任 CA 证书的 PEM bundle；没有 insecure 模式 |
| `agent.token_file` | 是 | 仅含生成器产生的 `fsn1_<base64url>` 值及末尾换行 |
| `agent.tun_name` | 否 | 请求的平台接口名；平台不支持固定名称时可忽略并记录实际名称 |

可覆盖的 Agent 环境变量：

```text
FUSEN_CONFIG
FUSEN_NODE_ID
FUSEN_SERVER_ADDR
FUSEN_BACKEND
FUSEN_SERVER_NAME
FUSEN_CA_FILE
FUSEN_TOKEN_FILE
FUSEN_TUN_NAME
```

不存在 `FUSEN_TOKEN` 或 `--token`。对应 CLI 参数为 `--node-id`、
`--server-addr`、`--backend`、`--server-name`、`--ca-file`、`--token-file` 和
`--tun-name`。日志级别使用 `RUST_LOG`，但不得通过
trace 日志泄露控制 payload 或包体。

## 文件权限

在 Unix 上，token 文件和 TLS 私钥不能给 group/other 任何权限，通常应为
`0600`；不安全权限必须导致 `config check` 和启动失败。证书、CA 和仅含摘要的
节点注册表可以按部署需要设为 `0644` 或更严格。

Windows 上的 token 和私钥 ACL 只允许文件所有者、Administrators 和 SYSTEM。
`config check` 和启动过程通过 Windows PowerShell 读取 ACL；所有者不属于这些
主体，存在授权给其他主体的 Allow 规则，或 ACL 无法读取时，配置直接失败。

## 校验规则

`config check` 检查 TOML schema、路径解析、文件存在性、平台秘密文件权限、CIDR、
MTU、节点唯一性、token 摘要格式、listener 冲突以及所选后端是否编译。对于证书、
CA 和私钥，它只确认普通文件中存在相应的 PEM BEGIN 标记；不会完整解析 PEM/DER、
验证证书链、有效期、私钥匹配或 SAN。

三个后端在创建 endpoint 时读取完整 `cert_file` 链和 `ca_file` bundle，并解析证书及
私钥；Server 证书/私钥不匹配会使启动失败。证书链信任、有效期以及 Agent
`server_name` 对 SAN 的校验发生在 TLS 握手时。因此 `config check` 成功不等于 TLS
配置可用。该命令不建立网络连接、创建 TUN 或修改路由，成功时只打印配置类型和
路径，不打印秘密内容。
