# Stellaris 单一身份、信任、动态地址与 Relay 闭环改造计划

> **文档适用性：Proposed；适用范围：候选 v3 `V3-R1 Identity & Relay Core`；
> 设计评审状态：Draft；ADR 决策状态：Proposed；交付状态：Not started；验证状态：
> Unverified；发布状态：Unreleased。**
> 目标版本：`0.4.0-alpha.1`。
> 方案快照：2026-07-26。
> 设计批准：N/A。
> 关联 ADR：docs/adr/0004-durable-node-identity-and-address-leases.md。
> 最后核对：working tree（2026-07-26）。

本文定义下一次破坏式身份、信任、地址和数据路径改造的唯一候选契约。当前运行行为仍由
`architecture.md`、`protocol.md`、`configuration.md` 和 `security-model.md` 描述；
本文中的接口在 ADR 0004 Accepted、本文 Approved 且实现合入前均不是当前能力。

## 问题与证据

当前实现把可读 `node_id` 同时用作名称和长期安全身份，在静态节点表中预先分配 IPv4，
并让在线 Server 直接持有自签节点 CA 私钥。当前事实及限制分别见
[架构](architecture.md)、[线协议](protocol.md)、[配置](configuration.md)、
[安全模型](security-model.md) 和 [v2 验收计划](distributed-network-plan.md)。

该模型无法独立表达改名、长期 principal、安全换钥、动态地址分配、地址释放隔离、
分层 CA、签发者轮换和可信快照恢复。下一阶段不延续任何历史协议或状态语义，也不把
迁移、兼容或双栈作为设计输入，而是一次建立并切换到唯一的新模型。

## 当前行为

当前代码仍运行 `0.3.0-alpha.1` v2：

- Server 使用静态节点表建立 node ID 与 overlay IPv4 绑定；
- Agent 配置包含 node ID，首次运行可以自动消费 token；
- enrollment、control、Relay 和 P2P 使用四条 `/2` ALPN；
- Server 在线持有单层节点 CA，节点 leaf 有效期为 24 小时；
- Relay-first、按需 P2P 和单路径选择已经接入，但发布门禁尚未完成；
- 当前持久状态、配置、证书和线格式均不属于本提案的新模型。

这些事实只用于界定破坏式切换范围，不产生兼容要求，也不成为本阶段的验收前置条件。

## 目标与非目标

### 目标

`V3-R1` 结束时必须同时具备：

- 单组织、单信任域、单 Coordinator 和可信 Relay；
- `TrustDomainId + NodeUid` 组成的长期 principal；
- 独立的规范化 `NodeName` 和永久名称 tombstone；
- admission 驱动的显式注册、UUIDv7 NodeUid 和持久动态 IPv4 lease；
- Active、Disabled、Revoked、Released 完整生命周期；
- 改名、证书续期、Active 换钥、Revoked 恢复、释放和隔离后地址复用；
- 离线 Root、在线 Intermediate、Root 签名 issuer manifest 和严格 leaf profile；
- enrollment、control、Relay 三条唯一协议与显式 Agent enroll/re-enroll；
- 原子快照、外部 generation witness、备份恢复和 fail-closed recovery mode；
- 两个 Linux Agent 经真实 TUN 和 Relay 完成双向 ping、TCP、UDP。

### 非目标

本阶段不定义或保留：

- P2P listener、ALPN、candidate、descriptor、plan、Hello 或路径切换；
- NAT 穿透、server-reflexive candidate、STUN/TURN 或 Multipath QUIC；
- 多 Coordinator、HA、Raft/BFT 或在线远程管理 API；
- 多信任域、ACL、多租户、IPv6、DNS、默认路由或子网路由；
- Relay 路径端到端加密或隐藏 Relay 元数据；
- TPM、Secure Enclave、attestation 或 transparency log；
- s2n-quic、gm-quic 或其他 QUIC runtime 的运行支持；
- 30 分钟 soak、256 个在线 Agent 或 macOS/Windows 原生运行支持；
- 历史配置、状态、证书、token、Agent 或线协议的迁移和兼容。

## 公共接口和版本边界

### 单一版本

本阶段只定义：

- Cargo 版本 `0.4.0-alpha.1`；
- 配置 schema `version = 3`；
- 协调状态 schema `version = 3`；
- Agent identity schema `version = 3`；
- 固定帧 `version = 3`；
- `stellaris/enroll/3`、`stellaris/control/3`、`stellaris/relay/3` 三条 ALPN。

实现完成后仓库不得存在第二套公开 schema、codec、listener、配置解析器、兼容 feature
或运行时 adapter。任意非 3 version、未知 ALPN、未知消息和未声明字段按普通未知输入
fail-closed；不维护历史输入专用分支或 fixture。

### 核心类型

```rust
struct TrustDomainName(String);
struct TrustDomainId([u8; 32]);
struct NodeUid(Uuid);
struct NodeName(String);
struct AdmissionId(Uuid);
struct OperationId(Uuid);
struct ControlSessionId(Uuid);
struct RelaySessionId(Uuid);
struct Sha256Fingerprint([u8; 32]);
struct UnixSeconds(u64);

struct NodePrincipal {
    trust_domain_id: TrustDomainId,
    node_uid: NodeUid,
}

enum NodeLifecycle {
    Active,
    Disabled,
    Revoked,
    Released,
}

struct AuthorizationTuple {
    principal: NodePrincipal,
    overlay_ip: Ipv4Addr,
    lifecycle: NodeLifecycle,
    state_epoch: u64,
    allocation_epoch: u64,
    key_epoch: u64,
    revocation_epoch: u64,
    authorized_spki_sha256: Sha256Fingerprint,
}
```

`TrustDomainName` 只接受已经规范化的小写 ASCII DNS A-label：

- 总长度 1..=253 bytes，每个 label 1..=63 bytes；
- label 首尾为 `[a-z0-9]`，内部只允许 `[a-z0-9-]`；
- 不接受大写、Unicode、空 label、尾随点、前后空白或静默转换；
- 初始化后不可在同一状态目录中更改。

`NodeName` 是单个 DNS label，长度 1..=63 bytes，采用相同字符规则。名称在当前信任域
内唯一；改名后旧名称永久进入 tombstone，不能分给其他 NodeUid。

`TrustDomainId` 固定为：

```text
SHA-256("stellaris trust domain v3\0" || canonical DER Root SPKI)
```

Root SPKI 的 canonical DER 固定采用 RFC 5480 `id-ecPublicKey`、参数
`prime256v1` 和 65-byte 未压缩 SEC1 point；压缩 point、缺失或不同曲线参数以及等价但
不同的 SPKI 编码全部拒绝。`TrustDomainId` 的文本和 JSON 编码为 64 位小写十六进制，
DER 中为 32-byte OCTET STRING。

本文所有 Root、issuer 和 node SPKI 指纹都固定为 canonical DER SPKI 的 SHA-256；只有
明确写为 certificate、manifest、request、response、state 或 private-key exact-byte digest
的字段才覆盖对应对象的完整原始 bytes，二者不得混用。

`NodeUid`、`AdmissionId` 和 `OperationId` 均使用 UUIDv7，时间以生成时的 Unix
milliseconds 填充，随机位来自 OS CSPRNG；规范文本和 JSON 编码是小写连字符 UUID。
`ControlSessionId` 和 `RelaySessionId` 是 OS CSPRNG UUIDv4，只存在于进程内和线协议，
不进入持久 principal。所有 UUID 的 DER/内部定长编码才使用 RFC 4122 顺序的 16 bytes。
生成碰撞时在事务锁内最多重试 8 次，仍冲突则 fail-closed。

### Epoch 与生命周期

首次 enrollment 创建 Active 节点时：

- `name_epoch = 1`；
- `state_epoch = 1`；
- `key_epoch = 1`；
- `revocation_epoch = 0`。

`allocation_epoch` 是地址的跨 owner 单调分配代数：从选中 lease 的当前值 checked-increment
后同时写入 NodeRecord 和 lease。首次使用从 0 变为 1；隔离后复用旧地址时可以大于 1。

所有 epoch 使用 checked increment，溢出时拒绝操作且状态不变。允许的转换只有：

```text
Active <-> Disabled
Active | Disabled -> Revoked
Revoked --replacement enrollment--> Active
Revoked -> Released
```

| 操作 | 允许的原状态 | 目标状态和 epoch | 保留或清除 |
| --- | --- | --- | --- |
| initial enrollment | 尚无 NodeRecord | Active；name/state/key = 1，revocation = 0；allocation = 选中 lease +1 | 建立名称、SPKI 和 lease |
| rename | Active、Disabled、Revoked | lifecycle 不变；name +1 | 旧名称永久 tombstone；其他字段不变 |
| disable | Active | Disabled；state +1 | 保留名称、SPKI 和 lease；取消已有 Pending Replacement |
| enable | Disabled | Active；state +1 | 保留名称、SPKI 和 lease |
| revoke | Active、Disabled | Revoked；state +1、revocation +1 | 清除 SPKI，保留名称和 lease；取消已有 Pending Replacement |
| Active re-enroll | Active | Active；key +1 | 替换 SPKI，其他 epoch 和 lease 不变 |
| Revoked re-enroll | Revoked | Active；state +1、key +1 | 建立新 SPKI，revocation 不变，保留 lease |
| renewal | Active | Active；所有 epoch 不变 | 复用 SPKI、UID 和 lease |
| release | Revoked | Released；state +1 | 清除 SPKI、detach lease，取消已有 Pending Replacement，名称和 Node tombstone 永久保留 |

rename 到当前名称、disable Disabled、enable Active、revoke Revoked 和 release Released
返回无变化成功，不增加 generation 或任何 epoch。除此之外，表中未列出的源状态均返回
明确的 lifecycle error 且状态不变；特别是 Disabled 不得 re-enroll，Released 不得恢复、
改名或换钥。`name_epoch` 不进入 AuthorizationTuple、证书 claims、认证或路由缓存键。
rename 到非当前名称时还必须同时避开 `historical_names` 和每个 Pending Initial admission
的 `node_name`；冲突时状态不变，不能夺取或破坏 admission 的名称 reservation。
只有表中真正发生的 disable、revoke 或 release 转换才取消该 NodeUid 的 Pending
Replacement；对已经处于目标 lifecycle 的幂等重复操作不得取消转换后新创建的 recovery
admission。

### CLI

公开命令固定为：

```text
stellaris pki root init --trust-domain-name NAME \
  --cert-out FILE --key-out FILE
stellaris pki intermediate issue --root-cert FILE --root-key FILE \
  --cert-out FILE --key-out FILE
stellaris pki manifest issue --root-cert FILE --root-key FILE \
  --trust-domain-name NAME [--previous-manifest FILE] \
  --issuer-cert FILE [--issuer-cert FILE ...] --output FILE

stellaris server init --config PATH
stellaris server run --config PATH
stellaris server backup --config PATH --output DIR --witness-out FILE
stellaris server restore --config PATH --input DIR --witness FILE

stellaris admission create --config PATH --name NAME \
  [--fixed-ip IP] --token-out FILE [--ttl 24h]
stellaris admission replace --config PATH --node-uid UID \
  --token-out FILE [--ttl 24h]
stellaris admission list --config PATH
stellaris admission cancel --config PATH --admission-id ADMISSION_ID

stellaris node list --config PATH
stellaris node show --config PATH --node-uid UID
stellaris node rename --config PATH --node-uid UID --name NAME
stellaris node disable|enable --config PATH --node-uid UID
stellaris node revoke|release --config PATH --node-uid UID \
  --confirm-node-uid UID

stellaris agent enroll --config PATH --token-file FILE
stellaris agent re-enroll --config PATH --token-file FILE
stellaris agent run --config PATH

stellaris config check --config PATH
```

三个 PKI 命令不读取 Server/Agent 配置，也没有隐式默认路径。所有输出使用 create-new，
任一输出已存在、是 symlink 或无法以规定权限持久化时整条命令失败且不得覆盖：

- `pki root init` 校验规范 TrustDomainName，生成 10 年 Root certificate 和 P-256
  PKCS#8 private key；certificate 的 Subject 只用于运维展示，TrustDomainId 仍只由
  canonical Root SPKI 决定；
- `pki intermediate issue` 验证 Root certificate/key 匹配后生成新的 P-256 key，并签发
  90 天 Intermediate；
- `pki manifest issue` 验证所有 `--issuer-cert` 都由该 Root 签发，按 SPKI SHA-256 原始
  bytes 排序并拒绝重复。没有 `--previous-manifest` 时只能签发 epoch 1；存在时必须验证
  前一份 manifest 的 Root、域、签名和 bytes，再签发 `previous_epoch + 1`，不接受手工
  指定或跳跃 epoch；
- private key 输出为 Unix `0600`，certificate/manifest 输出为 `0644`，所有文件完成
  file fsync、rename 和 directory fsync 后命令才成功。Root 私钥永不进入 Server 配置、
  snapshot 或备份。

admission token 为 32 bytes OS CSPRNG，文本为 `stl3_` 加无填充 base64url。默认 TTL
24 小时，允许 1 小时至 7 天。Server 只存 SHA-256 digest；明文只写 create-new、Unix
`0600` 的 `--token-out`，不得进入 stdout、日志或状态快照。token 文件必须恰好包含规范
ASCII token bytes，不含 BOM、空白或尾随换行；Agent 读取整个文件并拒绝任何额外 bytes。
create/replace 必须在同一事务中建立 admission 与 token 绑定，不保留独立 token generate
命令。

所有 admission、node、backup、restore 命令，包括 list/show，都要求 Server 已停止并
取得 Coordinator store 独占锁；锁冲突立即失败，读命令也不得绕过锁读取变化中的
snapshot。Agent enroll、re-enroll 和 run 共用 identity 独占锁，不能并行。

### 配置 schema

Server TOML 只允许：

```toml
version = 3

[trust_domain]
name = "example.internal"
root_cert_file = "pki/node-root.pem"
intermediate_cert_file = "pki/node-intermediate.pem"
intermediate_key_file = "pki/node-intermediate-key.pem"
issuer_manifest_file = "pki/issuer-manifest.der"

[network]
overlay_cidr = "10.88.0.0/23"
reserved_ipv4 = ["10.88.0.1"]
mtu = 1100

[storage]
coordinator_state_file = "state/coordinator-v3.json"
witness_file = "/separate-witness/stellaris-v3.json"

[service_tls]
server_name = "stellaris.example.internal"
certificate_file = "certs/deployment-server.pem"
private_key_file = "certs/deployment-server-key.pem"

[listeners]
enrollment = "0.0.0.0:7000"
control = "0.0.0.0:7001"
relay = "0.0.0.0:7002"

[limits]
max_nodes = 256
max_connections_per_ip = 8
max_pending_handshakes = 64
queue_capacity = 256
```

`[observability] metrics_bind` 保持可选。`reserved_ipv4` 默认为空；`mtu` 固定校验
576..=1100；`max_nodes` 为 1..=256 且不得超过扣除 network、broadcast 和 reserved
后的实际可用地址数。信任域、overlay、reserved set、MTU 和 max_nodes 在 Server init
后写入 snapshot，后续配置必须精确匹配，不能通过改配置静默改变。

Agent TOML 只允许：

```toml
version = 3

[agent]
tun_name = "stellaris0"

[trust_domain]
name = "example.internal"
root_cert_file = "pki/node-root.pem"

[coordinator]
enrollment_addr = "server.example.internal:7000"
control_addr = "server.example.internal:7001"
relay_addr = "server.example.internal:7002"
server_name = "stellaris.example.internal"
deployment_ca_file = "certs/deployment-ca.pem"

[identity]
directory = "state/agent"
```

`[observability] metrics_bind` 保持可选。Agent 只预置 Root certificate 和期望的
TrustDomainName，不预置 issuer manifest。Agent 配置不包含 NodeName、NodeUid、overlay
IP、token 或 P2P 字段。`agent enroll` 固定发送 `intent = "initial"`，且创建新 staged
operation 前 identity 必须不存在；它通过 `ManifestRequest` 取得并验证首份 manifest 后创建
identity。`agent re-enroll` 固定发送 `intent = "replacement"`：identity 存在时，只有经
Root 和 manifest 验证的响应 NodeUid 与现有 principal 相同才允许原子替换；identity 因
丢失而不存在时，由签名响应建立 admission 所绑定的 NodeUid。两条命令都不得覆盖不相关
的现有 identity。
`agent run` 只加载完整 identity，可以刷新公开 manifest，但缺失 identity 或存在未完成
staged operation 时 fail-closed，绝不自动 enrollment 或 rekey。

所有 TOML struct 使用 `deny_unknown_fields`。相对路径以配置文件目录为基准；配置、
私钥、state、witness、manifest 和 identity 路径拒绝 symlink。Unix 私钥、token、state、
witness 和 identity 文件要求 `0600`，包含秘密的目录要求 `0700`；公开 Root、certificate
和 manifest 可以是 `0644`，但不得 group/world writable。

## 身份、安全与信任边界

### 信任边界

本阶段信任部署管理员、单一 Coordinator、在线 Intermediate 和 Relay。部署 service TLS
保护 enrollment endpoint 并提供 Server 身份；Node Root/Intermediate 只负责节点身份。
Relay 可以看到转发的 IPv4 包、源/目标、长度和时序。本方案不抵御恶意 Coordinator，不
向 Relay 隐藏流量，也不提供跨组织隔离。

Agent 必须预置 Root certificate 和期望的 TrustDomainName；不能把 enrollment Server
下发的新 Root 当作信任锚。Root 私钥离线保存；Server 只加载 Root certificate、当前
Intermediate certificate/key 和 Root 签名 manifest。

### PKI profile

Root、Intermediate、node key 和 manifest signature 只允许 P-256 /
ECDSA-SHA256，不做算法协商：

| 对象 | 固定 profile 上限 | 必需约束 |
| --- | --- | --- |
| Root | 10 年 | CA=true，pathLen=1，keyCertSign/cRLSign |
| Intermediate | 90 天 | CA=true，pathLen=0，keyCertSign/cRLSign |
| issuer manifest | 30 天 | Root 签名，1..=4 个 active issuer |
| node leaf | 24 小时 | CA=false，DigitalSignature，ClientAuth/ServerAuth |

统一允许时钟偏差 5 分钟。签发工具使用 `NotBefore = observed_now - 5 minutes`，并使
`NotAfter - NotBefore` 等于表中期限；父证书剩余期限不足时拒绝，不缩短后静默签发。
GeneralizedTime 精确到 UTC seconds，不接受小数秒或本地时区。Root SPKI 改变即创建新的
TrustDomainId，不能作为域内轮换。Intermediate 可以重叠轮换；manifest active issuer
指纹按原始 32 bytes 字典序排序，拒绝重复项。

leaf 的完整 `[NotBefore, NotAfter]` 必须同时落在 signing Intermediate 和签发时 manifest
的有效区间内；剩余区间不足完整 24 小时时拒绝签发，不静默缩短。Server 启动时要求当前
manifest 在 5 分钟偏差内有效，并在 `manifest.NotAfter + 5 minutes` 前完成更高 epoch 的
停服激活；到达该时刻仍未更新时关闭全部 Control/Relay、停止签发和路由并退出，不能用
过期 active issuer set 继续服务。

node leaf 使用：

- 空 Subject；
- 恰好一个 URI SAN：
  `urn:stellaris:node:v3:<trust-domain-id-hex>:<lowercase-hyphenated-uuid>`；
- 恰好一个当前 overlay IPv4 SAN；
- 当前授权 P-256 SPKI；
- ClientAuth 与 ServerAuth EKU；
- critical Stellaris claims extension。

空 Subject 使 SAN extension 必须为 critical；BasicConstraints、KeyUsage 和 claims 也
必须 critical，EKU 必须 non-critical。ServerAuth 是本阶段冻结的 leaf profile 字段，
但 Relay-only runtime 没有节点侧 TLS server、P2P listener、P2P ALPN 或任何消费该用途
的代码路径；它不能作为隐藏 P2P surface 或支持声明。

claims OID 固定为：

```text
2.25.183780856908411626329710048056624933369
```

strict DER payload 固定为：

```text
StellarisNodeClaims ::= SEQUENCE {
  schemaVersion      INTEGER (1),
  trustDomainId      OCTET STRING (SIZE(32)),
  manifestEpoch      INTEGER (1..MAX_U64),
  allocationEpoch    INTEGER (1..MAX_U64),
  keyEpoch           INTEGER (1..MAX_U64),
  revocationEpoch    INTEGER (0..MAX_U64)
}
```

字段顺序固定，INTEGER 必须最小 DER 编码，禁止 trailing data。未知 schema、缺失/重复
extension、非 critical extension、额外 URI/IP SAN、错误 EKU/算法/期限均拒绝。

issuer manifest 使用严格 DER envelope：

```text
IssuerManifest ::= SEQUENCE {
  tbsManifest         TbsIssuerManifest,
  signatureAlgorithm  AlgorithmIdentifier, -- ecdsa-with-SHA256
  signatureValue      BIT STRING
}

TbsIssuerManifest ::= SEQUENCE {
  schemaVersion       INTEGER (1),
  trustDomainName     IA5String,
  trustDomainId       OCTET STRING (SIZE(32)),
  manifestEpoch       INTEGER (1..MAX_U64),
  notBefore           GeneralizedTime,
  notAfter            GeneralizedTime,
  activeIssuerSpkiSha256
                      SEQUENCE SIZE (1..4) OF OCTET STRING (SIZE(32))
}
```

`signatureAlgorithm` 固定为 `ecdsa-with-SHA256` OID 且 parameters 必须 absent；
`signatureValue` 的 unused-bits 为 0，内容是严格 DER `ECDSA-Sig-Value ::= SEQUENCE {
r INTEGER, s INTEGER }`。签名覆盖 `tbsManifest` 的完整 DER bytes。相同 epoch 只接受逐字节
相同 manifest。作为当前信任状态输入时，更高 epoch 必须验证 Root 签名、域、期限和有序
issuer set，并在使用前持久化；低 epoch 或同 epoch 分叉拒绝。exact response 内嵌的历史
manifest 只服从下文不降级验证规则。leaf 中的 manifest epoch 可以小于等于当前 epoch，
但实际签发
Intermediate 必须仍在当前 active set；issuer 一旦移除，其 leaf 无条件拒绝。

PKCS#10 CSR profile 固定为空 Subject、恰好一个 canonical P-256 SPKI、空 attributes 集合
且不含 extensionRequest 或其他 requested extension，并使用 ECDSA-SHA256 对完整
CertificationRequestInfo 完成 proof-of-possession。任何 SAN、NodeName、NodeUid、IP、
EKU、CA bit、自定义 claims、额外 attribute、非规范 SPKI 或其他签名算法都直接拒绝，
Server 不从 CSR 复制任何身份或扩展字段。

实现使用 `p256`、`der`、`const-oid` 完成 arbitrary-data ECDSA 和 DER；
`rcgen` 只负责 X.509 签发，`x509-parser` 负责结构解析和链检查。禁止使用
`SHA-256(message || public key)` 或空 signature 代替密码学签名。

### 认证结果

TLS 证书解析只能产生 `VerifiedNodeCertificate`。Leaf claims 有意不包含 lifecycle 和
`state_epoch`：Server 必须要求证书 principal、IP、SPKI、allocation/key/revocation epoch
与当前 TrustDomainRecord、NodeRecord 和 AddressLeaseRecord 相等，manifest epoch 不高于
当前 epoch 且实际 issuer 仍在当前 active set，并要求当前 lifecycle=Active；随后只从当前
NodeRecord 填充 AuthorizationTuple 的 lifecycle 和 `state_epoch`，生成
`AuthenticatedNode`。Control、Relay 和路由层只接受该类型，应用消息不能覆盖其中任何
字段。

TLS 握手时生成的 `AuthenticatedNode` 不是永久授权缓存。每个 Control 请求、Relay Bind、
入站 Datagram 和出站 route lookup 都必须在执行前，把连接捕获的完整 AuthorizationTuple
与当前内存 snapshot 中的 Node/lease tuple 再次逐项比较。RelayBind 另按下文要求引用同一
节点当前 Ready Control，但在 route 安装前不要求已经拥有 route；只有 Datagram 转发、
route lookup、route 替换和清理还必须验证完整 pair ID 仍拥有目标 route。内存 snapshot
提交是授权栅栏；tuple 变化后旧会话即使尚未收到响应或尚未被物理关闭，也只能丢包/拒绝
请求，不能继续收发或删除新 route。

三条 V3-R1 连接全部禁用 TLS 1.3 0-RTT 和 session resumption。deployment service TLS
与节点 PKI 保持独立；三条 Server listener 使用 deployment service certificate 证明
Server 身份，control/Relay 额外要求 node leaf 客户端认证。

## 线协议

### 固定帧

所有应用消息使用 12-byte header：

```text
0..4   magic = "STLR"
4..6   version = 3 (big endian u16)
6      message type
7      flags = 0
8..12  payload length (big endian u32)
```

enrollment/control JSON payload 最大 32 KiB，Relay handshake 最大 4 KiB。每条连接只
允许对端打开一条应用双向 stream；额外 stream 关闭整个连接。codec 必须处理任意 stream
分片和连续多个完整 frame；JSON payload 内的 trailing token/bytes 才是错误，不能把合法
的下一帧误判为 trailing bytes。

payload 必须是一个 UTF-8 JSON object。所有字段都必须出现，除 `Error.operation_id` 可以
为 `null` 外没有 optional 字段；struct 与所有嵌套对象使用 `deny_unknown_fields`，重复字段、
浮点数、负数、超出 `u64`、未知 enum、控制字符和非规范编码全部拒绝。JSON 编码固定为：

- UUID 使用规范小写连字符文本，并验证规定的 v7 或 v4 version；
- TrustDomainId 使用 64 位小写 hex，SPKI/manifest/request digest 使用 `sha256:` 加 64 位
  小写 hex；IPv4/CIDR 使用无前导零的规范文本；
- token 独占使用 `stl3_` 加 32-byte 无填充 base64url；其他二进制字段使用 RFC 4648
  standard base64，必须带规定 padding 且不得有空白；
- CSR DER 解码后最多 4096 bytes，单张 certificate DER 最多 6144 bytes，issuer manifest
  DER 最多 16384 bytes；certificate chain 恰好是 `[leaf, Intermediate]` 两个 DER base64
  元素，不在线上发送 Root，也不接受 PEM；
- `AuthorizationTuple` JSON 字段固定为 `trust_domain_id`、`node_uid`、`overlay_ip`、
  `lifecycle`、`state_epoch`、`allocation_epoch`、`key_epoch`、`revocation_epoch`、
  `authorized_spki_sha256`；线上只允许 `lifecycle = "active"`。

magic/version/type/flags、方向、长度、UTF-8、JSON 或语义校验失败均发送不出任何应用成功
结果并关闭连接；实现必须先验证长度上限再分配 payload。

### Enrollment

`stellaris/enroll/3` 使用 deployment service TLS，不要求现有 node client certificate：

| type | 消息 | 方向 | 完整 JSON 字段 |
| --- | --- | --- | --- |
| `0x01` | `ManifestRequest` | Agent -> Server | `known_manifest_epoch` (`u64`; 0 表示没有) |
| `0x02` | `ManifestResponse` | Server -> Agent | `issuer_manifest_der_base64` |
| `0x03` | `EnrollRequest` | Agent -> Server | `operation_id`、`intent` (`"initial"` 或 `"replacement"`)、`admission_token`、`csr_der_base64` |
| `0x04` | `EnrollAccepted` | Server -> Agent | `operation_id`、`authorization`、`overlay_cidr`、`mtu`、`certificate_chain_der_base64`、`certificate_not_before_unix_seconds`、`certificate_not_after_unix_seconds`、`issuer_manifest_der_base64` |
| `0xff` | `Error` | 双向 | 见 [Error](#error) |

连接状态机固定为以下两种之一：

```text
manifest refresh: ManifestRequest -> ManifestResponse -> close
                  ManifestRequest -> Error -> close
enrollment:       ManifestRequest -> Error -> close
                  ManifestRequest -> ManifestResponse
                  -> EnrollRequest -> EnrollAccepted | Error -> close
```

`known_manifest_epoch <= current` 时 Server 总是返回当前完整 manifest；若 Agent 声明更高
epoch，Server 返回 `manifest_stale` 并关闭连接，不发送较低 manifest。首次和 replacement
enrollment 共用 `EnrollRequest`，由持久 AdmissionKind 决定目标；`intent` 只声明调用命令
种类，不能覆盖 AdmissionKind。请求不得携带 NodeName、NodeUid 或 overlay IP，CSR 也必须
满足上文空 Subject、无 attributes/requested extensions 的 profile，不能夹带 SAN、名称、
地址或 claims。
`ManifestRequest` 不需要 token，但受连接数、来源 IP、频率和响应大小上限约束。

处理 `EnrollRequest` 时，Server 必须先按 operation ID 和完整 framed request digest 执行
下文 replay/conflict 判定；只有不存在 replay record 时才处理 admission。Server 解码 raw
32-byte token、计算 SHA-256，并以该全局唯一 digest 定位 Pending Admission；AdmissionId
是纯内部标识，不进入 token 文件或线协议。接受匹配前必须使用 constant-time 32-byte
digest equality。在构造签发、消费或其他 enrollment mutation 前，Server 必须要求
`intent = "initial"` 对应 Initial、`intent = "replacement"` 对应 Replacement；不匹配时
不得消费 token。不存在、非 Pending、过期、取消、已消费、digest 不匹配或 intent 不匹配
对外均只返回完全相同类别的 `admission_rejected`，不得通过 code、message、retryable、
日志、metrics label 或可控分支响应泄露是否命中及其状态。

Agent 已在发送 EnrollRequest 前 durable 接受本连接的当前 ManifestResponse，在 renewal 前
也已经接受当前 ControlWelcome/ManifestUpdate。由于成功响应需要 exact replay，
EnrollAccepted 或 CertificateIssued 内嵌的 manifest 可以是同一 Root 下较低 epoch 的历史
exact bytes：Agent 必须验证其 Root 签名、域、期限以及 leaf claims epoch，但不得用它覆盖
本地较高 manifest；Agent 不保存完整 manifest 历史，因此不声称检测较低历史 epoch 的
同 epoch 分叉。只有 leaf 的实际 issuer 仍在本连接/会话已接受的当前 manifest active set
中才接受证书。内嵌 manifest 高于当前 epoch、与当前同 epoch 但 bytes 不同或 issuer
已移除均 fail-closed。旧 issuer 被移除后 exact response 仍逐字节
replay，绝不能改签或降级 manifest；Agent 将其视为结果已确定但 candidate 不可用。renewal
在仍有可认证 current identity 时删除该 staged candidate 并生成新的 operation；已消费的
enroll/re-enroll 无法重用 token，必须丢弃 staged operation，由管理员为已创建/更新的
NodeUid 签发新的 Replacement admission。这是立即撤销 issuer 的明确可用性代价。

### Control

`stellaris/control/3` 要求 node leaf client authentication：

| type | 消息 | 方向 | 完整 JSON 字段 |
| --- | --- | --- | --- |
| `0x01` | `ControlWelcome` | Server -> Agent | `control_session_id`、`authorization`、`overlay_cidr`、`mtu`、`certificate_not_after_unix_seconds`、`issuer_manifest_der_base64` |
| `0x02` | `ControlReady` | Agent -> Server | `control_session_id`、`manifest_epoch`、`manifest_sha256` |
| `0x03` | `RenewCertificate` | Agent -> Server | `operation_id`、`csr_der_base64` |
| `0x04` | `CertificateIssued` | Server -> Agent | `operation_id`、`authorization`、`certificate_chain_der_base64`、`certificate_not_before_unix_seconds`、`certificate_not_after_unix_seconds`、`issuer_manifest_der_base64` |
| `0x05` | `ManifestUpdate` | Server -> Agent | `issuer_manifest_der_base64` |
| `0xff` | `Error` | 双向 | 见 [Error](#error) |

TLS 认证先生成不可由 JSON 覆盖的 AuthenticatedNode。Server 生成新的 v4
`control_session_id` 并发送 Welcome；Agent 必须要求 lifecycle=Active，并验证 principal、
IP、allocation/key/revocation epoch、SPKI、网络参数和 leaf 与本地 identity 完全一致。
Welcome 的 `state_epoch` 不得低于本地值；相等时保持不变，更高时表示 Server 在保留同一
key/lease 的 disable/enable 周期后重新启用节点，Agent 必须把新值与验证通过的 manifest
一起原子持久化。完成这些步骤后才用 manifest epoch 和 exact-byte SHA-256 发送 Ready。
Server 只在二者匹配当前 manifest 后把该 pending Control 标记 Ready，Ready 前拒绝
RelayBind 和其他 Control 请求。

Ready 后允许至多一个在途 renewal operation；ManifestUpdate 只接受更高 epoch 或相同
epoch 的逐字节 replay，Agent 必须先 durable persist 再使用，低 epoch/同 epoch分叉关闭
连接。普通 renewal CSR 必须使用当前 SPKI 并满足同一严格 CSR profile；
CertificateIssued 的 AuthorizationTuple 必须与当前 tuple 完全相等，普通 renewal 不改变
UID、IP、SPKI 或任何 epoch。换钥只能使用 replacement admission 和 `agent re-enroll`。

### Relay

`stellaris/relay/3` 要求与当前 Control 相同的 AuthenticatedNode。Control 与 Relay TLS
分别记录其实际 node leaf NotAfter；不要求两条连接使用逐字节相同 leaf，因为普通续期的
重叠 leaf 可以具有相同 AuthorizationTuple：

| type | 消息 | 方向 | 完整 JSON 字段 |
| --- | --- | --- | --- |
| `0x01` | `RelayBind` | Agent -> Server | `control_session_id` |
| `0x02` | `RelayAccepted` | Server -> Agent | `control_session_id`、`relay_session_id`、`mtu`、`max_datagram_size` |
| `0x03` | `RelayReady` | 双向 | `relay_session_id` |
| `0xff` | `Error` | 双向 | 见 [Error](#error) |

Relay 连接状态固定为
`RelayBind -> RelayAccepted -> Agent RelayReady -> Server RelayReady`。Server 只接受同一
AuthenticatedNode 当前 pending/active Ready Control 的 Bind，并为每个 attempt 生成新的
v4 `relay_session_id`。Agent 校验 Accepted 后回送该 ID；Server 只有在 session ownership
仍匹配时才原子安装 route 并回送同一 ID 作为 ack。Agent 收到 ack 后才能发送 TUN 包。
所有 route、延迟 Ready、关闭和清理均按 `(control_session_id, relay_session_id)` 比较，
旧 attempt 绝不能安装、替换或删除新 route。pair/route deadline 固定取 Control 与 Relay
两张实际 leaf NotAfter 的较小值，任一连接已到期时不得安装或提升 route。

Relay Datagram 只接受完整 IPv4 包：源地址必须等于 AuthenticatedNode.overlay_ip，目标
必须是另一个 Ready route，长度必须等于 IPv4 total length 且不超过 MTU。禁止 IPv6、
网络/广播/组播、源伪造、overlay 外目标和控制消息覆盖身份。每个包只进入一个 Relay
route，不实现 P2P 或第二路径。

### Error

三条协议共享以下有界 Error envelope；所有字段都必须出现：

```text
operation_id: UUIDv7 string | null
code: fixed lowercase enum
message: UTF-8 string, 1..=512 bytes
retryable: boolean
```

错误码固定为：

```text
invalid_request, protocol_violation, admission_rejected, operation_conflict,
node_disabled, node_revoked, node_released, capacity_exhausted,
manifest_stale, server_busy, recovery_required, internal
```

`admission_rejected` 不区分不存在、过期、取消、已消费或 token 错误。`message` 不得包含
控制字符、token、私钥、CSR、证书、内部路径或用户包。认证、协议和方向错误关闭连接；
只有经过 deployment TLS 认证的 `admission_rejected`、lifecycle error、
`capacity_exhausted` 或 `operation_conflict` 且 Server 明确标记 `retryable = false` 时，Agent
才可丢弃 staged operation。网络中断、超时、`server_busy`、`internal` 和任何未认证错误
均保留 staged bytes 并原样重试。

## 状态机、持久化和失败语义

### Coordinator snapshot

单一规范化紧凑 JSON 快照完整包含：

```rust
struct TrustDomainRecord {
    name: TrustDomainName,
    id: TrustDomainId,
    root_certificate_der_base64: String,
    root_certificate_sha256: Sha256Fingerprint,
    issuer_manifest_der_base64: String,
    issuer_manifest_sha256: Sha256Fingerprint,
    manifest_epoch: u64,
    signing_issuer_certificate_der_base64: String,
    signing_issuer_spki_sha256: Sha256Fingerprint,
    signing_issuer_private_key_sha256: Sha256Fingerprint,
}

struct NetworkRecord {
    overlay_cidr: Ipv4Net,
    reserved_ipv4: Vec<Ipv4Addr>,
    mtu: u16,
    max_nodes: u16,
}

struct NodeRecord {
    trust_domain_id: TrustDomainId,
    node_uid: NodeUid,
    name: NodeName,
    lifecycle: NodeLifecycle,
    overlay_ip: Option<Ipv4Addr>,
    name_epoch: u64,
    state_epoch: u64,
    allocation_epoch: u64,
    key_epoch: u64,
    revocation_epoch: u64,
    authorized_spki_sha256: Option<Sha256Fingerprint>,
}

enum AdmissionKind {
    Initial { node_name: NodeName, fixed_ip: Option<Ipv4Addr> },
    Replacement { node_uid: NodeUid },
}

enum AdmissionState {
    Pending,
    Consumed { operation_id: OperationId },
    Cancelled,
    Expired,
}

struct AdmissionRecord {
    trust_domain_id: TrustDomainId,
    admission_id: AdmissionId,
    kind: AdmissionKind,
    state: AdmissionState,
    token_sha256: Sha256Fingerprint,
    created_at: UnixSeconds,
    expires_at: UnixSeconds,
}

enum AddressLeaseState {
    Free,
    Reserved { admission_id: AdmissionId },
    Allocated { owner: NodeUid },
    Quarantined {
        previous_owner: NodeUid,
        not_reusable_before: UnixSeconds,
    },
}

struct AddressLeaseRecord {
    overlay_ip: Ipv4Addr,
    state: AddressLeaseState,
    allocation_epoch: u64,
    last_issued_not_after: Option<UnixSeconds>,
}

enum OperationSubject {
    Enrollment { admission_id: AdmissionId, node_uid: NodeUid },
    Renewal { node_uid: NodeUid },
}

struct OperationReplay {
    operation_id: OperationId,
    subject: OperationSubject,
    request_sha256: Sha256Fingerprint,
    exact_framed_response_base64: String,
    certificate_not_after: UnixSeconds,
    signing_issuer_spki_sha256: Sha256Fingerprint,
    retain_until: UnixSeconds,
}

struct CoordinatorSnapshot {
    version: u16, // 3
    trust_domain: TrustDomainRecord,
    network: NetworkRecord,
    state_generation: u64,
    previous_state_sha256: Option<Sha256Fingerprint>,
    clock_watermark: UnixSeconds,
    nodes: BTreeMap<NodeUid, NodeRecord>,
    admissions: BTreeMap<AdmissionId, AdmissionRecord>,
    leases: BTreeMap<Ipv4Addr, AddressLeaseRecord>,
    historical_names: BTreeMap<NodeName, NodeUid>,
    historical_spki: BTreeMap<Sha256Fingerprint, NodeUid>,
    operation_replays: BTreeMap<OperationId, OperationReplay>,
}
```

持久 JSON 中所有 struct 和 enum variant 都拒绝未知或重复字段。字段顺序按上面声明；
`NodeLifecycle` 编码为 `active|disabled|revoked|released` 小写字符串。所有 enum object 的
内部 tag 统一为 `type`：AdmissionKind 使用 `initial|replacement`，AdmissionState 使用
`pending|consumed|cancelled|expired`，AddressLeaseState 使用
`free|reserved|allocated|quarantined`，OperationSubject 使用 `enrollment|renewal`；其余字段
按 variant 中的声明顺序紧随 tag。所有 `Option` 字段都必须出现，`None` 固定编码为 JSON
`null`，不得靠省略表达。`reserved_ipv4` 按 IPv4 数值严格递增且无重复；exact framed
response 使用带 padding 的 RFC 4648 standard base64。

snapshot 是 trust domain、当前 exact manifest 和当前签发 Intermediate 的逻辑权威；在线
private key 不写入 snapshot，但其 PKCS#8 exact-byte SHA-256 必须被 snapshot 绑定。
Server 启动后只使用启动时已经读取并匹配该 digest 的内存 key，不在运行中重新读取文件。
`server init` 把规范化后的 overlay CIDR、数值排序的 reserved set、MTU 和 max_nodes 写入
NetworkRecord；`server run`、backup/restore 和 `config check` 都要求配置与该记录精确相等。

canonical serializer 固定输出 UTF-8、无 BOM、无空白和无尾随换行的单个 JSON object；
struct 按本文字段顺序输出，BTreeMap key 使用各强类型规范文本的 raw ASCII bytes 排序，
整数使用无前导零十进制，字符串只对 quote、backslash 和 U+0000..U+001F 使用 JSON escape，
quote/backslash 分别固定为 `\"`/`\\`，控制字符一律使用 `\u00xx` 小写 hex，不使用
等价 short escape。decoder 必须重新序列化并要求 bytes 完全相等，才接受
snapshot；state SHA-256 直接覆盖这组 exact bytes。实现必须冻结 generation 1 和至少一组
含 Node/admission/lease/replay 的 canonical test vector。

`state_generation` 初始化为 1，每次有状态变化的事务增加 1；幂等 replay 和无变化管理
操作不增加 generation。generation 1 的 nodes、admissions、historical names/SPKI 和 replay
map 为空；leases map 恰好为 overlay 中扣除 network、broadcast 和 reserved set 后的每个
host 建立一个 Free、allocation epoch 0、`last_issued_not_after = null` 的记录。`server init`
在校验证书/manifest 时间时取得同一个 `observed_now`，generation 1 的
`clock_watermark = observed_now`；canonical fixture 使用固定 UnixSeconds 测试值，不能由
serializer 自行读取时钟。

`max_nodes` 占用数固定为 Active/Disabled/Revoked NodeRecord 数量，加 Pending Initial
Admission 数量；Consumed Initial 已由 Node 计数，Replacement、Cancelled、Expired 和
Released tombstone 不计。创建 Initial admission 必须在同一锁和事务中先执行 expiry sweep、
再检查 `occupied + 1 <= max_nodes`，超限时无状态或 token-file 副作用。cancel/expire 或
release 会释放名额，但 Released Node/name/SPKI tombstone 永久保存，不阻止地址按隔离规则
分配给新 NodeUid。

所有 map key 必须与 record 内的 NodeUid、AdmissionId、IPv4 或 OperationId 相等；所有
NodeRecord 和 AdmissionRecord 的 TrustDomainId 必须等于 TrustDomainRecord。Node 与 lease
状态约束固定为：

| lifecycle | overlay_ip | authorized SPKI | lease 约束 |
| --- | --- | --- | --- |
| Active、Disabled | 非 null | 非 null | 恰好一个同 IP、owner 和 allocation epoch 的 Allocated lease |
| Revoked | 非 null | null | 恰好一个同 IP、owner 和 allocation epoch 的 Allocated lease |
| Released | null | null | 不拥有 lease；NodeRecord 保留最后 allocation epoch |

每个 Allocated lease 反向只能对应上述一个未 Released Node。`historical_names` 包含每个
曾提交给 NodeRecord 的名称（包括当前名称），值为唯一 NodeUid，永不删除或改绑。
`historical_spki` 同样包含每个曾成功授权的 node SPKI（包括当前 SPKI），值为唯一 NodeUid；
initial/replacement enrollment 的 CSR SPKI 必须此前从未出现，成功事务中插入且永不删除
或改绑。普通 renewal 必须复用当前 SPKI，不新增记录；exact operation replay 在该检查前
返回旧响应。由此旧 key 不能通过后续 replacement admission 被同一或其他 Node 重新授权。
每个 Quarantined lease 的 `previous_owner` 必须指向 Released NodeRecord，二者保留的
allocation epoch 必须相等；lease 转为 Reserved、Free 或 Allocated 后该反向关联随 variant
字段一起消失。`not_reusable_before` 必须至少为 `last_issued_not_after + 5 minutes`，加法
溢出时 release 失败且状态不变。
启动、每次事务前后以及读取备份时都完整校验上述不变量。

### Admission 与 token

Initial admission 的名称必须与现有 Node、永久 name tombstone 和其他 Pending Initial
admission 都不冲突；未消费的 admission 取消/过期后仅释放临时名称保留，不建立永久
tombstone。指定 fixed IP 时必须是池内可分配 host，并转为 Reserved；未指定时直到
enrollment 才选择地址。可分配表示 Free，或
`not_reusable_before <= observed_now` 的 Quarantined；后一种在 reservation 事务中转为
Reserved，保留 allocation epoch 和 last-issued 水位。

Replacement admission 只允许绑定 Active 或 Revoked Node，不改变现有 lease；Disabled
时必须先 enable 回到 Active，或先 revoke 再为新的 Revoked 状态创建 recovery admission；
Released 永久拒绝。每个 Node 同时最多一个 Pending Replacement。实际发生的 disable、
revoke 或 release 转换在同一事务中把当时已有的 Pending Replacement 变为 Cancelled，
从而保证 Active 状态签发的换钥 capability 不能跨越 revoke 恢复节点；转换完成后才能为
当前 Active 或 Revoked 状态创建新 token。幂等重复 lifecycle 操作不取消后来创建的 token。
token digest 在全状态中唯一且只保存在对应 AdmissionRecord；Pending admission 只能成功
消费一次。除 operation replay/conflict 必须先判定外，所有 CLI、Server 启动和新的
EnrollRequest 处理都先在同一事务中把 `expires_at <= observed_now` 的 Pending 记录变为
Expired，并释放其 Initial reservation；cancel Pending 幂等转 Cancelled，cancel
Cancelled/Expired 无变化成功，Consumed 拒绝。

Pending Initial 有 fixed IP 当且仅当存在一个指回该 AdmissionId 的 Reserved lease；
Pending Initial 无 fixed IP 以及所有 Replacement 不得拥有 reservation。Consumed Initial
的目标 NodeUid 由 `historical_names[node_name]` 唯一确定，Consumed Replacement 的目标由
AdmissionKind 固定；对应 replay 尚存在时，其 enrollment subject 必须与 AdmissionId、
OperationId 和目标 NodeUid 全部一致。replay 在保留期后可以删除，因此 Consumed 不要求
该外键永久存在。Cancelled/Expired 不得拥有 reservation，所有 AdmissionRecord 满足
`created_at < expires_at`。Pending Replacement 指向的 Node 必须仍为 Active 或 Revoked；
Disabled 或 Released Node 不得残留 Pending Replacement。

创建 admission 时先取得 store 锁、执行 expiry sweep 并完整构造和校验 provisional next
state，再以 create-new `0600` 在目标目录写 token 临时文件、以 no-replace 语义 rename 为
`--token-out` 并完成 file/directory fsync；随后重新校验同一 next state，执行只含 digest
的 state/witness 提交，最后才向调用者报告成功。state 提交失败时 token 文件可能存在但
不是有效 capability，命令必须失败并明确要求操作员删除；绝不能先提交一个无法恢复明文
token 的 admission。这是下述 Normal Coordinator store mutation 顺序唯一允许的外部
precommit，token 文件不是成功响应或运行时副作用；故障测试必须覆盖 token 每个
write/fsync/rename 和 state 提交边界。离线 `server restore` 服从下文独立恢复顺序，
不属于 Normal mutation。
Consumed admission 审计记录永久保留；精确 enrollment response 的保留期服从下文
Operation replay。

### 地址 lease

地址池排除 network、broadcast 和 `reserved_ipv4`，动态分配始终在 Free 与隔离期已结束
的 Quarantined 中选择数值最小的地址。allocation epoch 初始为 0，只在地址从 Free、
Reserved 或隔离期已经结束的 Quarantined 分给新 owner 时 checked-increment，并把结果
同时写入 NodeRecord；首次分配从 0 变为 1，复用可以为 2+。reservation 的创建、取消和
过期本身不增加 epoch；取消或过期后转为 Free，并保留 allocation epoch 和 last-issued
水位。`last_issued_not_after` 为 null 当且仅当该地址从未签发 leaf；Allocated 和
Quarantined 必须非 null，且该值在任何转换中都不得清零或回退。
首次 enrollment、renewal 和 re-enrollment 必须在返回任何证书 bytes 前原子执行：

```text
last_issued_not_after =
    max(last_issued_not_after, new_leaf.NotAfter)
```

release 只接受 Revoked，原子写入 Released node tombstone 和 Quarantined lease。
`not_reusable_before = max(last_issued_not_after, now) + 5 minutes`。隔离期结束只使 lease
具备再次分配资格；真正分给新 owner 时才增加 allocation epoch。

### 原子提交与 witness

Server 使用 `fs2` advisory exclusive lock。解析规范绝对路径并拒绝 symlink 后，
witness path 必须位于 state 文件及其父目录之外；每次 backup 还必须验证
`--witness-out` 与 output bundle 互不包含。物理独立存储和访问控制由部署者保证。

除上述 admission token 文件 precommit 外，每次 Normal Coordinator store mutation 的
durable commit 顺序固定为：

```text
acquire in-process transaction lock
build and fully validate next snapshot
serialize canonical bytes and compute SHA-256
create same-directory temporary file with 0600
write all bytes
fsync temporary file
atomic rename over state
fsync state directory
create witness-directory temporary file with 0600
write all witness bytes
fsync witness temporary file
atomic rename over witness
fsync witness directory
commit in-memory snapshot
reply
perform runtime side effects
```

witness 固定包含：

```rust
struct StateWitness {
    version: u16, // 1
    trust_domain_id: TrustDomainId,
    state_generation: u64,
    state_sha256: Sha256Fingerprint,
    previous_state_sha256: Option<Sha256Fingerprint>,
}
```

witness 使用与 snapshot 相同的 canonical compact JSON、固定字段顺序和规范强类型文本；
读取时同样要求重新序列化后的 bytes 完全相等，`state_sha256` 始终只覆盖 state bytes。

启动判定固定为：

| state 与 witness | 结果 |
| --- | --- |
| TrustDomainId、generation、state digest 和 previous digest 全部相等 | Normal |
| state generation = witness generation + 1，且 state previous digest = witness state digest | 在绑定 listener 前按上述完整 witness 写序补写；成功后 Normal |
| 任一文件缺失、state 落后、领先多于一代、域不匹配、digest/previous digest 不匹配 | Recovery required |

generation 1 的 previous digest 为 `null`；其余 generation 必须恰好引用前一份 durable
state digest。任何持久 I/O 失败在当前进程中 poison store，禁止后续 mutation、listener
启动和外部确认。临时文件清理是 best effort，不得改变已判定的 durable state。

### Operation replay

每个成功提交签发的 EnrollRequest 和 RenewCertificate operation 按上述 schema 保存
operation ID、种类/关联 admission/node、完整 framed request bytes 的 SHA-256、精确 framed
response bytes、certificate NotAfter、实际 signing issuer SPKI 和
`retain_until = NotAfter + 5 minutes`；加法溢出时签发前 fail-closed。相同 ID + 相同 digest
逐字节 replay，不重新签名、提升水位或增加 generation；相同 ID + 不同 digest 返回
`operation_conflict`。EnrollRequest 的该判定先于
token digest 查找、admission expiry sweep 和任何 AdmissionState 校验，因此已消费 token
的相同请求仍只 replay 原响应。记录在 `retain_until` 前不能因容量压力删除，之后可以在
独立 committed mutation 中清理。每节点最多保留 8 个尚未到期 renewal replay，达到上限
时返回 `server_busy`，不能覆盖有效记录。

snapshot 校验必须用同一严格 codec 解码 `exact_framed_response_base64`，并要求它恰好是
Enrollment subject 的 EnrollAccepted 或 Renewal subject 的 CertificateIssued；frame 内
operation ID 和 AuthorizationTuple 的 NodeUid 必须分别匹配 map key 与 subject。解析出的
leaf NotAfter、实际 Intermediate SPKI 必须分别等于 `certificate_not_after`、
`signing_issuer_spki_sha256`，且 `retain_until` 必须恰好是前者 checked-add 300 秒。任一冗余
字段漂移、错误 frame/type/chain 或关联不一致都使整个 snapshot 无效。

### Agent identity

Agent identity 使用一个原子 `identity.json` 保存当前 private key、leaf DER chain、预置
Root 的 exact digest、最高 manifest、principal、网络参数和 authorization epochs；它不
保存或接受 NodeName。enroll/re-enroll 的 `staged-operation.json` 保存新 private key、
严格 CSR、token binding、operation ID、精确 framed request bytes、类型和必须出现的
`superseded_renewal_operation_id`（普通操作为 null）。renewal 使用
`staged-renewal.json` 保存当前 key 生成的严格 CSR、operation ID、精确 request bytes，
以及收到并验证但尚未完成链路切换的 candidate leaf response。

Agent 在第一次发送任何签发请求前完成 staged file write、file fsync、rename 和 directory
fsync。超时、断线、崩溃或不确定错误必须原样重试；不得生成第三把 key 或新 operation
ID。enroll/re-enroll 成功响应经 Root、manifest、证书 profile、principal、IP、SPKI 和
epoch 完整验证后，原子替换 identity，再删除 staged 文件并 fsync 目录。若在 identity
replace 后、staged cleanup 前崩溃，下一次显式 enroll/re-enroll 必须识别 identity 中记录
的 completed operation ID，只完成 cleanup，不生成或重发不同请求。

renewal candidate 必须先与 staged request 一起 durable persist，再用于建立 pending
Control/Relay；只有 pending pair Ready 后才原子提升 candidate 为 current leaf、记录
completed operation ID、删除 staged-renewal 并 fsync directory。任一 crash 后只能继续
旧 current leaf 或恢复同一 staged renewal，不能产生第三张 leaf/request。

唯一例外是操作员持有 Replacement token 并显式执行 `agent re-enroll`：主动换钥、Revoked
恢复、current leaf 到期或 issuer 移除都允许它接管未完成 renewal。命令先 durable 创建新的
replacement `staged-operation.json`，其中记录被取代的 renewal OperationId，再删除
`staged-renewal.json` 并 fsync directory；若 crash 后两者同时存在，只有在
`staged-operation.superseded_renewal_operation_id` 等于 `staged-renewal.operation_id` 时，
replacement stage 才优先并负责清理旧文件，否则 fail-closed。该接管不修改 Server 上已
确定 renewal 的结果；replacement 使用新 key/OperationId，并按正常 exact request retry。
`agent enroll` 和 `agent run` 永远不能执行这种取代。

Agent identity 不提供 snapshot anti-rollback。identity 丢失或回滚后已经无法认证时，
管理员必须创建 replacement admission；旧 SPKI、旧授权 epoch 或过期 leaf 均会被当前
Server 状态拒绝。普通 renewal 复用同一 SPKI 和授权 epoch，Server 不持久化 current leaf
fingerprint/serial，因此回滚到同 key、仍在有效期内的旧 renewal leaf 可能继续认证；
本阶段不声称阻止这种 Agent 本地回滚。

### Clock 与恢复模式

store 在每次使用 wall clock 的 committed mutation 中持久化
`clock_watermark = max(clock_watermark, observed_now)`。系统时间比 watermark 落后超过
5 分钟时分类为 Recovery required；校正可信系统时间到允许范围即可重新判定，绝不能靠
降低 watermark 恢复。

Recovery required 是离线 store 状态，不是降级网络服务。`server run` 必须在绑定任何
enrollment/control/Relay listener 或产生 runtime 副作用前，以固定非零
`recovery_required` 退出；只允许离线 `config check`、诊断读取和 `server restore`。因此
recovery 状态下不存在签发、Control、Relay、分配、复用或只读网络 listener。

普通文件 witness 只有在操作员把它保存于独立、受保护且确认最新的位置时才是可信外部
见证。攻击者或误操作若能把 state、backup 和 witness 一起回滚到一致旧代，本方案无法
检测；本文不声称提供 HSM/远程单调计数的回滚抵抗。witness 不得放进 backup bundle；
`--witness-out` 是必须由操作员独立保管的 exact witness copy。

`server backup` 仅在 Normal、独占锁和 state/witness 完全一致时运行。`--output` 必须是
不存在的新目录；bundle 固定包含 exact state、Root certificate、当前 signing
Intermediate certificate/private key、当前 issuer manifest 和 `backup-manifest.json`。
backup manifest 固定记录 schema 1、TrustDomainId、generation、state digest、immutable
network/trust-domain 配置摘要、每个相对文件名和 SHA-256；deployment service TLS、Root
private key、listener 和 observability 配置不进入 bundle。全部 bundle file/dir fsync 完成
后，再以 create-new 写并 fsync bundle 外的 `--witness-out`，最后报告成功。

`server restore` 要求 Server 停止并取得锁，只接受所有 file digest、Root/manifest/
signer binding、TrustDomainId、generation、state digest 与 `--witness` 逐项精确匹配的
完整 bundle。它先把 PKI 文件写入各配置目标的 temp、fsync/rename/fsync directory，再
写 state，最后写 live witness；任何 crash 后的下一次启动都必须在 PKI binding 或
state/witness 检查处 fail-closed，使用相同 bundle 和 witness 重跑必须幂等完成。已有
Normal state 只允许恢复同 generation、同 digest 的
exact backup，不允许回滚；非 Normal state 也只能使用操作员声明为当前且匹配的可信
witness。其他退出方式只有废弃原目录并以新 Root/TrustDomainId 全量初始化。

## Relay 运行闭环

### Server

`server init` 验证 Root、Intermediate certificate/key、epoch 1 manifest、service TLS、
地址容量、state/witness 位置和权限后创建 generation 1 snapshot；其中 Node/admission/
history/replay 为空，全部可用地址具有上文固定的 Free lease。TrustDomainRecord 保存
Root/manifest/signer 的 exact bytes 或 digest 绑定，命令不生成 Root 或 Intermediate。

`server run` 获取 store 锁并完成 state/witness 判定后，读取配置指向的 Root、manifest 和
signing Intermediate certificate/key。Root certificate digest 必须与 snapshot 完全相等；
signer key 必须匹配 certificate，certificate 必须链到 Root 且其 SPKI 位于输入 manifest
active set。输入 manifest 只允许与 snapshot exact-byte 相同，或是验证通过的更高 epoch；
低 epoch、同 epoch 不同 bytes 和未知 issuer 全部在 listener 前 fail-closed。

若 manifest epoch 更高，或在同一 active set 内切换 signing issuer，Server 将其作为一次
停服 `PkiActivation` mutation：更新 TrustDomainRecord 中的 exact manifest、epoch、signer
certificate/SPKI/key digest，按完整 state/witness 顺序 durable commit，再把 key 读入内存。
更高 manifest 可以立即移除 issuer；移除后由该 issuer 签发的 leaf 和 replay candidate
无条件拒绝，不等待 leaf 或 replay 到期。只有 Normal 判定或成功补写/激活后才绑定三条
Quinn listener。运行中修改任何 PKI 文件没有效果；下一次停服启动重新执行上述流程。

Server enrollment 事务在一个锁内完成 admission/token/CSR 校验、NodeUid 生成、地址
分配、leaf 签发、水位更新、token 消费和精确响应持久化。只有 state 与 witness 均
durable 后才返回。

内存 snapshot 的提交同时构成授权栅栏。尤其 Active replacement enrollment 改变 SPKI/
key epoch 后，旧 pair 的 tuple 在响应发送前已经不再匹配；Control 和 Relay worker 必须
按上文逐请求、逐 Datagram/route lookup 拒绝旧 tuple。响应之后按完整 pair ID 关闭旧
Control/Relay 和移除旧 route 只是资源清理，失败或延迟不能恢复其授权。

Control TLS 认证生成 AuthenticatedNode。每个 Node 在内存中至多有一组 `active_pair` 和
一组 `pending_pair`；二者都由独立 v4 ControlSessionId/RelaySessionId 标识，不使用额外
持久 session generation counter。新 Control Welcome/Ready 只建立 pending Control，
不替换 active。pending Control Ready 后才能绑定 pending Relay；Server 验证 Agent
RelayReady 后按完整 pair ID 原子把 route 从旧 active 切到 pending、将 pending 提升为
active、回送 RelayReady ack，
随后关闭旧 pair。pending 在 Ready 前失败或超时只清理自己的 ID，旧 active 与 route
保持不变。Server 重启后没有在线 pair，Agent 必须重新认证，但 principal、IP 和所有
授权 epoch 保持。

Server 为每个 pending/active pair 分别保存 Control TLS 与 Relay TLS 已验证 node leaf 的
exact NotAfter，并把二者较小值以可信 wall clock 映射到 pair 的 monotonic deadline；
`now >= min(control_not_after, relay_not_after)` 时必须先按完整 pair ID 原子移除 route，再
关闭 Relay 和 Control。pending 在该时刻不得提升。5 分钟 clock skew 只用于证书/manifest
验证边界，不延长已建立会话的 NotAfter，因此混用重叠期新旧 leaf 的非协作 Agent 也不能
维持到较晚证书的期限。

### Agent

`agent run` 只加载完整 identity，使用 deployment CA 验证三个 Server listener。它先在
enrollment ALPN 上发送 ManifestRequest，验证 Root/domain/epoch 后原子保存返回的 exact
manifest；这只是公开信任材料刷新，不是 enrollment 或 rekey。随后 Agent 建立 Ready
Control，再创建/恢复 TUN 和 route，最后建立 Ready Relay；任何中间失败都清理本次安装
的 route/TUN。存在 enroll/re-enroll staged operation 时要求操作员运行对应显式命令；
存在 staged renewal 时 `agent run` 只能恢复该相同 renewal。

Agent 始终将 TUN 出站包送入 Relay，不创建 P2P endpoint 或第二路径。Relay 断开时停止
转发并按有界退避重连；Control 失效会使 Relay lease 失效。

leaf renewal 使用以下固定正向 jitter：

```text
seed = SHA-256("stellaris renewal jitter v3\0" || NodeUid RFC4122 bytes || leaf DER)
jitter_seconds = big_endian_u64(seed[0..8]) mod 1801
renew_at = NotBefore + floor((NotAfter - NotBefore) / 2) + jitter_seconds
```

因此 jitter 只在生命周期中点后 `0..=30 minutes`。重启后由相同 identity 得到相同时间；
若 `now >= renew_at` 则立即恢复/开始 renewal，不重复延后。renewal 复用当前 key；Agent
验证并 durable 保存 candidate leaf 后建立 pending Control/Relay。Server RelayReady ack
之后 Agent 才原子把 candidate leaf/pair 提升为 current、切换 TUN writer 并关闭旧 pair，
绝不向两条 Relay 主动复制同一包。到旧 leaf NotAfter 仍未完成切换时关闭旧 pair、删除
其 route 并停止转发；若 Control/Relay 实际使用不同重叠 leaf，则以两者 NotAfter 较小值
为准。candidate/staged 状态保留供后续原样恢复。

### 管理传播

所有 server-local `node` 管理 CLI mutation 要求 Server 停止，因此这些变更后的第一次
`server run` 不恢复旧在线 session；enrollment listener 上的 initial/replacement enrollment
是上文定义的在线事务，不属于该 CLI 限制：

- rename 不改变 leaf 或 AuthorizationTuple；
- Disabled 节点不能建立新 control/Relay；enable 后原 key/leaf 可重新认证，Agent 只按
  上述规则单调同步新的 state epoch；
- Revoked 节点不能认证，replacement enrollment 成功后恢复 Active；
- Active replacement enrollment 原子替换 SPKI 并增加 key epoch；
- Released 节点永久拒绝，地址按 quarantine 规则等待复用。

### 资源与观测

enrollment 并发、每 IP handshake、Control session、Relay session、队列和 replay journal
均受配置上限约束。Prometheus 指标至少包含 admission/enrollment 结果、证书续期、
lifecycle mutation、recovery mode、Control/Relay session、路由包、分类丢包、queue
高水位和持久化错误；标签不得包含 token、证书、完整 UID/IP 或高基数 NodeName。

## 实施阶段与退出门禁

### 阶段

| 阶段 | Purpose | 交付 |
| --- | --- | --- |
| `V3-R0` | Design-only | 冻结本文和 ADR；ADR Accepted 后由后续 Design-only 记录本文 Approved 与批准 commit |
| `V3-R1` | Implementation | 在一个纵向阶段内替换公共类型、配置、协议、PKI、store、CLI 和 Relay runtime；同步 Current 文档，状态为 Implemented / Unverified |
| Evidence | Evidence-only | 对已合入的 `V3-R1` target commit 执行并归档全部原子 Gate；不得混入实现或正文变化 |

`V3-R1` 可以在实现分支中按类型/协议、store/PKI、CLI/runtime、测试/文档分提交评审，
但进入目标分支时必须是一个可编译、可运行的单一模型；不得合入双模型中间状态或公开
stub。

### 原子 Gate

以下 manifest 恰好包含 20 个原子 Gate；Evidence-only 的 Atomic Gate ID 集合必须与本表
完全相等，不能只引用阶段名或合并省略其中一项。

| 原子 Gate ID | 可独立判定的结果 | 初始状态 |
| --- | --- | --- |
| `V3-R0-SPEC-01` | ADR 0004 Accepted；本文 Approved 并记录完整批准 commit；公共接口、安全/恢复语义和以下 Gate 无未决项 | Open |
| `V3-R1-TYPES-01` | TrustDomain/Node/Admission/Operation/session 类型、规范编码、UUIDv7/v4、epoch 溢出和完整生命周期单元测试全部通过 | Open |
| `V3-R1-CONFIG-CLI-01` | 严格 Server/Agent TOML、snapshot 网络参数绑定、全部公开 CLI 参数/错误、PKI create-new 输出、权限/锁和全部禁止字段测试通过 | Open |
| `V3-R1-WIRE-01` | 三条 ALPN、固定头、type 数值/方向、JSON 字段与状态序列、DER chain、unknown/duplicate/noncanonical/truncated/oversize 输入测试通过 | Open |
| `V3-R1-RELAY-ONLY-01` | 二进制、配置和协议只暴露三条 ALPN；无 P2P/underlay path candidate/第二数据路径；Relay 不可用时数据停止且不尝试直连 | Open |
| `V3-R1-FUZZ-01` | wire、manifest DER、claims DER、persisted-state 四个 cargo-fuzz target 各运行 15 分钟，无 crash、OOM、异常 timeout 或非规范成功解码 | Open |
| `V3-R1-STATE-01` | canonical snapshot vector 与至少 10,000 个、每个最长 200 步的属性序列保持 Node/admission/lease/replay、name/SPKI tombstone、地址 ABA epoch、水位、幂等和隔离不变量 | Open |
| `V3-R1-ADMISSION-01` | Initial/Replacement intent、identity 前置、TTL sweep、cancel、lifecycle transition capability 失效、rename/名称/固定地址 reservation、constant-time token digest、类型误用不消费和 create-new `0600` 故障矩阵通过 | Open |
| `V3-R1-PERSIST-SERVER-01` | Server 在 token-file precommit、state write/file-fsync/rename/dir-fsync、witness write/file-fsync/rename/dir-fsync、memory commit 和 response 前后 crash 并重启，不产生半提交、有效 orphan capability 或提前响应 | Open |
| `V3-R1-PERSIST-AGENT-01` | Agent enroll/re-enroll/renewal staged write、显式 replacement 接管、identity/candidate swap、cleanup 各 crash 点恢复后只继续唯一权威 stage/current，不生成第三把 key/leaf/request | Open |
| `V3-R1-RECOVERY-01` | 可信 backup/restore、state/witness 一代补写、旧/混合 generation、digest 分叉、PKI binding、时钟回退和无 listener recovery-required 行为通过 | Open |
| `V3-R1-CAPACITY-01` | `/23` 中 256 个持久 lease、固定地址竞争、池耗尽通过；`/24 + max_nodes=256` 在副作用前拒绝 | Open |
| `V3-R1-PKI-PROFILE-01` | Root/Intermediate/leaf 链、canonical SPKI、严格空 Subject/无 attribute CSR、URI/IP SAN、EKU、critical claims、算法、TTL、clock skew 正反矩阵通过 | Open |
| `V3-R1-PKI-MANIFEST-01` | manifest DER/signature/domain/time/epoch/分叉、exact response 旧 epoch 和立即移除可用性代价测试通过；双 issuer overlap 接受，移除后旧 issuer leaf 无条件拒绝 | Open |
| `V3-R1-ENROLL-01` | 两 Agent 显式 enrollment 获得不同 UID/IP；token、禁止 CSR 自报名称/IP/扩展、PoP、并发、幂等、提交后响应丢失和重启场景通过 | Open |
| `V3-R1-RENEW-01` | 正向确定性 jitter、renewal 水位、精确 replay、candidate leaf、active/pending Ready 原子切换、混合新旧 leaf 的 pair 最早 NotAfter、Server/Agent 双侧强制关闭和重启场景通过 | Open |
| `V3-R1-LIFECYCLE-01` | rename、disable/enable state epoch 同步、Active rekey、revoke 前 token 失效、Revoked re-enroll、release、quarantine/reuse 和旧凭据/旧在线 tuple 立即拒绝通过 | Open |
| `V3-R1-RELAY-01` | 生产 Quinn + fake TUN 两节点完成 Control/Relay active-pending Ready、双向 IPv4、current tuple/route ownership 逐包栅栏、源校验、session ABA、无重复包和重启闭环 | Open |
| `V3-R1-LINUX-01` | disposable root Linux x86_64 namespace runner 上真实 TUN 完成双向 ping/TCP/UDP、Server/Agent 重启和正常/异常清理 | Open |
| `V3-R1-DOCS-01` | Current 规范、配置/部署/恢复/排障、支持矩阵、示例和 CHANGELOG 与实现一致，且仓库规定的 fmt、三组 Ruby 治理测试、docs check 和 diff check 全部通过 | Open |

Gate ID 一经本文 Approved 不得改义。阶段状态只由目标 commit 的
[verification](verification/README.md) latest result 计算，表中 Open 不回写为 Passed。

## 测试与验收

### 本地与 CI

- `cargo test -p stellaris --test v3_types --locked`；
- `cargo test -p stellaris --test v3_config_cli --locked`；
- `cargo test -p stellaris --test v3_wire --locked`；
- `cargo test -p stellaris --test v3_admission --locked`；
- `PROPTEST_CASES=10000 cargo test -p stellaris --test v3_state_model --locked`；
- `cargo test -p stellaris --test v3_persistence --locked -- --test-threads=1`；
- `cargo test -p stellaris --test v3_pki --locked`；
- `cargo test -p stellaris --test v3_two_node --locked -- --test-threads=1`；
- workspace fmt、Clippy `-D warnings`、tests、rustdoc 和 vendored transport checks；
- `cargo +nightly fuzz run v3_wire -- -max_total_time=900`、`v3_manifest`、`v3_claims` 和
  `v3_persisted_state` 使用同一参数分别执行；
- PR CI 对上述四个 cargo-fuzz target 各执行 60 秒 smoke；
  60 秒结果不能替代每 target 15 分钟的 Evidence Gate。

生产组件必须注入 `Clock`、UUID/RNG、durable writer stage 和 response sink，故障测试复用
生产提交路径。时间测试使用 fake Clock，不 sleep 24 小时。持久化 Gate 使用子进程 kill
与 reopen，不只依赖 test-only 返回错误。

### Linux 真实网络

`tests/e2e/run-real-tun.sh linux identity-relay` 必须：

- 在唯一命名的两个 netns 中创建 veth/bridge 和两个真实 TUN；
- 启动一个 Coordinator 和两个 Agent；
- 显式 enrollment 后确认不同 UID/IP 和持久 lease；
- 经唯一 Relay 路径双向运行 ping、TCP echo 和 UDP echo；
- 验证 Ready 前无 route、源伪造丢弃、旧 session cleanup 不影响新 session；
- 重启 Server 和两个 Agent，确认 identity/lease 保持且重新认证；
- 正常退出、被终止和启动失败均清理 route、TUN、namespace 和子进程；
- 记录脱敏日志、`ip -j link/address/route` 前后快照、包计数、命令、内核、权限和 artifact
  SHA-256，不记录 token、私钥、CSR、完整证书或用户包。

测试名缺失、ignored、skip、无 root、无 `/dev/net/tun` 或清理失败均 fail-closed。

## 部署、恢复与回滚

本阶段是停机全量重建，不是滚动升级。部署顺序固定为：

1. 在离线环境生成 Root；
2. 签发在线 Intermediate 和 epoch 1 issuer manifest；
3. 配置独立 deployment service TLS；
4. 初始化空 Server state 与外部 witness；
5. 为每个节点创建 Initial admission；
6. 在 Agent 上只预置 Root、期望的 TrustDomainName 和 deployment CA；
7. 逐节点执行 `agent enroll`；
8. 启动 Server 和 Agent，完成 Control/Relay Ready；
9. 执行真实网络和备份恢复演练。

不存在原地迁移、混合集群或回退到历史状态。实施切换失败时只能：

- 在产生任何新状态前放弃本次初始化；或
- 从与外部 witness 精确匹配的 v3 完整备份恢复；或
- 废弃不可信状态，以新 Root/TrustDomainId 全量重建。

## 受影响文档

Design-only 只修改本文、ADR 0004 和允许的导航索引。后续 Implementation 必须同步：

- `architecture.md`、`protocol.md`、`configuration.md`、`security-model.md`；
- `deployment.md`、`troubleshooting.md`、`compatibility.md`、`releasing.md`；
- 根 README、英文摘要、配置与容器示例、Linux E2E 说明；
- `CHANGELOG.md` 和所有受影响测试。

Implementation 不得新增 verification record 或提升验证/发布状态。代码合入后只能写
“Implemented, gates incomplete”；全部 Gate 必须针对已合入 target commit 重新执行，
再由 Evidence-only 追加不可变记录。

## 未决问题

没有留给实现阶段的公共接口、状态机、安全、恢复、测试阈值或兼容决策。维护者可以在
Draft 评审中接受、拒绝或要求修改本文；任何正文修改都必须在 Approved 前完成。本文
仍为 Draft、ADR 0004 仍为 Proposed 时，不得开始 `V3-R1` Implementation。
