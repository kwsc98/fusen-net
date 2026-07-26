# Stellaris 线协议 v2

> **文档适用性：Current；适用范围：v2；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**

本文是 Stellaris `0.3.0-alpha.1` 唯一的线协议规范。关键字“必须”“不得”“应该”和
“可以”按 RFC 2119 的约定理解。v2 仍处于 alpha：在 `0.x` 期间可能发生破坏式
调整，但实现不得协商其他帧版本、ALPN 或降级路径。

## 传输与认证

所有链路使用 QUIC/TLS 1.3 和 Quinn。0-RTT 禁用；控制状态、身份材料和用户包不得
通过 early data 发送。四种用途由互不相同的 ALPN 隔离：

| 链路 | ALPN | TLS 身份 | 可靠流上的握手 |
| --- | --- | --- | --- |
| enrollment | `stellaris/enroll/2` | Agent 验证部署 service certificate | `EnrollRequest`, `EnrollAccepted`, `Error` |
| control | `stellaris/control/2` | Agent 验证 service certificate；Server 验证节点证书 | `ControlWelcome`、候选、连接计划、续期、撤销 |
| Relay | `stellaris/relay/2` | Agent 验证 service certificate；Server 验证节点证书 | `RelayBind`, `RelayAccepted`, `RelayReady`, `Error` |
| P2P | `stellaris/p2p/2` | 双方使用节点 CA 完成 mTLS | `P2pHello`, `P2pReady`, `Error` |

部署 service certificate 和节点 CA 必须分离。control/Relay TLS 握手提取的节点
证书身份还必须与静态注册表、当前授权 SPKI、用途和有效期一致。P2P 双方必须验证
节点 CA、签名 node ID/overlay IP、证书期限和协调方 descriptor 指纹。应用消息不能
声明或覆盖 TLS 已认证身份。

ALPN 不匹配、缺少要求的 peer certificate、Datagram 不可用或最大 Datagram 小于
协商 MTU 时必须关闭连接，不能改用可靠流承载用户包。

## 固定帧

四类握手都在一条双向 QUIC stream 上使用相同的 12-byte 头。多字节整数使用网络
字节序：

```text
0               4       6       7       8              12
+---------------+-------+-------+-------+----------------+
| magic "STLR"  | ver   | type  | flags | payload_length |
+---------------+-------+-------+-------+----------------+
   4 bytes        u16     u8      u8          u32
```

- `magic` 必须为 ASCII `STLR`，`ver` 必须为 `2`，`flags` 必须为 `0`。
- `payload_length` 不包含头；control/enrollment 最大 16384 bytes，Relay/P2P 握手
  最大 4096 bytes。实现必须先检查长度再分配或读取 payload。
- payload 是 UTF-8 JSON 对象。未知字段、重复字段、错误类型、错误方向和语义非法值
  必须拒绝。
- 实现必须处理流分片和连续帧；解析失败不得 panic，也不得记录原始 payload。

规范小写带连字符的 UUID v4 用作 enrollment、session 和 connection ID。请求 ID、
incarnation 和非空候选对应的 epoch 都是非零 `u64`。证书指纹是叶证书 DER 的
`sha256:` 加 64 个小写十六进制字符。DER 字段使用规范 base64，不接受等价的非规范
编码。单个 CSR 最大 4096 decoded bytes，单张证书最大 6144 decoded bytes，证书链
为 1 至 3 张。

### JSON 字段规范

下表冻结每种 payload 的完整字段集合。除 `Error.request_id` 外，字段都必须出现；
`Error.request_id` 可以省略或为 `null`。表中未列出的字段必须拒绝。`Candidate`、
`PeerDescriptor` 等嵌套对象同样不得包含额外字段。

| 对象 | 完整字段（JSON 类型） |
| --- | --- |
| `Candidate` | `address` (SocketAddr string), `kind` (`"host"` or `"server_reflexive"`), `priority` (`u32`) |
| `EnrollRequest` | `enrollment_id` (UUID v4 string), `node_id` (string), `enrollment_token` (string), `csr_der_base64` (base64 string) |
| `EnrollAccepted` | `enrollment_id` (UUID v4 string), `node_id` (string), `overlay_ip` (IPv4 string), `overlay_cidr` (IPv4 CIDR string), `mtu` (`u16`), `certificate_chain_der_base64` (base64 string array), `node_ca_certificate_der_base64` (base64 string), `not_before_unix_seconds` (`u64`), `not_after_unix_seconds` (`u64`) |
| `ControlWelcome` | `session_id` (UUID v4 string), `incarnation` (`u64`), `overlay_ip` (IPv4 string), `overlay_cidr` (IPv4 CIDR string), `mtu` (`u16`), `certificate_not_after_unix_seconds` (`u64`), `coordinator_time_unix_seconds` (`u64`) |
| `AnnounceCandidates` | `epoch` (`u64`), `candidates` (`Candidate[]`) |
| `LookupPeer` | `request_id` (`u64`), `overlay_ip` (IPv4 string) |
| `PeerRecord` | `request_id` (`u64`), `node_id` (string), `overlay_ip` (IPv4 string), `incarnation` (`u64`), `session_id` (UUID v4 string), `certificate_fingerprint` (string), `certificate_not_after_unix_seconds` (`u64`), `epoch` (`u64`), `candidates` (`Candidate[]`) |
| `PeerDescriptor` | `node_id` (string), `overlay_ip` (IPv4 string), `incarnation` (`u64`), `session_id` (UUID v4 string), `certificate_fingerprint` (string), `certificate_not_after_unix_seconds` (`u64`), `candidate_epoch` (`u64`), `candidates` (`Candidate[]`) |
| `ConnectRequest` | `request_id` (`u64`), `overlay_ip` (IPv4 string) |
| `ConnectPlan` | `request_id` (`u64`), `connection_id` (UUID v4 string), `role` (`"initiator"` or `"responder"`), `peer` (`PeerDescriptor`), `expires_at_unix_seconds` (`u64`) |
| `RenewCertificate` | `request_id` (`u64`), `csr_der_base64` (base64 string) |
| `CertificateIssued` | `request_id` (`u64`), `certificate_chain_der_base64` (base64 string array), `not_before_unix_seconds` (`u64`), `not_after_unix_seconds` (`u64`) |
| `PeerRevoked` | `node_id` (string), `overlay_ip` (IPv4 string), `epoch` (`u64`) |
| `Error` | optional `request_id` (`u64` or `null`), `code` (error-code string), `message` (string), `retryable` (boolean) |
| `RelayBind` | `control_session_id` (UUID v4 string), `incarnation` (`u64`) |
| `RelayAccepted` | `relay_session_id` (UUID v4 string), `mtu` (`u16`), `max_datagram_size` (`u16`) |
| `RelayReady` | `relay_session_id` (UUID v4 string) |
| `P2pHello` | `connection_id` (UUID v4 string), `control_session_id` (UUID v4 string), `incarnation` (`u64`), `certificate_fingerprint` (string) |
| `P2pReady` | `connection_id` (UUID v4 string), `mtu` (`u16`) |

字段的数值范围、相互约束和状态机约束由下文对应章节定义。实现增加、删除、重命名
字段或改变类型时，必须先以破坏式协议设计变更更新本表；Rust 类型本身不能替代本规范。

## Enrollment

消息 type：

| 值 | 名称 | 方向 |
| --- | --- | --- |
| `0x01` | `EnrollRequest` | Agent -> Server |
| `0x02` | `EnrollAccepted` | Server -> Agent |
| `0xff` | `Error` | 双向 |

`EnrollRequest`：

```json
{
  "enrollment_id":"550e8400-e29b-41d4-a716-446655440000",
  "node_id":"edge-a",
  "enrollment_token":"stl2_<base64url-no-pad>",
  "csr_der_base64":"<base64 DER>"
}
```

`enrollment_token` 的随机部分解码后必须恰为 32 bytes。CSR 必须证明 Agent 持有其
私钥。Server 从静态注册表取得 node ID 和 overlay IP，覆盖 CSR 中任何身份、用途或
CA 请求；私钥不得上传。

`EnrollAccepted`：

```json
{
  "enrollment_id":"550e8400-e29b-41d4-a716-446655440000",
  "node_id":"edge-a",
  "overlay_ip":"10.88.0.2",
  "overlay_cidr":"10.88.0.0/24",
  "mtu":1100,
  "certificate_chain_der_base64":["<leaf>","<node CA>"],
  "node_ca_certificate_der_base64":"<node CA>",
  "not_before_unix_seconds":1780000000,
  "not_after_unix_seconds":1780086400
}
```

Server 必须在持久提交已消费 token、授权 SPKI 和签发结果后才能返回成功。相同
node binding、enrollment ID、token 和完整请求的重试返回同一已提交结果；ID 相同但
内容不一致、token 已被其他请求消费或 token 不匹配时拒绝。认证失败响应不得区分
node 不存在与 token 错误，也不得泄露注册表或摘要。

## Control

control 连接完成 mTLS 和静态授权后，Server 首先发送 `ControlWelcome`。Agent 在
收到并验证它之前不得发送其他控制消息。

| 值 | 名称 | 方向 | 用途 |
| --- | --- | --- | --- |
| `0x01` | `AnnounceCandidates` | Agent -> Server | 发布当前 session 的 host candidates |
| `0x02` | `LookupPeer` | Agent -> Server | 查询 overlay IP 的在线记录 |
| `0x03` | `PeerRecord` | Server -> Agent | 返回认证身份和候选快照 |
| `0x04` | `PeerRevoked` | Server -> Agent | 使缓存和路径失效 |
| `0x05` | `Error` | 双向 | 结构化错误 |
| `0x06` | `ControlWelcome` | Server -> Agent | 建立 session 与权威网络参数 |
| `0x07` | `ConnectRequest` | Agent -> Server | 请求按目标地址建立 P2P |
| `0x08` | `ConnectPlan` | Server -> Agent | 向连接两端分配同一计划 |
| `0x09` | `RenewCertificate` | Agent -> Server | 在当前 mTLS session 上请求续期 |
| `0x0a` | `CertificateIssued` | Server -> Agent | 返回新的节点证书链 |

### Welcome 与新鲜度

```json
{
  "session_id":"550e8400-e29b-41d4-a716-446655440000",
  "incarnation":4,
  "overlay_ip":"10.88.0.2",
  "overlay_cidr":"10.88.0.0/24",
  "mtu":1100,
  "certificate_not_after_unix_seconds":1780086400,
  "coordinator_time_unix_seconds":1780000000
}
```

`incarnation` 是 Server 为 node 持久维护的单调代数。替代 control session 必须取得
更大值和新 session ID；旧 session 的事件不得更新或清理新 session。Server 在节点
证书 `NotAfter` 到达时主动关闭 control 和绑定的 Relay。

请求 ID 使用当前 control session 内的固定 64-bit 滑动重放窗口：窗口内未见过的
乱序值可以接受；重复值和落后最高值至少 64 的值拒绝。候选 `epoch` 在一个 session
内严格递增；新 incarnation 从 `epoch = 0`、空候选开始。

### Candidates 与目录

```json
{
  "epoch":1,
  "candidates":[
    {"address":"192.0.2.10:7100","kind":"host","priority":100}
  ]
}
```

每个列表最多 16 项，SocketAddr 不得重复。当前运行时只接受 IPv4 `host`：端口非零，
地址不能是 unspecified、loopback、multicast、broadcast 或 `0.0.0.0/8`。
`server_reflexive` 是保留枚举值，Agent 不得发布，本阶段路径管理器也不使用它。

v2 线格式不携带接口 provenance，也没有单独禁止候选落入 overlay CIDR。接收方不能
仅凭 `kind = "host"` 证明该地址来自真实 underlay；当前实现必须通过后续过滤修复和
真实路由门禁证明 LAN 直连语义。

`PeerRecord` 和 `PeerDescriptor` 由 Server 从同一认证 session 构造，包含 node ID、
overlay IP、incarnation、session ID、叶证书指纹、证书期限、候选 epoch 和候选列表。
Agent 必须先比较 incarnation，再在相同 incarnation/session 内比较 candidate epoch。
记录本身不是 P2P Ready 证明。

### Connection plan

`ConnectRequest` 包含非零 `request_id` 和目标 `overlay_ip`。目标在线且有候选时，
Server 生成一个 `connection_id`，向请求方发送 `role = "initiator"` 的计划，并向
目标发送 `role = "responder"` 的同一计划。每份 `ConnectPlan` 都包含对端
`PeerDescriptor` 和 `expires_at_unix_seconds`；期限不得晚于任一节点证书到期时间。

角色只定义握手计划。双方都可准备入站并拨号；重复连接由确定性规则仲裁，最终每个
目标只保留一个 Ready 路径。在 Ready 前数据继续走 Relay。

### Certificate renewal

`RenewCertificate` 只允许在当前已认证 control session 上发送，包含请求 ID 和新的
CSR DER。CSR 的 SPKI 必须等于当前认证节点证书的 SPKI。`CertificateIssued` 回显
请求 ID，返回证书链及准确的 `not_before`/`not_after`；不得借续期改变 node ID、
overlay IP、公钥或节点 CA。

节点证书当前有效期为 24 小时。Agent 在约半个有效期、最多正负 30 分钟抖动时续期。
新证书必须安全安装并用于替代 control/Relay 会话；旧证书到期后既有连接必须关闭。

### Revocation 与 Error

`PeerRevoked` 包含 node ID、overlay IP 和独立的单调撤销 epoch。它不能和 candidate
epoch 比较。收到更新撤销时，Agent 必须清理 descriptor、停止新探测、关闭匹配 P2P
并使后续包回退 Relay；Server 必须拒绝该节点的新认证会话。

`Error` 格式：

```json
{"request_id":42,"code":"peer_not_found","message":"peer is not online","retryable":true}
```

`request_id` 可省略，存在时非零。`message` 为 1..=512 bytes 且不含控制字符、秘密、
内部路径或完整请求。错误码为 `invalid_request`、`protocol_violation`、
`replay_detected`、`peer_not_found`、`revoked`、`server_busy` 和 `internal`。致命认证、
方向或帧错误必须关闭连接；`retryable` 不能绕过接收方本地退避和授权策略。

## Relay

消息 type：

| 值 | 名称 | 方向 |
| --- | --- | --- |
| `0x01` | `RelayBind` | Agent -> Server |
| `0x02` | `RelayAccepted` | Server -> Agent |
| `0x03` | `RelayReady` | 双向 |
| `0xff` | `Error` | 双向 |

Agent 首先发送当前 `control_session_id` 和 `incarnation`。Server 必须确认 TLS 节点
身份与该 control lease 完全一致，再返回新的 `relay_session_id`、`mtu` 和
`max_datagram_size`。Agent 发送含该 session ID 的 `RelayReady`；Server 必须先原子
安装 Ready 路由，再回显完全相同的 `RelayReady` 作为提交确认。Agent 收到并验证该
确认后，才能把替代 Relay 视为可用并关闭旧 control/Relay。Server 在提交前不得通过
新 Relay 路由转发任何 Datagram。

Ready 后，每个 QUIC Datagram 恰好承载一个原始 IPv4 包，不增加 Stellaris 封装。
Server 校验完整长度、MTU、源地址等于认证节点地址，以及目标是 overlay 内另一个
Ready 节点。Datagram 允许丢包和乱序，不在控制 stream 上重传用户包。control lease
结束、证书到期或 session 被替代时 Relay route 立即删除；旧清理事件只能删除自己
拥有的 session。

## P2P

消息 type：

| 值 | 名称 | 方向 |
| --- | --- | --- |
| `0x01` | `P2pHello` | Initiator -> Responder |
| `0x02` | `P2pReady` | Responder -> Initiator |
| `0xff` | `Error` | 双向 |

发起方的 `P2pHello` 携带 plan `connection_id`，以及自身的 control session ID、
incarnation 和证书指纹。响应方必须把 TLS 叶证书与收到的对端 descriptor 逐项比对，
并只对未过期的当前 plan 返回包含同一 connection ID 和 MTU 的 `P2pReady`。发起方
同样验证响应方证书和 descriptor 后才可标记 Ready。

Ready 后 Datagram 直接承载原始 IPv4 包。接收方必须要求：源地址等于已认证 peer
overlay IP，目标地址等于本机 overlay IP，包完整且不超过 MTU。任何身份、plan、
candidate、证书期限或路径 generation 变化都会使连接失效。

每个出站包只选择 Relay 或 P2P 之一。P2P 发送失败时当前包丢弃，连接转为失效；后续
包选择 Relay。禁止将失败的同一个包再次投递到 Relay。默认五分钟无应用数据使用后
回收 P2P，传输 keepalive 不算应用数据。

## 当前范围

本规范只定义静态 IPv4 和 host-candidate LAN P2P。NAT 打洞、地址观察、
server-reflexive candidate、STUN/TURN、HA、Relay 路径端到端加密、ACL、多租户、
IPv6 和其他 QUIC 后端需要后续协议/ADR，不能隐式加入当前线协议。

实现存在不等于门禁完成。协议恶意输入、持久化故障注入、Relay/P2P E2E、Linux
真实 TUN 和资源 soak 的剩余状态见
[`distributed-network-plan.md`](distributed-network-plan.md)。
