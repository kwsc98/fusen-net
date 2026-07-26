# Stellaris 节点身份、信任域与地址分配演进计划

> **文档适用性：Proposed；适用范围：候选 v3；设计评审状态：Draft；ADR 决策状态：
> Proposed；交付状态：Not started；验证状态：Unverified；发布状态：Unreleased。**
> 目标版本：`0.4.0-alpha.1`（候选，ADR 接受时冻结）。
> 方案快照：2026-07-26。
> 设计批准：N/A。
> 关联 ADR：docs/adr/0004-durable-node-identity-and-address-leases.md。
> 最后核对：working tree（2026-07-26）。
> 本文先说明
> `0.3.0-alpha.1` 的真实行为，再定义一份
> 候选的破坏式 v3 改造计划。当前实现仍以 [`architecture.md`](architecture.md)、
> [`protocol.md`](protocol.md)、[`configuration.md`](configuration.md) 和
> [`security-model.md`](security-model.md) 为准。本文中的 NodeUID、动态地址租约、
> 离线 Root CA 和在线 Intermediate CA 是 Proposed v3；多信任域是 Conditional 后续
> 方向。它们均不能描述为当前能力。
> 当前、Proposed 与后续方向的一页总览见 [`design-overview.md`](design-overview.md)。

## 目标与非目标

近期推荐继续采用单信任域和中心签发服务，但把身份、名称、地址、密钥、证书和会话
拆成不同生命周期：

```text
TrustDomainId 密码学信任域
NodeUID       永久内部身份
NodeName      可读且域内唯一的名称
AddressLease  持久 overlay IP 使用权
NodeKey/SPKI  当前授权公钥
Certificate  对 UID、IP 和 SPKI 的短期签名证明
Session       每次 QUIC 连接的临时状态
```

中心签发不等于中心数据转发。Server 可以统一 enrollment、签发和授权，Agent 之间的
Ready P2P 数据仍然端到端直连。对于当前单组织、最多 256 节点的目标，引入区块链、
DID、PoW/PoS、全局 BFT 或零知识成员证明没有直接收益。

本轮目标是让节点在改名、续期、换钥、重启和地址回收期间保持可审计的长期身份，并让
每次地址分配、签发和恢复都有唯一、持久、可拒绝陈旧状态的语义。NAT 穿透、多信任域、
HA、ACL、IPv6 和 Relay 端到端加密不进入本轮；完整清单见[首轮非目标](#首轮非目标)。

## 问题与证据

当前 v2 的 `node_id` 同时承担名称和安全身份，静态注册表又预先绑定 overlay IP；这使
改名、换钥、地址回收和审计生命周期互相耦合。当前节点 CA 私钥由在线 Server 直接
持有，协调状态没有 generation 或旧快照回滚检测，正式撤销和地址管理也没有管理员
入口。这些事实分别以 [`architecture.md`](architecture.md)、
[`configuration.md`](configuration.md) 和 [`security-model.md`](security-model.md) 为准。

QUIC/TLS 只能证明连接方持有某把私钥，不能单独裁决永久节点身份、当前授权 SPKI、
overlay IP 所有权或撤销状态。当前 v2 门禁还没有证明 host candidate 排除了 TUN/overlay
地址，也没有真实 underlay 路由证据；可复用缺口以
[`distributed-network-plan.md`](distributed-network-plan.md#v3-决策前的可复用基线)
中的 `V2-B*` Gate 为准。上述问题共同构成本次 v3 设计的输入，不能用未来计划反向覆盖
当前事实。

## 概念边界

| 概念 | 含义 | 是否是秘密 |
| --- | --- | --- |
| NodeUID | 节点不可变内部标识；拟由首次 enrollment 生成 | 否 |
| NodeName | 管理员和配置使用的可读名称 | 否 |
| SPKI | DER 编码的节点公钥信息；其 SHA-256 用作指纹 | 否 |
| NodeKey | 节点私钥，证明对 SPKI 的持有 | 是 |
| Overlay IP | TUN 使用的虚拟地址 | 否 |
| Underlay address | 云私网、公网或局域网候选地址 | 否 |
| Certificate | CA 对节点身份、公钥、地址、用途和期限的签名声明 | 否 |
| Session ID/incarnation | 防止旧连接更新或清理新连接的临时版本 | 否 |
| TrustDomainName | 用于配置、issuer manifest 和运维展示的规范化域名 | 否 |
| TrustDomainId | 对节点 Root CA SPKI 做域分离 SHA-256，标识密码学信任域 | 否 |

身份、地址和证书不能互相替代。证书是可过期、可续期的凭据；IP 是可重新分配的网络
资源；名称是人类可读标签；真正的长期引用应使用
`TrustDomainId + NodeUID`，在单信任域内部才可简写为 NodeUID。

## 当前 v2 事实

### Server 与 Agent 角色

当前不是每个节点都运行 Stellaris Server：

```text
                          one logical Server
             enrollment / control / trusted Relay / node CA
                         /                        \
                        /                          \
          Agent A: TUN + Hybrid P2P ===== Agent B: TUN + Hybrid P2P
                     listen + dial              listen + dial
```

Agent 的 Hybrid Quinn endpoint 同时监听和拨号，因此它能在某一条 P2P QUIC 连接上
充当客户端或服务端，但它不能签发节点证书、管理注册表、决定地址或充当全网协调服务。

### QUIC、TLS、认证与授权

QUIC 内置 TLS 1.3，所以同一条 QUIC 链路不需要再套 TLS 或重复加密 Datagram。但
Relay 回退由 Agent -> Relay 和 Relay -> Agent 两条独立 QUIC 链路组成，并不自动提供
Agent 到 Agent 的端到端机密性。无论哪种路径，QUIC 加密本身也不会回答以下问题：

- 连接者属于哪个节点；
- 节点是否允许使用某个 overlay IP；
- token 是否已经消费，SPKI 是否仍被授权；
- Relay 是否绑定当前 control lease；
- P2P 对端是否匹配协调服务发布的 descriptor。

因此当前链路仍需要证书认证和 Stellaris 授权：

| 链路 | QUIC TLS 身份 | Stellaris 授权 |
| --- | --- | --- |
| Enrollment | Agent 验证 Server service certificate | 一次性 token、CSR proof-of-possession |
| Control | 双方 TLS，Agent 出示节点证书 | 静态 node/IP、当前 SPKI、enabled、撤销和 session |
| Relay | 双方 TLS，Agent 出示同一节点证书 | 当前 control lease、incarnation、Ready |
| P2P | 节点间 mTLS | descriptor 指纹、session、incarnation、plan、Ready |

### 当前一个节点的凭据

稳态下，每个 Agent 只有一把 P-256 节点私钥和一张当前 leaf 节点证书。Control、Relay
和 P2P 复用这套身份；证书 profile 同时包含 `ClientAuth` 和 `ServerAuth`。续期期间旧、
新证书及其连接短暂重叠，但二者使用同一节点私钥和同一 overlay IP。

Agent 还保存两个公开信任材料，它们不是节点身份证书：

```text
node-key.pem             唯一节点私钥
node leaf certificate   Control / Relay / P2P 共用
node CA certificate     验证同信任域节点
deployment CA bundle    验证协调 Server
```

Server 的 service certificate 用于 enrollment、control 和 Relay 三个 listener；节点
CA 是另一条信任链。当前节点 CA 是 `server init` 创建、由 Server 在线持有私钥的
自签名 CA，不是离线 Root/在线 Intermediate 层级。Agent 永远不取得任何 CA 私钥。

### 当前 node ID 唯一性

当前 `node_id` 同时承担可读名称和密码学身份，唯一性由中心 Server 裁决：

1. `StaticNodeRegistry` 启动时拒绝重复 node ID、overlay IP 和 token 摘要；
2. enrollment 要求 node ID 与其独立 token 完全匹配；
3. Server 覆盖 CSR 请求的身份，把注册表中的 node ID/IP 签入证书；
4. `CoordinatorStore` 保留 node ID/IP 和授权 SPKI，冲突绑定拒绝；
5. Control/Relay 在 Server 侧必须同时匹配证书、注册表和持久状态；P2P 验证节点证书
   后再匹配协调服务经已认证 control channel 发布的 descriptor。当前 descriptor 没有
   独立数字签名。

比较是大小写敏感的，因此当前 `edge-a` 与 `Edge-A` 是两个不同标识。在线证书续期可
短暂存在同一逻辑节点的 current/staged 会话；节点唯一不等于永远只能有一条连接。

### 当前 overlay IP 分配

v2 没有地址池、DHCP、租约或动态管理 API。管理员在 `nodes.toml` 中为每个 node ID
静态填写唯一 IPv4：

```toml
[[nodes]]
id = "edge-a"
ipv4 = "10.88.0.2"
enrollment_token_sha256 = "sha256:<digest>"
enabled = true
```

Agent 配置不包含 overlay IP。enrollment 根据 node ID/token 查出静态地址，将其写入
节点证书和持久状态；Agent 随后使用签名地址创建 TUN。节点断线或证书过期不会释放
地址。已经进入持久状态的 node ID/IP 不能原地改绑；修改静态表造成不一致时 Server
启动失败。

静态节点表只在 Server 启动时读取。v2 没有正式的 `node disable/revoke` 管理 CLI 或
远程管理 API；内部 store/coordinator 虽有撤销状态机，但还不是完整运维入口。重启后，
`enabled` 影响新的 enrollment/control/Relay，token 摘要只影响新的 enrollment，不改变
当前授权 SPKI。二者都不会让已经签发的证书在失联的 P2P 上即时失效。

云主机私网、公网和 LAN IP 属于 underlay，由云平台或操作系统分配。Agent 只枚举
可用 host candidate 并上报，不把它们当作 overlay 身份。

### 当前单信任域

单信任域表示所有节点接受同一个部署组织的根信任、准入和授权决定。它不表示只有一张
CA 证书：部署 TLS CA 与节点 CA 可以分离，但仍由同一管理边界控制。

当前协调服务、节点 CA 和可信 Relay 都在信任边界内。完整 Server（在线节点 CA 私钥
加授权状态）失陷后可签发并授权任意节点；只泄露 CA 私钥时，当前 SPKI 绑定和
descriptor 仍是额外约束，但系统不把该场景视为安全。Relay 可看到回退 IPv4 包。
互不信任的组织不能安全地只靠“再信任一张 CA”加入同一 overlay，因为名称、地址
所有权、ACL、撤销传播和冲突裁决都尚未定义。

## 身份、安全与信任边界

以下模型只针对一个组织控制的单信任域。中心 Server、在线 Intermediate、授权状态和
可信 Relay 仍在信任边界内；它改善身份与恢复语义，但不声称抵御恶意控制器或隐藏
Relay 回退包。

### 持久身份模型

v3 建议采用以下权威记录：

```rust
struct TrustDomainRecord {
    name: TrustDomainName,
    id: TrustDomainId,
    manifest_epoch: u64,
    active_issuer_spki_sha256: BTreeSet<Sha256Fingerprint>,
}

enum NodeLifecycle {
    Active,
    Disabled,
    Revoked,
    Released,
}

struct NodeRecord {
    trust_domain: TrustDomainId,
    node_uid: Uuid,
    node_name: String,
    name_epoch: u64,
    lifecycle: NodeLifecycle,
    state_epoch: u64,
    overlay_ip: Option<Ipv4Addr>,
    allocation_epoch: u64,
    authorized_spki_sha256: Option<Sha256Fingerprint>,
    key_epoch: u64,
    revocation_epoch: u64,
}
```

- `node_uid` 由中心 Server 首次成功 enrollment 时生成 UUIDv7，并永久持久化；
- `trust_domain_name` 是 v3 初始化时冻结的规范化小写 DNS 名，用于配置和 manifest；
- `trust_domain` 使用带 `stellaris trust domain v3` domain-separation tag 的规范 DER Root
  SPKI SHA-256，与 NodeUID 共同构成稳定 principal；Root key 不得跨域复用；
- Name -> Id 映射必须来自 Root 签名的 issuer manifest，不能只相信 URI 中的名称；
- `node_name` 只接受规范化小写 ASCII，在当前信任域内唯一；
- `name_epoch` 只在 NodeName 改变时递增，用于排序管理和目录展示事件；它不进入 leaf
  claims 或数据路径有效性，改名不关闭已有连接；
- `overlay_ip` 属于 NodeUID 的持久租约，不属于某一张证书；
- `allocation_epoch` 只在地址分配给新 owner 时递增；
- `state_epoch` 只在生命周期改变时递增，供授权和 descriptor 排序，不代替状态本身；
- `key_epoch` 只在授权 SPKI 成功替换时递增；
- `revocation_epoch` 只在旧凭据被明确 revoke 时递增，不能用于普通续期或密钥轮换。

`Active` 必须同时有 lease 和授权 SPKI；`Disabled` 保留二者；`Revoked` 保留 lease 但
清空 SPKI；`Released` 清空二者并永久保留审计 tombstone。待注册信息使用独立
`AdmissionRecord`，不伪装成尚无 NodeUID 的 NodeRecord。NodeRecord 与地址记录的 owner、
IP 和 allocation epoch 必须双向一致，并在同一事务中校验和提交。

UUIDv7 使用操作系统 CSPRNG 生成，store 对 NodeUID、当前 NodeName 和历史 SPKI 分别
建立唯一索引；极小概率碰撞也必须在事务内重试或 fail-closed，不能覆盖旧记录。同一
时刻每个 IP 只能有一个 Allocated owner。NodeName 改名保留旧名 tombstone，首轮不把
历史名称重新分给其他 NodeUID，审计和协议始终以 NodeUID 而不是名称关联。

证书、改名、密钥轮换、Agent 重启或 Server 重启不改变 NodeUID。只有管理员完成
`revoke -> release`，再创建新的 admission 并成功 enrollment 时，才生成新的 NodeUID；
Released 旧记录仍作为审计 tombstone 保留。

### 证书与 IP 的关系

正确关系不是“一张证书永久拥有一个 IP”，而是：

```text
one durable NodeUID address lease
             |
             +-- certificate generation 1
             +-- certificate generation 2
             +-- certificate generation 3
```

稳态下只有一张当前证书；无空窗续期允许两张证书短暂重叠，但它们必须证明同一个
NodeUID、IP 和授权 SPKI。证书过期、连接断开和短期控制面故障都不释放地址。

### 节点 CA 层级

中心签发服务对当前规模是合适的，但不应长期直接持有根 CA 私钥。推荐层级：

```text
offline Node Root CA
          |
          | signs and constrains
          v
online Node Intermediate CA
          |
          | signs 6-24 hour leaf certificates
          v
Agent node certificate
```

Server 只保存 online Intermediate 私钥。Agent 保存离线 Root 公钥和最新的 Root 签名
issuer manifest；TLS 时发送 leaf 与 Intermediate chain，验证方除验证到 Root 的链外，
还必须确认 Intermediate SPKI 在 manifest 的 active set 中。manifest 固定携带
TrustDomainName/Id、单调 `manifest_epoch`、有效期和活跃 Intermediate 指纹集合；Agent
持久化已见最高 epoch 并拒绝回滚。正常轮换可短暂同时允许新旧两个 Intermediate。

Intermediate 泄露时，Root 签发更高 epoch 的 manifest 移除旧 issuer，再由节点刷新
manifest 和 leaf。该撤销不是对离线节点即时生效：尚未获得新 manifest 的节点仍可能
信任旧 issuer，风险上限由 manifest 与 leaf 的有效期决定。Root key 本身不在 v3 域内
轮换；替换 Root SPKI 会产生新的 TrustDomainId，视为新信任域并要求全量重新初始化和
enrollment。部署 service certificate 继续使用独立 deployment CA 或公开 WebPKI。

建议节点 leaf certificate 使用标准字段：

- 恰好一个 URI SAN：`urn:stellaris:node:v3:<trust-domain-id-hex>:<node-uid>`；
- IP SAN：当前 overlay IP；
- SPKI：节点当前 P-256 公钥；
- EKU：`ClientAuth` 与 `ServerAuth`；
- TTL：首轮保持 24 小时，并由所有验证方拒绝超过 profile 上限的 leaf。

这里使用项目自己的 URI profile，不声称兼容 SPIFFE X.509-SVID。URI、UUID、十六进制、
DNS A-label 和 DER SPKI 都必须有唯一规范编码；TrustDomainId 必须由实际通过链验证的
Root SPKI 计算，不能相信 leaf 自报值。

v3 还应定义一个严格 DER 编码的 critical Stellaris node-claims extension，携带固定
版本的 TrustDomainId、签发时 manifest epoch 和 allocation/key/revocation epoch。
验证时要求 leaf manifest epoch 不大于本地当前 manifest epoch，并要求实际签发
Intermediate SPKI 仍在当前 active set；一旦 issuer 被移除，不论 leaf epoch/NotAfter
为何都拒绝。OID、DER schema 和未知版本规则必须在 `V3-P0` 冻结。Control/Relay 必须把证书
claims 与当前 TrustDomainRecord/NodeRecord 比对；P2P 必须再匹配当前 descriptor、证书
fingerprint、域、UID、IP 和 epochs。

`PeerDescriptor` 和 `ConnectPlan` 必须携带 `lifecycle + state_epoch` 以及
allocation/key/revocation epoch；`P2pHello` 必须回显并绑定计划中的同一授权 tuple。
接收方拒绝任何 lifecycle 非 Active、state epoch 陈旧或其他路径 epoch 不一致的握手。
`name_epoch` 只进入管理/目录展示事件，不进入 leaf、descriptor、plan、Hello 或数据路径
缓存键，改名不得使已认证路径失效。

单 leaf/私钥复用简化了续期和 P2P 双向握手，但节点私钥泄露会同时暴露 Control、Relay
和 P2P。四类 ALPN、逐用途 verifier 和 Ready 状态机仍必须独立；v3 首轮禁用 0-RTT 和
节点链路 TLS session resumption，防止旧 key/status/allocation epoch 被恢复会话绕过。

## 状态机、持久化和失败语义

### 动态地址租约

`CoordinatorStore` 成为地址租约权威。地址池必须排除网络地址、广播地址和显式保留
地址，并永久保留每地址的 epoch 与签发期限水位，即使地址释放也不能丢弃 ABA 历史：

```rust
enum AddressLeaseState {
    Free,
    Reserved { admission_id: AdmissionId },
    Allocated { owner: NodeUID },
    Quarantined {
        previous_owner: NodeUID,
        not_reusable_before: UnixSeconds,
    },
}

struct AddressLeaseRecord {
    overlay_ip: Ipv4Addr,
    state: AddressLeaseState,
    allocation_epoch: u64,
    last_issued_not_after: Option<UnixSeconds>,
}
```

可选固定地址在 NodeUID 生成前使用 admission ID 持久保留。`allocation_epoch` 初始为 0，
只在地址分配给新 owner 的事务中 checked-increment；release 不重复递增，溢出时
fail-closed。系统时钟早于持久水位且超出允许偏差时，签发和地址复用都必须停止。

首次 enrollment 的原子事务为：

```text
validate admission + token + CSR PoP
               |
generate NodeUID and choose/reserve IP
               |
issue certificate and build exact response
               |
persist NodeRecord + lease + token consumption + idempotent response
               |
reply only after file fsync + rename + directory fsync
```

相同 enrollment ID、token 和 CSR 重试必须返回完全相同的 UID、IP 和证书。并发注册
在同一事务锁内串行分配，任何持久失败都不能确认地址。

首次 enrollment、普通续期、re-enrollment 和换钥签发都必须在发送任何证书字节前，
用同一事务执行
`last_issued_not_after = max(last_issued_not_after, new_leaf.NotAfter)` 并持久化精确幂等
响应；幂等 replay 只读取原结果，不再次改变水位。

地址只允许显式 `release`，且节点必须已经 `Revoked`。release 先原子持久化 Released
NodeRecord、lease detach 和 Quarantined tombstone，提交内存并应答后，在线 runtime
才可 best-effort 关闭可达连接。首轮本地 CLI 要求 Server 已停止，因此只能在下次启动
后传播撤销，不能声称已关闭失联 Agent 之间的 P2P。

由于原始 IPv4 Datagram 不携带 epoch，失联节点上的旧 P2P 不能只靠在线撤销立即
消失。`not_reusable_before` 至少为
`max(last_issued_not_after, now) + allowed_clock_skew`；若 issuer 泄露或恢复状态无法证明
水位最新，则至少使用 `now + max_leaf_ttl + allowed_clock_skew`。隔离结束后才能将地址
变回 Free。新 owner 使用更高 allocation epoch，PeerManager 缓存和延迟清理必须按
`(TrustDomainId, NodeUID, IP, allocation_epoch)` 失效，不能只按 IP。

所有持久变更统一遵循“构造并校验 next state -> 临时写与文件 fsync -> 原子替换 ->
目录 fsync -> 提交内存 -> 应答 -> 运行时副作用”。原子替换只防 torn write，不防旧
快照回滚；备份恢复语义在发布门禁中单独处理。

地址池容量必须从实际可用 host 地址计算，而不是只看 `max_nodes`。IPv4 `/24` 通常
只有 254 个普通 host 地址，若验收需要 256 个同时分配的节点，示例至少使用 `/23`，
并校验 `max_nodes <= 可分配地址数`。

### 管理入口

首轮不需要开放远程管理 API。可以增加 server-local 管理 CLI，并用独占 store lock
保证 Server 运行时无法离线修改同一状态：

```text
stellaris node create
stellaris node rename
stellaris node disable|enable
stellaris node revoke
stellaris node rotate-token
stellaris node release
stellaris agent re-enroll --config <path>
```

`node create` 写入待 enrollment admission 并安全生成一次性 token；可选固定地址用于
基础设施节点，未指定时 enrollment 从池中分配。管理命令在 Server 持有状态锁时直接
失败，变更后由 Server 重新启动并重新认证在线 session。在线管理 API 另立 ADR。

`node rename` 只替换规范化 NodeName、递增 name epoch 并保留旧名称 tombstone，不改变
NodeUID、lifecycle/state epoch、SPKI、证书、地址 lease 或现有连接。

`disable` 阻止新认证但保留 UID、SPKI 和地址 lease，`enable` 可恢复它；`revoke` 还会
清除授权 SPKI、递增 revocation/state epoch，保留 UID/地址但必须重新 enrollment 才能
恢复。由于入口是停服修改，以上状态要在 Server 重启和节点重新接入控制面后传播；
失联 P2P 最迟仍要等 leaf 到期。

`node rotate-token` 为已有 NodeUID 创建一次性 replacement admission，但此时不改变当前
SPKI/epoch。管理员通过安全的带外通道把 token 文件送到 Agent v3 配置中
`identity.replacement_token_file` 指向的位置，停止 Agent 后运行 `agent re-enroll`。

Agent 持有 identity store 独占锁，在首次发送前原子持久化 staged private key、CSR、
enrollment ID、token 绑定和完整请求字节。超时、断线、崩溃或响应丢失都保留 staged
状态并原样重试；只有经过认证、协议明确保证未提交的终止错误才能丢弃它。收到幂等
成功响应后，Agent 验证完整结果，原子替换本地 key/leaf，再清除 staged 状态。因此
Server 已提交而响应丢失时仍能取回同一证书，不会只剩已失效的旧 key。

Server 对 Active 节点换钥时保持 Active，只递增 key epoch、替换 SPKI 并提升证书期限
水位；对 Revoked 节点恢复时还必须执行 `Revoked -> Active` 并递增 state epoch，使用
revoke 时已经递增的 revocation epoch。两种事务提交后，旧认证会话都不再获得逐消息
授权并被关闭。明确的 pre-commit 拒绝保留原身份；不确定结果必须走上述幂等恢复。
`release` 只接受 Revoked 节点，是不可逆地址回收，不能再用 `enable` 恢复原 lease。

### 备份、恢复与回滚

Coordinator state、admission、issuer manifest、在线 Intermediate 和配置必须作为同一
`state_generation` 备份，并在恢复时要求操作员提供外部记录的预期 generation。原子
文件替换无法识别一份结构正确但已经过时的全量快照；没有外部 witness、HSM 单调计数
或最新 generation 记录时，系统不能自动区分合法恢复与恶意回滚。

generation 无法证明时 Server 必须进入 recovery mode，禁止签发、路由和地址复用。
安全恢复至少要撤销所有旧 admission/SPKI、由离线 Root 发布新 issuer manifest，并将
整个地址池隔离 `max_leaf_ttl + allowed_clock_skew` 后重新 enrollment；无法保留可信
NodeUID/lease 历史时，应创建新的 TrustDomainId 做全量重建，而不是猜测旧状态仍有效。

## 多信任域远景

真正的多信任域不是简单加载多个节点 CA，而是没有任何组织能单方面代表全网。一个
候选的全局身份为：

```text
DomainID = TrustDomainId
         = SHA-256("stellaris trust domain v3\0" || canonical DER root SPKI)
DomainLocalNodeUID = v3 domain-issued immutable UUIDv7
GlobalNodeID = DomainID + DomainLocalNodeUID
```

该远景沿用 v3 已定义的 UUIDv7 NodeUID，不重新解释同一标识。若后续需要基于 device
root SPKI 的自认证设备标识，必须通过新的 ADR 引入不同类型，并定义它与 NodeUID 的绑定、
换钥和恢复语义。

每个域独立运行根信任、成员签发、地址前缀和策略控制器；跨域通过明确 trust bundle、
双方 capability 和策略交集互联。peer record 和授权声明可验证签名后，发现目录和 DHT
可以是不可信的。Relay 只有在另行实现节点间 overlay 端到端加密后才能降为不可信
转发者；签名目录本身不能隐藏 Relay 上的包内容。

长期完整模型还需要：

- 每域独立 IPv6 overlay prefix 及可验证前缀所有权；
- NodeUID 与 underlay locator 分离，支持移动和多路径；
- 域内 Controller HA，普通故障使用 Raft，恶意控制器威胁才使用 BFT/阈值签名；
- 离线 Root、短期 Intermediate、硬件或 TPM 根密钥；
- 双边 ACL/capability、撤销传播和冲突处理；
- Relay 只转发节点间端到端密文；
- 签发和撤销 transparency log，检测控制面 equivocation。

多信任域存在不可消除的权衡：允许离线认证就不能保证即时撤销；要求即时撤销就依赖
在线控制面；全局动态 IPv4 需要中心裁决或共识；抵御开放网络 Sybil 还需要组织准入、
费用、stake、硬件证明或其他稀缺资源。PoW/PoS 解决的不是普通 mTLS 节点认证。

上述多域能力不进入首轮 NodeUID/租约改造。真正实施 federation、IPv6 前缀或 Relay
端到端加密时必须分别新增 ADR 和威胁模型。

## 公共接口和版本边界

如果 [ADR 0004](adr/0004-durable-node-identity-and-address-leases.md) 被接受，建议一次
切换到 schema、持久状态和线协议 v3：

- ALPN 直接使用 `stellaris/enroll/3`、`stellaris/control/3`、
  `stellaris/relay/3`、`stellaris/p2p/3`；
- 删除 v2 listener、codec、证书 profile、静态地址 registry runtime 和兼容 adapter；
- v2 配置、Server 状态、Agent identity 和节点证书全部拒绝；
- 不提供状态迁移器、双栈 listener、协议降级、旧证书重新签发或兼容 feature；
- 切换时重新初始化 v3 PKI/状态并重新 enrollment 所有 Agent；
- 中间实现不得发布为 v2/v3 混合可用版本。

已经提交的 v2 不能在保持版本号不变时静默改变含义。候选 release 目标为
`0.4.0-alpha.1`；最终编号在 ADR 接受时冻结，公共格式必须升为 v3。

`V3-P0` 还必须冻结 v3 Server/Agent 配置 schema、上述本地 `stellaris node ...` 与
`stellaris agent re-enroll` 命令的精确参数，以及 enrollment/control/Relay/P2P 的消息
字段。正文未冻结前，这些候选名称不得写入 Current 配置示例或部署步骤。

## 实施阶段与退出门禁

| 阶段 ID | 阶段 | 主要工作 | 退出门禁 |
| --- | --- | --- | --- |
| `V3-P0` | 候选规范与决策冻结 | 冻结 TrustDomainName/Id、NodeUID、生命周期/epoch、issuer manifest、URI/claims、证书 profile、地址隔离、恢复和 Agent/Server 管理入口；随后将 ADR 0004 评审为 Accepted 或 Rejected | 不再有未决身份语义；只有 ADR Accepted 且详细设计 Approved 才允许进入 `V3-P1` |
| `V3-P1` | v3 类型与协议 | 新增强类型 UID/Name/lifecycle/epoch；重写 v3 消息、ALPN、配置和严格 codec；禁用 0-RTT/resumption；删除 v2 surface | 全部 v2 输入拒绝；方向、重复字段、边界、恢复会话和 fuzz 通过 |
| `V3-P2` | 持久状态与租约 | v3 `CoordinatorStore`、admission/reservation、地址状态机、水位/clock/generation、tombstone、独占锁和本地管理 CLI | 跨记录不变量、并发唯一性、容量、重启、幂等、回滚和持久故障点通过 |
| `V3-P3` | PKI 层级 | 离线 Root/在线 Intermediate 工具、issuer manifest 和 profile；NodeUID URI SAN、IP SAN、链验证 | 错误根/中间链、manifest 回滚、UID/IP/SPKI、用途、期限和轮换测试通过 |
| `V3-P4` | Server/Agent 闭环 | enrollment 分配 UID/IP；control/Relay 使用 `AuthenticatedNode`；Agent v3 identity/TUN 与显式 re-enroll/key rotation | 两 Agent Relay 双向 IPv4、续期、换钥、重启、禁用/撤销和旧凭据拒绝 |
| `V3-P5` | P2P 与回收 | descriptor/plan/Hello 携带域、UID、`lifecycle + state_epoch` 和 allocation/key/revocation epoch，明确排除 name epoch；按完整 lease tuple 释放/复用；保持单路径语义 | LAN P2P、同时拨号、陈旧 lifecycle/state epoch、地址 ABA、撤销传播/期限边界、无重复包通过 |
| `V3-P6` | 发布门禁 | 文档、部署、备份/回滚恢复、真实 Linux namespace、容量和 soak | Linux 全门禁、`/23` 的 256 个持久 lease 状态边界，以及两/多 Agent 至少 30 分钟连接稳定性通过 |

阶段 ID 用于排期，以下原子 Gate ID 才用于单独记录 Passed/Failed/Partial。关闭阶段要求
该阶段所有原子 Gate 都有目标 commit 证据；后续增加门禁时只能新增 ID，不能改变已有 ID
的含义。

| 原子 Gate ID | 可独立判定的结果 | 初始状态 |
| --- | --- | --- |
| `V3-P0-SPEC-01` | 候选编码、状态机、PKI、管理、恢复及破坏边界无未决项，ADR 0004 Accepted，详细设计 Approved 并记录批准 commit | Open |
| `V3-P1-WIRE-01` | 四类 v3 ALPN、消息方向、固定头、大小和严格字段测试通过 | Open |
| `V3-P1-REJECT-V2-01` | v2 protocol/config/state/identity、0-RTT、resumption 和降级输入全部 fail-closed | Open |
| `V3-P1-FUZZ-01` | v3 严格 codec 的边界、重复字段、异常输入和规定 fuzz 门禁通过 | Open |
| `V3-P2-INVARIANTS-01` | Node/admission/lease 双向不变量、唯一性、epoch 和状态转换测试通过 | Open |
| `V3-P2-PERSIST-01` | 临时写、文件 fsync、rename、目录 fsync、内存提交各故障点不提前确认 | Open |
| `V3-P2-RECOVERY-01` | 独占锁、generation、旧快照拒绝和 recovery mode 行为通过 | Open |
| `V3-P2-CAPACITY-01` | `/23` 中 256 个持久 lease 可分配且 `/24` 的错误容量在启动前拒绝 | Open |
| `V3-P3-CHAIN-01` | Root/Intermediate/leaf profile、URI/IP SAN、用途、期限和错误链测试通过 | Open |
| `V3-P3-MANIFEST-01` | manifest 签名、期限、单调 epoch、回滚及 active issuer 移除测试通过 | Open |
| `V3-P3-ROTATION-01` | 双 Intermediate overlap、移除旧 issuer 和 Root 替换等同新信任域的演练通过 | Open |
| `V3-P4-ENROLL-01` | 两 Agent 首次 enrollment 获得不同 UID/IP，token/CSR/幂等和提交后响应丢失测试通过 | Open |
| `V3-P4-RELAY-01` | 两 Agent 经 control-bound Ready Relay 完成双向 IPv4、源校验和 session ABA | Open |
| `V3-P4-RENEW-01` | 短期 leaf 续期、替代 control/Relay、到期关闭和重启恢复通过 | Open |
| `V3-P4-REKEY-01` | Active/Revoked re-enroll 换钥、不确定提交恢复和旧 SPKI 拒绝通过 | Open |
| `V3-P4-REVOKE-01` | disable/enable/revoke/release 的授权、传播和期限边界符合状态机 | Open |
| `V3-P5-P2P-01` | 两/多 Agent LAN P2P、同时拨号仲裁、空闲回收和断线回退通过 | Open |
| `V3-P5-STALE-01` | 陈旧 lifecycle/state/allocation/key/revocation epoch 的 descriptor/plan/Hello 全部拒绝，name epoch 改变不失效路径 | Open |
| `V3-P5-ABA-01` | 地址隔离、重新分配和延迟连接/清理不能影响新 owner | Open |
| `V3-P5-PATH-01` | 包 ID 证明单路径、无主动重复包/环路且失败 P2P 包不补发 | Open |
| `V3-P6-LINUX-01` | Linux namespace 中 ping/TCP/UDP、Server/Agent 重启和路由回滚通过 | Open |
| `V3-P6-SOAK-01` | 两/多 Agent 至少 30 分钟连接抖动中资源不持续增长 | Open |
| `V3-P6-DOCS-01` | Current 规范、部署/恢复、支持矩阵、链接和发布检查全部通过 | Open |

每个阶段只能有一个权威身份模型。`V3-P1` 开始后不得通过 feature flag 恢复 v2；
`V3-P6` 完成前，文档不得把动态地址、离线 Root 或 NodeUID 描述为已支持。

## 测试与验收

### 身份与协议

- NodeUID CSPRNG/碰撞处理、NodeName 小写规范/历史 tombstone、UID/Name/SPKI 唯一性和
  活跃 IP lease 唯一性；
- enrollment token 错误、消费、并发竞争、CSR PoP、幂等冲突，以及 Server 提交后响应
  丢失时 staged Agent 请求原样恢复；
- Active/Disabled/Revoked/Released 转换，state/name/key/revocation epoch 各自单调且
  不混用；改名不改变证书或路径有效性；
- Root/Intermediate/leaf 链、issuer manifest 签名/期限/回滚、URI SAN、IP SAN、EKU、
  期限和算法 profile；
- 双 issuer overlap、旧 leaf epoch 接受、未来 leaf epoch 拒绝，以及 issuer 移出 active
  set 后旧 leaf 无条件拒绝；
- Control/Relay/P2P 的域、UID、IP、SPKI、lifecycle/state epoch 及
  manifest/key/revocation/allocation epoch 不匹配；name epoch 改变不使路径失效；
- 错误 `/2` ALPN、v2 frame、v2 config/state/identity、0-RTT、session resumption 和所有
  降级尝试拒绝。

### 地址与持久化

- 地址池网络/广播/保留地址排除，可选固定地址冲突、CIDR 容量和池耗尽；`/23` 可分配
  256 个并发持久 lease，`/24` 配置 `max_nodes = 256` 必须在启动前拒绝；
- 并发 reservation/enrollment 不重复分配，失败事务不泄漏租约；
- 首次签发、续期、换钥和幂等 replay 的 NotAfter 水位不回退，系统时钟回退 fail-closed；
- 重启保持 UID/IP/epochs，断线、到期和续期不释放地址；
- disable、revoke、release、证书期限隔离、tombstone 和重新分配后的旧状态拒绝；
- 临时写、文件 fsync、rename、目录 fsync 和内存提交各故障点不提前确认；
- 完整 generation 恢复、旧快照拒绝和无法证明最新时的 recovery mode。

### 真实网络

- 两个 Linux Agent 首次注册得到不同 UID/IP，经 Relay 完成 ping/TCP/UDP；
- LAN P2P 建立、Relay/P2P 切换、断线回退和单包不补发；
- Agent/Server 重启、证书续期、Agent re-enroll/SPKI 轮换、Revoked 恢复、Intermediate
  撤销/轮换和地址复用；
- `/23` 中 256 个持久 lease 状态边界；两/多 Agent 至少 30 分钟连接稳定性 soak。

`V3-P6` 不要求 256 个 Agent 同时在线；完整 256 在线节点资源 soak 属于
Conditional `0.6`。

## 首轮非目标

- 多信任域 federation、跨域 ACL/capability 和全局名称；
- 多 Controller HA、Raft/BFT、阈值 CA 和 transparency log；
- TPM/Secure Enclave/远程 attestation；
- IPv6 prefix delegation、NAT 穿透和 Multipath QUIC；
- Relay 路径端到端加密和元数据隐藏；
- 公网开放注册、PoW/PoS、DID、区块链或零知识成员证明；
- s2n-quic/gm-quic 的 v3 runtime。

这些方向有独立价值，但不能阻塞先把单信任域的身份、地址和证书生命周期做正确。

## 部署、恢复与回滚

v3 切换是停机、全量重新初始化流程，不是滚动升级：先冻结并备份 v2 仅供审计，停止
全部 v2 Server/Agent，再离线生成 Node Root、签发在线 Intermediate 和首份 issuer
manifest，初始化空 v3 store，创建 admission，最后逐节点重新 enrollment。部署前必须
验证 Root/Intermediate、manifest、地址池容量、state generation、文件权限和备份恢复；
任何 v2 配置、状态、证书或 identity 被读入时都应失败关闭。

在第一个 v3 enrollment 提交前，可以停止切换并重新启动完整、未修改的 v2 部署。在
已有 v3 状态后不提供原地降级或状态转换；若必须回到 v2，只能再次停机、废弃 v3
trust domain、重新初始化一套全新的 v2 状态并重新 enrollment 全部节点。正常恢复只
接受能够证明 generation 最新且与 Root/manifest 一致的整套 v3 备份；无法证明时按上文
进入 recovery mode 或创建新的 TrustDomainId。

## 受影响文档

实现每个阶段时必须按 [`documentation-workflow.md`](documentation-workflow.md) 同步：

- `protocol.md`、`configuration.md`、`architecture.md` 和 `security-model.md`：v3 成为
  唯一 Current 契约时一次性替换 v2 定义；
- `deployment.md`、`troubleshooting.md`、容器示例和 `tests/e2e/`：同步初始化、权限、
  重新 enrollment、恢复和真实网络命令；
- `compatibility.md`、`README.md`、`README.en.md`、`roadmap.md` 和 `CHANGELOG.md`：只按
  实际交付与目标 commit 证据更新状态，不提前声明支持；
- `adr/README.md` 与 `verification/`：分别记录最终决策和不可变 Gate 结果。

## 未决问题

以下内容必须在 `V3-P0-SPEC-01` 前冻结，目前不得视为已批准接口：

- TrustDomainName 规范、NodeUID/URI/DER/OID 的唯一编码和允许的密码算法集合；
- issuer manifest 的规范格式、签名、有效期、时钟偏差、epoch 和分发/恢复规则；
- 地址选择顺序、固定 reservation、隔离公式、池耗尽和系统时钟回退行为；
- server-local 管理命令、Agent re-enroll 参数、独占锁、幂等请求和操作员确认语义；
- state generation 的外部见证方式、备份组成、recovery mode 的退出条件和全量重建步骤。

任一项仍未决时，本文保持 Draft、ADR 0004 保持 Proposed，且不得开始 `V3-P1` 实现。
