# ADR 0004：V3-R1 单一节点身份、分层 PKI、持久租约与可信 Relay 闭环

- 文档适用性：Proposed
- 适用范围：候选 V3-R1 Identity & Relay Core（`0.4.0-alpha.1`）
- 设计评审状态：N/A
- ADR 决策状态：Proposed
- 交付状态：Not started
- 验证状态：Unverified
- 发布状态：Unreleased
- 日期：2026-07-26

## 背景

节点长期身份、可读名称、当前公钥、overlay IPv4、短期证书和运行会话具有不同的
生命周期。把这些概念合并为一个配置标识，或把短期证书当作地址所有权，会使改名、
换钥、撤销、地址复用和恢复互相耦合。QUIC/TLS 只能证明连接方持有某把私钥，不能独立
裁决节点的长期身份、当前授权公钥、地址所有权或持久撤销状态。

V3-R1 需要以一套全新的单一契约同时闭合 enrollment、Control、可信 Relay、分层 PKI、
地址租约和崩溃恢复。该契约不从其他协议或持久格式继承身份语义，也不在本阶段引入直连
数据路径。详细接口、阶段和原子 Gate 由
[`../node-identity-trust-addressing-plan.md`](../node-identity-trust-addressing-plan.md)
冻结。

## 提议决策

### 范围与数据路径

1. V3-R1 是单组织、单租户、单信任域、单 Coordinator、IPv4 的新运行边界，最多管理
   256 个持久节点；Linux 是唯一运行验收平台。
2. 可信 Relay 是唯一 overlay 数据路径，不是回退路径。Agent 不发布 underlay candidate，
   不协商其他路径，也不保留对应的配置、ALPN、消息、状态或运行时占位。
3. Relay 位于信任边界内，可以看到转发的 IPv4 包和流量元数据。V3-R1 不抵御恶意
   Coordinator、在线 issuer 或 Relay，也不提供 Relay 路径的端到端机密性。

### 身份、名称与生命周期

4. 全局稳定 principal 固定为 `TrustDomainId + NodeUid`。运行时授权入口统一传递
   `AuthenticatedNode`，不得用裸名称或裸 IP 代替已认证身份。
5. `TrustDomainName` 只接受 1..253 字节、已经规范化的小写 DNS A-label 编码；大写、
   Unicode 和尾点直接拒绝。`TrustDomainId` 固定为：

   ```text
   SHA-256("stellaris trust domain v3\0" || canonical DER Root SPKI)
   ```

   Root key 不得跨信任域复用；替换 Root key 会创建新的信任域，而不是域内轮换。
6. 中心 Server 在首次成功 enrollment 时使用操作系统 CSPRNG 生成永久 UUIDv7
   `NodeUid`，并由持久 store 的唯一约束裁决碰撞。
7. `NodeName` 是 1..63 字节的单个 DNS label；首尾只能是小写 ASCII 字母或数字，
   中间字符还可以使用连字符。它只用于管理和展示；域内当前名称唯一，所有历史名称
   永久保留 tombstone，不再分配给其他节点。
8. 生命周期固定为 `Active <-> Disabled`、`Active|Disabled -> Revoked`、
   `Revoked --re-enroll--> Active` 和 `Revoked -> Released`；`Released` 是永久终态。
   重复执行已经成立的操作必须幂等，且不增加任何 epoch。真正发生的 disable、revoke
   或 release 转换必须原子取消当时已有的 Pending Replacement；幂等重复操作不得取消
   转换完成后新创建的 recovery admission，因此撤销前签发的 capability 不能恢复节点。
9. 初次 enrollment 后 `name_epoch`、`state_epoch` 和 `key_epoch` 均为 1，
   `revocation_epoch` 为 0。`allocation_epoch` 是地址的跨 owner 分配代数：首次使用
   某地址时为 1，隔离后复用时从该 lease 的上一代 checked-increment，因此可大于 1。
   所有 epoch 溢出时 fail-closed；普通证书续期不增加 epoch。
10. `AuthorizationTuple` 固定包含 TrustDomainId、NodeUid、overlay IPv4、lifecycle、
    state/allocation/key/revocation epoch 和当前 SPKI SHA-256 指纹。NodeName 与
    name epoch 不进入证书授权 claims、路由键或数据路径缓存键。

### 准入、CLI 与配置

11. 待注册信息使用独立 Admission record，不创建虚假的 Node record。首次准入和换钥
    准入都由 Server 预先绑定名称、可选固定地址或现有 NodeUid；Agent 请求不得自报
    名称或地址。
12. Admission token 是 32 字节 OS CSPRNG 随机值，编码为 `stl3_` 加无填充
    base64url；默认有效期 24 小时、最大 7 天。Server 只持久化 SHA-256 digest，token
    输出文件必须以 create-only `0600` 创建。创建命令在持有 store 锁并完整校验
    provisional next state 后，先持久化 token 文件，再提交只含 digest 的 state/witness；
    后半段失败时文件不是有效 capability，命令必须失败。这是避免提交不可恢复明文 token
    的 Normal Coordinator store mutation 唯一外部 precommit 例外，不能推广到其他正常
    mutation 或成功响应；离线 restore 使用独立的 fail-closed 恢复顺序。
13. 公开 CLI 固定为：

    ```text
    stellaris pki root init
    stellaris pki intermediate issue
    stellaris pki manifest issue
    stellaris server init|run --config PATH
    stellaris server backup --config PATH --output DIR --witness-out FILE
    stellaris server restore --config PATH --input DIR --witness FILE
    stellaris admission create --config PATH --name NAME [--fixed-ip IP] --token-out FILE [--ttl 24h]
    stellaris admission replace --config PATH --node-uid UID --token-out FILE [--ttl 24h]
    stellaris admission list|cancel --config PATH ...
    stellaris node list|show|rename|disable|enable --config PATH ...
    stellaris node revoke|release --config PATH --node-uid UID --confirm-node-uid UID
    stellaris agent enroll|re-enroll --config PATH --token-file FILE
    stellaris agent run --config PATH
    stellaris config check --config PATH
    ```

14. Server 配置只使用 `[trust_domain]`、`[network]`、`[storage]`、`[service_tls]`、
    `[listeners]`、`[limits]` 和 `[observability]`。Agent 配置只使用 `[agent]`、
    `[trust_domain]`、`[coordinator]`、`[identity]` 和 `[observability]`，不得配置
    NodeName、NodeUid、overlay IP、token 或其他数据路径。
15. `agent enroll` 创建初始身份，`agent re-enroll` 显式换钥或恢复 Revoked 节点，
    `agent run` 只加载已经提交的身份，绝不自动注册或换钥。Agent 的 enroll、re-enroll
    与 run 通过 identity lock 互斥；所有 server-local 管理 mutation 要求 Server 停止
    并取得 store 独占锁。

### 线协议与会话

16. 唯一 ALPN 是 `stellaris/enroll/3`、`stellaris/control/3` 和
    `stellaris/relay/3`。所有消息保留 12 字节 `STLR` 固定头，payload 使用
    `deny_unknown_fields` 的严格 JSON。
17. 未知 version、ALPN、message type、flags、错误方向、重复字段、非规范 ID/base64、
    超长和截断输入全部 fail-closed，不提供协议降级或替代解析路径。
18. Enrollment 消息固定为 `ManifestRequest`、`ManifestResponse`、`EnrollRequest`、
    `EnrollAccepted` 和 `Error`；首次注册与换钥共用请求格式，由 Admission 类型决定
    语义。
19. Control 消息固定为 `ControlWelcome`、`ControlReady`、`RenewCertificate`、
    `CertificateIssued`、`ManifestUpdate` 和 `Error`。Relay 消息固定为 `RelayBind`、
    `RelayAccepted`、`RelayReady` 和 `Error`。
20. Relay session 必须绑定同一 AuthenticatedNode 当前且已经 Ready 的 control session；
    绑定完成并进入 RelayReady 后才可安装路由或接收 IPv4 Datagram。Control 失效、
    lifecycle/epoch/SPKI 不再匹配或完整 Control/Relay session ID ownership 被替换时，
    绑定 Relay 必须失效，旧 session 的延迟清理不得影响新 session。TLS 握手得到的 tuple
    不是永久授权缓存；每个 Control 请求、Relay Datagram 和 route lookup 都必须与当前
    内存状态重新比对，内存提交是旧 tuple 立即失效的授权栅栏。
21. 三条 V3-R1 连接全部禁用 0-RTT 和 TLS session resumption。证书续期在有效期中点后
    加 `0..=30` 分钟 jitter 触发；Agent 先建立新的 Ready Control 与 Relay，再关闭旧
    会话。Control 和 Relay 可以在普通续期重叠期使用 AuthorizationTuple 相同的不同 leaf，
    但 pair/route 的 deadline 必须取两条 TLS 连接 leaf NotAfter 的较小值；到该期限仍未
    成功续期时，Server 与 Agent 都必须关闭对应网络会话。

### PKI 与证书

22. Node Root、Intermediate、节点 key 和 issuer manifest 全部固定使用
    P-256/ECDSA-SHA256，不提供算法协商。Root TTL 为 10 年、Intermediate 为 90 天、
    manifest 为 30 天、leaf 为 24 小时，允许时钟偏差固定为 5 分钟。
23. Server 只持有在线 Intermediate 私钥，不持有 Root 私钥。Agent 预置 Root 和期望的
    TrustDomainName；deployment service TLS 使用独立信任链，Agent 不得把服务端下发的
    新 Root 当作信任锚。
24. Node leaf 使用唯一 URI SAN
    `urn:stellaris:node:v3:<trust-domain-id-hex>:<node-uid>`、唯一 overlay IPv4 SAN、
    空 Subject、当前节点 SPKI，以及 `ClientAuth` 和 `ServerAuth` EKU。
25. Critical node claims OID 固定为
    `2.25.183780856908411626329710048056624933369`。严格 DER claims 包含 schema version
    1、TrustDomainId、manifest/allocation/key/revocation epoch；未知 claims 版本直接
    拒绝。验证方必须把证书承载的 principal、IP、SPKI 和上述 claims 与当前
    AuthorizationTuple 对应字段比对；lifecycle 与 state epoch 只服从当前 Node record。
26. Issuer manifest 使用 Root ECDSA 签名的严格 DER envelope。Issuer SPKI 指纹按原始
    字节排序；同一 epoch 只接受完全相同的 manifest bytes，更高 epoch 必须先持久化再
    生效，较低 epoch、同 epoch 分叉、过期 manifest 和不在 active set 的 Intermediate
    全部拒绝。
27. 证书只证明签发时的当前 lease，不拥有地址。普通续期可短暂重叠，但旧、新证书必须
    对应同一 NodeUid、IP 和 SPKI；换钥事务提交后，旧 SPKI 的认证和逐消息授权立即失效，
    且已经授权过的 SPKI 永久 tombstone，不得在后续 replacement 中重新授权。
    Leaf claims 不包含 lifecycle 或 state epoch；Server 只从当前 Node record 填充这两个
    AuthorizationTuple 字段。节点 disable 后不能认证，重新 enable 后，Agent 仅在其余
    证书绑定字段完全一致时接受并持久化 Server 提供的单调增加 state epoch。

### 地址、持久化与恢复

28. 地址状态机固定为 Free、Reserved、Allocated 和 Quarantined。地址池排除 network、
    broadcast 与显式 reserved 地址，并按最低可用地址分配。固定 reservation 绑定
    Admission；Admission 过期或取消后释放 reservation，且不增加 allocation epoch。
29. Node 与 lease 的 owner、IP 和 allocation epoch 必须双向一致并在同一事务校验。
    断线、disable、revoke、证书过期和普通续期都不释放地址；只有 Revoked 节点允许
    显式 release。
30. Release 原子持久化 Released tombstone 与 Quarantined lease。地址的
    `not_reusable_before` 固定为 `max(last_issued_not_after, now) + 5m`；隔离结束并重新
    分配给新 owner 时 checked-increment allocation epoch。`/23` 必须可容纳 256 个持久
    lease，无法满足 `max_nodes` 的 CIDR 必须在启动前拒绝。
31. 首次 enrollment、续期和 re-enrollment 每次签发都必须在响应前原子执行
    `last_issued_not_after = max(last_issued_not_after, new_leaf.NotAfter)`，同时提交
    Admission 消费、状态/epoch 变化、operation 结果和精确响应 bytes。
32. Server 状态使用单一规范化 JSON snapshot、`fs2` 独占锁和外部 witness。Witness
    保存 TrustDomainId、generation、state SHA-256 和 previous state SHA-256，且必须
    位于 state 与 backup 目录之外。
33. 除第 12 条 admission token 文件 precommit 外，Normal Coordinator store mutation 的
    durable commit 顺序固定为：构造并验证 next state，临时写 snapshot，file fsync，原子
    rename，directory fsync，原子且持久地更新 witness，提交内存，响应，最后执行关闭
    会话等运行时副作用。任何前置步骤失败都不得响应成功或执行副作用；内存提交后旧
    AuthorizationTuple 已由逐消息栅栏失效，物理关闭延迟不能延长授权。离线 restore 的
    PKI/state/witness 写入使用详细设计冻结的独立恢复顺序。
34. State 落后 witness、digest 不一致或 witness 缺失时进入 fail-closed recovery mode，
    禁止签发、Control、Relay、地址分配和复用。只有 state 恰好领先 witness 一代且其
    previous hash 匹配 witness state hash 时，才允许把它判定为 witness 更新前崩溃并
    补写 witness；其他差异只能恢复与可信 witness 精确匹配的完整备份，或创建全新
    TrustDomain。
35. Server 按 operation ID 持久化请求 digest 与精确响应 bytes。相同 ID 和相同请求
    逐字节 replay，冲突请求拒绝；记录至少保留到响应证书 NotAfter 加 5 分钟。
36. Agent 在首次发送 enroll/re-enroll 请求前，原子持久化 staged private key、CSR、
    token binding、operation ID 和精确请求 bytes。不确定结果必须保留 staged 状态并
    原样重试；成功响应完成全部验证并原子替换 active identity 后才能清除 staged 状态。

## 破坏式边界与 ADR 关系

V3-R1 从空 trust domain、空 Server state、独立 witness 和新的 Agent enrollment 开始，
只接受本 ADR 冻结的协议、配置、状态、身份和证书 profile。不定义状态转换器、双
listener、协议降级、隐藏输入或原地回退；不符合唯一 V3-R1 契约的输入按其正常严格
解析和验证规则 fail-closed。

若本 ADR 被 Accepted，它将取代 [ADR 0003](0003-coordinator-p2p-relay-fallback.md)
的按需直连、Relay fallback、路径选择和 candidate 产品方向；ADR 0003 保留为历史决策
记录。V3-R1 只保留“单 Coordinator 内含可信 Relay”这一角色边界，并把 Relay 定义为
唯一数据路径。ADR 0004 仍为 Proposed 时，ADR 0003 的现有生命周期状态不变。

## 非目标

- 直连数据路径、candidate 交换、NAT 穿透和多路径；
- 多 Coordinator、HA、Raft/BFT 和在线管理 API；
- 多信任域 federation、跨域 ACL/capability 和全局名称；
- IPv6、Relay 端到端加密、元数据隐藏和硬件 attestation；
- 30 分钟稳定性 soak 和 256 个 Agent 同时在线的资源门禁；
- SQLite 或其他数据库后端。

## 备选方案

- **使用可读名称作为永久 principal：**实现简单，但改名会改变长期身份，并使审计引用
  与展示名称耦合。
- **使用节点 SPKI 作为永久 NodeUid：**省去 UUID 分配，但安全换钥会改变身份，或迫使
  系统继续信任已经泄露的旧密钥。
- **让一张证书永久拥有地址：**把短期凭据误作长期资源所有权，无法安全处理续期重叠、
  撤销和隔离复用。
- **由在线 Root 直接签发 leaf：**减少 PKI 工具，但扩大在线服务失陷的影响范围，且无法
  通过 Root 签名 manifest 独立移除 Intermediate。
- **在 V3-R1 同时实现直连与 Relay：**可能降低 Relay 带宽，但会同时引入对端发现、
  candidate 验证、路径仲裁和离线撤销传播，无法在本阶段形成较小且可验证的纵向闭环。
- **开放在线管理 API：**减少停服操作，但会增加管理员认证、授权、审计和并发 mutation
  的独立安全边界；本阶段选择停服的 server-local CLI。
- **使用嵌入式数据库：**可以复用事务与 WAL，但会改变备份、恢复和部署模型；本阶段选择
  规范化 snapshot 加外部 witness，并接受更严格的故障注入与运维要求。
- **由 `agent run` 自动准入或换钥：**启动更方便，但会隐藏持久身份 mutation 和不确定
  提交恢复；本阶段要求显式 enroll/re-enroll。

## 后果

正面结果：

- 改名、证书续期、换钥和地址管理不再改变节点长期身份；
- 单一 AuthorizationTuple 让 Control、Relay、证书签发和持久状态使用相同授权边界；
- 地址水位、隔离期和 allocation epoch 可防止陈旧凭据、延迟清理和地址 ABA 影响新 owner；
- 离线 Root 与签名 manifest 缩小在线 Intermediate 泄露后的长期影响；
- 原子 snapshot、外部 witness、幂等响应和 staged Agent 请求为崩溃恢复提供确定语义；
- Relay-only 使首阶段可以端到端验证 enrollment、续期、撤销、恢复和 IPv4 数据转发。

代价与风险：

- Relay 是带宽和可用性集中点，并能看到完整 overlay 包和流量元数据；
- Coordinator 或在线 Intermediate 失陷仍可冒充或授权节点，NodeUid 不消除中心信任；
- 单节点 leaf/key 同时用于 Control 与 Relay，私钥泄露会影响两类会话；
- 停服管理、外部 witness、离线 Root、manifest 和分离备份增加部署与恢复负担；
- witness 缺失、分叉或无法证明最新时会主动牺牲可用性并进入 recovery mode；
- 普通文件 witness 依赖独立保管；state、backup 与 witness 一起被一致回滚时无法检测；
- 永久名称、SPKI 与 Released tombstone 使部分持久状态只增不减；
- 单 snapshot、单 Coordinator 和 IPv4 地址池限制未来规模、HA 与多信任域扩展。

## 接受条件

在 ADR 决策状态改为 Accepted 前，必须满足：

- `TrustDomainName`、`TrustDomainId`、`NodeUid`、URI、OID/DER、NodeName、生命周期、
  epoch 和 AuthorizationTuple 的唯一编码与全部不变量已经冻结；
- 三类 ALPN 的 12 字节 header 布局、消息字段、方向、错误码、operation ID、严格 JSON
  规则和各类长度上限已经冻结；
- Server/Agent 配置字段、上述公开 CLI 的完整参数、默认值、文件权限和锁冲突错误已经
  冻结；
- Root/Intermediate/leaf profile、manifest DER envelope、签名输入、排序、期限、分发、
  overlap、issuer 移除和 Root 替换流程已经冻结；
- canonical JSON、snapshot/witness schema、原子 witness 更新、backup 内容、路径隔离、
  recovery mode 进入/退出和可信恢复步骤已经冻结；
- Admission、Node 和 lease 的完整状态转换、operation 保留/冲突、证书期限水位、固定
  reservation、池耗尽、时钟回退、release 和地址复用语义已经冻结；
- 威胁模型明确 Coordinator、在线 issuer 与 Relay 均在信任边界内，并明确单实例、
  Relay 可见性、无 HA 和无端到端 Relay 加密的后果；
- 详细 Proposed 设计保持 Draft，并冻结 `V3-R0-SPEC-01` 及全部 `V3-R1-*` 原子 Gate；
  ADR Accepted 后再单独评审设计并记录 Approved commit，不以实现或验证 Gate 通过作为
  接受本 ADR 的前置条件；
- 接受本 ADR 的决策变更同时记录其与 ADR 0003 的取代关系；在此之前，两份 ADR 的状态
  不得提前描述为 Accepted 或 Superseded。
