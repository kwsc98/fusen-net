# 配置参考

> **文档适用性：Current；适用范围：v2；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**

Stellaris `0.3.0-alpha.1` 只接受严格的 schema v2 TOML。Server、Agent 和静态节点表
顶层都必须是 `version = 2`；未知字段、重复字段、缺失 section 和其他版本直接拒绝。

```bash
stellaris config check --config /etc/stellaris/server.toml
stellaris config check --config /etc/stellaris/agent.toml
stellaris config check --config /etc/stellaris/nodes.toml
```

运行命令只支持 `--config PATH`，也可设置 `STELLARIS_CONFIG`。没有字段级 CLI 或
环境变量覆盖，没有 backend 选择，也没有明文 token 参数。相对资源路径一律以配置
文件所在目录为基准。

## Server

```toml
version = 2

[network]
overlay_cidr = "10.88.0.0/24"
mtu = 1100

[registry]
nodes_file = "nodes.toml"

[storage]
coordinator_state_file = "state/coordinator-state.json"

[tls]
server_name = "stellaris.example.com"
service_cert_file = "certs/deployment-server.pem"
service_key_file = "certs/deployment-server-key.pem"
node_ca_cert_file = "state/node-ca.pem"
node_ca_key_file = "state/node-ca-key.pem"

[listeners]
enrollment = "0.0.0.0:7000"
control = "0.0.0.0:7001"
relay = "0.0.0.0:7002"

[limits]
max_nodes = 256
max_connections_per_ip = 8
max_pending_handshakes = 64
queue_capacity = 256

# [observability]
# metrics_bind = "127.0.0.1:9100"
```

### Server 字段

| 字段 | 必需 | 规则 |
| --- | --- | --- |
| `version` | 是 | 固定为整数 `2` |
| `network.overlay_cidr` | 是 | IPv4 CIDR，prefix 为 `/8` 至 `/30` |
| `network.mtu` | 否 | 默认 `1100`，范围 `576..=1100` |
| `registry.nodes_file` | 是 | schema v2 静态节点表；只在启动时加载 |
| `storage.coordinator_state_file` | 是 | `server init` 创建的持久协调状态 |
| `tls.server_name` | 是 | service certificate 的 TLS 名称；ASCII DNS 名或规范 IPv4 单播地址 |
| `tls.service_cert_file` | 是 | 部署服务证书 PEM；叶证书在前 |
| `tls.service_key_file` | 是 | 与服务证书匹配的私钥 PEM |
| `tls.node_ca_cert_file` | 是 | `server init` 创建的节点 CA 证书 |
| `tls.node_ca_key_file` | 是 | `server init` 创建的节点 CA 私钥 |
| `listeners.enrollment` | 是 | 仅注册协议的 IPv4 UDP bind |
| `listeners.control` | 是 | 节点 mTLS 控制协议的 IPv4 UDP bind |
| `listeners.relay` | 是 | 节点 mTLS Relay 协议的 IPv4 UDP bind |
| `limits.max_nodes` | 否 | 默认/最大 `256`，必须非零且覆盖节点表全部条目 |
| `limits.max_connections_per_ip` | 否 | 默认 `8`；运行时必须在 `1..=max_nodes`；按 remote IP 和 listener 用途分别计数，control/Relay 为证书续期重叠各允许最多两倍配置值 |
| `limits.max_pending_handshakes` | 否 | 默认 `64`；配置范围 `2..=1024`；当前同时用作并发 enrollment 上限和每个 control session 的事件队列容量，不是所有 QUIC 握手的统一上限 |
| `limits.queue_capacity` | 否 | 默认 `256`，必须在 `1..=max_nodes` |
| `observability.metrics_bind` | 否 | 可选 Prometheus HTTP `/metrics` IPv4 bind；不得暴露到不可信网络，exporter 尚未通过发布门禁 |

三个 listener 必须使用非零端口，且 bind 不得冲突。相同 IP/端口冲突；通配地址与
同端口的具体地址也冲突。它们都是 Quinn UDP endpoint，不表示三种传输后端。

多个 Agent 共用一个 NAT 出口 IP 时会共享相应用途的 per-IP 额度。enrollment、control
和 Relay 分开计数；容量规划必须包含续期时旧/新 control 与 Relay 的短暂重叠。

`service_cert_file`/`service_key_file` 属于部署 TLS 身份，用在全部三个 Server
listener。enrollment 只要求客户端验证 Server；control 和 Relay 还要求客户端出示
节点 CA 签发的有效证书。节点 CA 不是部署服务证书的签发 CA。

`tls.server_name` 与 `coordinator.server_name` 使用同一严格规则。DNS 名总长为
1..=253 个 ASCII bytes，每个 label 为 1..=63 bytes，只能包含 ASCII 字母、数字和内部
连字符；末级 label 至少包含一个字母，且不接受尾随点。
`01.02.03.04`、`999.999.999.999` 等 dotted-decimal 输入不会退回按 DNS 名处理；IPv4
必须使用规范十进制写法且不能是 unspecified、multicast 或 broadcast 地址。

### 初始化与启动

```bash
stellaris server init --config /etc/stellaris/server.toml
stellaris server run --config /etc/stellaris/server.toml
```

`server init` 读取并验证 Server 配置、部署服务文件和节点表，然后创建
`node_ca_cert_file`、`node_ca_key_file` 与 `coordinator_state_file`。命令不覆盖也不
轮换现有状态。唯一可恢复的中断状态是：节点 CA certificate/key 同时存在且通过严格
证书、私钥、匹配关系和权限校验，而 `coordinator_state_file` 缺失；重试会保留该 CA
并创建空协调状态。其他单文件、损坏、不匹配、权限不安全或已有协调状态的组合均拒绝。

`server run` 不会隐式初始化。节点 CA、协调状态缺失、损坏、互不匹配或权限不安全时
必须失败。静态节点表只在启动时读取；修改 `enabled` 或轮换 token 后必须重启 Server。
地址只允许在节点首次 enrollment 前修改；已有持久 node/IP binding 时，原地改地址会
使 Server 启动 fail-closed。v2 没有地址重绑或迁移流程。本阶段没有配置 reload 或
管理 API。

## 静态节点表

```toml
version = 2

[[nodes]]
id = "edge-a"
ipv4 = "10.88.0.2"
enrollment_token_sha256 = "sha256:<64 lowercase hex characters>"
enabled = true

[[nodes]]
id = "edge-b"
ipv4 = "10.88.0.3"
enrollment_token_sha256 = "sha256:<64 lowercase hex characters>"
enabled = true
```

- 最多 256 项；`id`、`ipv4` 和 token 摘要分别唯一。地址数还受 overlay CIDR 的可用
  普通 host 限制：`/24` 通常最多 254 项，完整 256 项需要至少 `/23`。
- `id` 长度为 1..=63；首字符为 ASCII 字母或数字，后续还可使用 `.`, `_`, `-`。
- 地址必须是 Server overlay CIDR 内可用的单播 host，不能是网络或广播地址。
- `enabled` 省略时为 `true`。禁用项不能 enrollment 或建立新的 control/Relay 会话；
  它是可恢复的启动时准入开关，不等同于不可逆的 `PeerRevoked` tombstone。
- 表内只保存 enrollment token 的 `sha256:<hex>` 摘要，不保存明文。

生成 token：

```bash
stellaris token generate \
  --node-id edge-a --output /etc/stellaris/edge-a.token
```

命令用 CSPRNG 创建 32-byte 随机值，编码为 `stl2_<base64url-no-pad>`，以安全权限
创建并 fsync 文件，然后打印 `node_id` 和 `enrollment_token_sha256`。输出路径已存在
时拒绝覆盖。成功 enrollment 后 token 在协调状态中标记为已消费；它不是长期会话
凭据。

## Agent

```toml
version = 2

[agent]
node_id = "edge-a"
tun_name = "stellaris0"

[coordinator]
enrollment_addr = "203.0.113.10:7000"
control_addr = "203.0.113.10:7001"
relay_addr = "203.0.113.10:7002"
server_name = "stellaris.example.com"
deployment_ca_file = "certs/deployment-ca.pem"

[identity]
directory = "state/edge-a"
enrollment_token_file = "secrets/edge-a.token"

[p2p]
bind = "0.0.0.0:7100"
idle_timeout_secs = 300

# [observability]
# metrics_bind = "127.0.0.1:9101"
```

### Agent 字段

| 字段 | 必需 | 规则 |
| --- | --- | --- |
| `version` | 是 | 固定为整数 `2` |
| `agent.node_id` | 是 | 必须与静态节点表及签名证书身份完全一致 |
| `agent.tun_name` | 否 | schema 接受 1..=63 个非 NUL bytes；平台可能进一步限制或忽略名称，Linux 通常只有 15 个可用 bytes，`config check` 通过不保证内核接受 |
| `coordinator.enrollment_addr` | 是 | enrollment listener 的可达 IPv4 UDP 地址 |
| `coordinator.control_addr` | 是 | control listener 的可达 IPv4 UDP 地址 |
| `coordinator.relay_addr` | 是 | Relay listener 的可达 IPv4 UDP 地址 |
| `coordinator.server_name` | 是 | 三条 Server TLS 连接使用的 SNI/SAN 校验名；规则与 `tls.server_name` 相同 |
| `coordinator.deployment_ca_file` | 是 | 信任部署 service certificate 的 CA PEM bundle |
| `identity.directory` | 是 | 私有节点密钥、证书、节点 CA 和待完成请求的持久目录 |
| `identity.enrollment_token_file` | 首次注册时 | `stl2_` token 文件；已有有效 identity 时可省略；identity 丢失或证书过期到无法续期时，重新 enrollment 需提供新 token |
| `p2p.bind` | 是 | Quinn Hybrid endpoint 的 IPv4 UDP bind |
| `p2p.idle_timeout_secs` | 否 | 默认 `300`，运行时允许 `30..=3600` 秒 |
| `observability.metrics_bind` | 否 | 可选 Prometheus HTTP `/metrics` IPv4 bind；不得暴露到不可信网络，exporter 尚未通过发布门禁 |

Agent 配置不包含 overlay CIDR、overlay IP 或 MTU。首次 enrollment 和每次
`ControlWelcome` 都由协调服务提供权威网络值，Agent 将其与已安装签名身份交叉校验。

`identity.directory` 不存在时由 Agent 安全创建其最后一级目录，但父目录必须预先存在
且可由 Agent effective UID 访问；Agent 不递归创建缺失的父目录。随后在本地生成 P-256
私钥，私钥不会上传。enrollment 成功后原子安装节点证书和节点 CA。证书续期必须沿用
同一公钥，新的 control/Relay 会话建立后替换旧会话；P2P socket 不重新绑定。

## 文件权限

Unix 上，service 私钥、节点 CA 私钥、协调状态、enrollment token 和 Agent identity
目录必须由当前 effective UID 所有，并且 group/other 无任何权限。通常秘密文件为
`0600`、identity 目录为 `0700`。证书、部署 CA 和只含摘要的节点表可以按部署需要
使用 `0644` 或更严格权限。

因此 `config check` 与随后读取同一秘密的 `server init|run` 或 `agent run` 必须使用
同一 effective UID。先用普通用户校验、再用 `sudo` 切换为 root 运行通常会因所有者
不匹配而失败。

Windows 上的秘密 ACL 只允许所有者、Administrators 和 SYSTEM；无法读取 ACL 或
出现其他 Allow 主体时校验失败。macOS/Windows 当前仍是编译通过、运行未验证状态，
不能把权限检查存在解释为平台发布支持。

## `config check` 边界

`config check` 校验 TOML 类型、版本、未知字段、路径解析、文件存在性、基础 PEM
marker、token 格式、静态节点唯一性、CIDR/MTU、listener 冲突和受支持的秘密权限。
单独检查 `nodes.toml` 时没有 Server overlay 上下文，只校验格式、唯一性和基础单播；
检查 `server.toml` 时才会加载节点表并验证每个地址属于该 Server 的 overlay CIDR。
它不会：

- 创建或修改节点 CA、协调状态、Agent identity、TUN 或路由；
- 建立网络连接；
- 完整验证证书链、有效期、SAN 或证书/私钥匹配；
- 证明 UDP 可达、P2P 可达或真实 TUN 门禁已通过。

因此校验成功只说明输入结构可接受，不等于运行时 TLS 和网络可用。

NodeUID、动态地址池、分层节点 CA 和 v3 配置只在
[`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 中
作为 Proposed 方向定义。它们尚未加入本 v2 schema，不能提前写入配置。
