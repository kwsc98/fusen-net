# ADR 0004：持久节点身份、地址租约与分层节点 CA

- 文档适用性：Proposed
- 适用范围：候选 v3 身份、PKI 与地址
- 设计评审状态：N/A
- ADR 决策状态：Proposed
- 交付状态：Not started
- 验证状态：Unverified
- 发布状态：Unreleased
- 日期：2026-07-26

## 背景

当前 v2 将可读 `node_id` 同时作为永久身份和配置名称，在静态节点表中预先绑定唯一
overlay IPv4，并由 Server 直接持有节点 CA 私钥。这个模型适合首个单信任域运行时，
但会把改名、动态分配、密钥轮换、地址复用和长期审计耦合在一起。

QUIC 已提供 TLS 1.3 加密和握手 proof-of-possession，但不会自动决定节点名称是否
唯一、哪个节点拥有某个 IP、当前 SPKI 是否获准或旧地址租约是否已经撤销。这些仍然
需要协调服务的持久授权状态。

## 提议决策

本 ADR 提议在当前 v2 的可复用最小基线通过、且本 ADR 的 `V3-P0` 决策冻结后，进行一次
不兼容的 v3 身份与地址改造。若决定发布稳定 v2 而拒绝本 ADR，才继续关闭全部 v2
发布门禁：

1. v3 初始化冻结规范化 `TrustDomainName`，并以带固定 domain-separation tag 的规范
   DER Node Root CA SPKI SHA-256 作为 `TrustDomainId`；全局稳定 principal 是
   `TrustDomainId + NodeUID`。Root key 不得跨域复用；替换 Root key 会创建新信任域并
   要求全量重新初始化，而不是域内轮换。
2. 中心 Server 首次成功 enrollment 时生成永久 UUIDv7 `NodeUID`。
3. 可读 `NodeName` 与 NodeUID 分离，只接受规范化小写 ASCII，并在信任域内唯一。
   UUIDv7 使用系统 CSPRNG 并由 store 唯一约束裁决；改名保留旧名称 tombstone。
4. NodeRecord 显式使用 Active/Disabled/Revoked/Released 生命周期和独立
   state/name/key/revocation epoch；admission 另存，地址与授权 SPKI 按状态可空。
   生命周期、名称、密钥和撤销分别只增加自己的 epoch；普通续期不改任何 epoch。
5. `CoordinatorStore` 使用 Free/Reserved/Allocated/Quarantined 地址状态机为 NodeUID
   建立稳定 overlay IPv4 lease，并在同一事务验证 Node/lease 双向绑定。
6. 首次注册、续期、re-enrollment 和换钥每次签发都必须在响应前原子提升该地址的
   `last_issued_not_after`，同时提交幂等结果、token 消费和相关 epoch。
7. 节点 leaf certificate 使用项目自有、唯一规范的 NodeUID URI SAN、overlay IP SAN、
   当前 SPKI、严格的 TrustDomainId/manifest/allocation/key/revocation epoch claims、
   `ClientAuth`/`ServerAuth` 和短期限；Control、Relay、P2P 继续复用同一 leaf 身份。
8. 证书只证明当前租约，不拥有地址。普通续期证书可短暂重叠，但必须对应同一
   UID/IP/SPKI；换钥提交后旧认证会话不再获得逐消息授权。
9. 节点 CA 改为离线 Root 与在线 Intermediate 两层；Server 不持有 Root 私钥。Agent
   还必须验证 Root 签名、带有效期和单调 epoch 的活跃 issuer manifest，拒绝其中未列出
   的 Intermediate 以及 manifest 回滚。leaf 的签发时 manifest epoch 可小于等于当前
   epoch，但其实际 Intermediate 必须仍在当前 active set。
10. 部署 service TLS 保持独立信任链，用于首次确认 enrollment Server。
11. 地址不因断线、disable、revoke 或证书过期释放。只有显式 `release` 回收 lease，
    且必须先 revoke；Released/tombstone 必须先持久化再产生关闭连接等副作用。地址至少
    隔离到最后 leaf NotAfter 加时钟偏差，issuer/恢复水位不可信时从当前时间隔离完整
    最大 leaf TTL，不能只靠 epoch 立即复用。
12. 地址池按可用 host 地址校验容量；`/24` 通常只能分配 254 个普通 host，256 节点
    验收至少使用 `/23`。
13. 首轮管理面使用停服的 server-local CLI；换钥还必须提供持有 Agent identity lock 的
    显式 `agent re-enroll`，在发送前持久化 staged key/CSR/请求并对不确定结果原样幂等
    重试。Active 换钥保持 Active；Revoked 恢复显式转回 Active。离线管理变更不承诺
    即时关闭失联 P2P，只能通过控制面传播和证书到期收敛。
14. 所有持久变更必须完成文件及目录 fsync 后才应答和执行运行时副作用。备份使用统一
    generation；无法证明快照最新时进入 fail-closed recovery mode，不接受静默回滚。
15. 首轮继续是单协调实例、单租户和单信任域；多域 federation 另立 ADR。

详细模型、阶段和门禁见
[`../node-identity-trust-addressing-plan.md`](../node-identity-trust-addressing-plan.md)。

## 破坏式边界

若本 ADR 被接受，线协议、ALPN、配置、Server 状态、Agent identity 和证书 profile
同时升级为 v3。删除 v2 实现，不提供迁移器、双栈 listener、降级、兼容 feature 或
旧凭据重新签发。所有节点使用全新 v3 状态和 enrollment 凭据重新加入；节点认证链路
首轮禁用 0-RTT 和 TLS session resumption，避免恢复旧授权 epoch。

ADR 0003 的 Relay-first、按需 P2P、单路径选择和失败包不补发决策继续有效。本 ADR
只替代其“静态 node ID/IPv4、无动态地址、Server 直接持有单层节点 CA、续期永久固定
同一公钥”的身份和信任条款。

## 备选方案

- **保留 v2 静态 node ID/IP：**实现最简单，但改名、换钥、地址复用和长期审计继续
  耦合，不能满足本次改造目标。
- **用节点公钥作为永久 NodeUID：**省去 UUID 分配，但每次安全换钥都会改变长期身份，
  或被迫永久信任已泄露的旧密钥。
- **让一张证书永久拥有一个地址：**把短期凭据误当资源所有权，无法安全处理续期、
  重叠证书和证书到期后的地址隔离。
- **直接引入 DID、区块链或 BFT：**不能替代域内准入、地址唯一性和撤销状态，对当前
  单组织、最多 256 节点的威胁模型只会增加共识和运维复杂度。
- **保留 v2/v3 兼容桥：**会产生双重身份和状态语义，延长不安全旧输入的生命周期，
  与本项目明确的一次破坏式切换决策冲突。

## 后果

正面结果：

- 改名、证书续期、密钥轮换和地址管理不再改变节点的长期身份；
- 地址分配和复用具有可持久化 epoch，可拒绝新的旧凭据认证、旧 descriptor 和 ABA
  清理；失联节点上已建立的 P2P 仍受旧证书期限隔离约束；
- Root 签名 issuer manifest 可移除泄露的 Intermediate，但离线节点只能在取得新
  manifest 或旧 manifest/leaf 过期后拒绝它；
- 后续 federation、IPv6 前缀和硬件根密钥有稳定身份基础。

代价与风险：

- 需要重写协议、证书验证、持久状态、配置、CLI 和全部真实网络门禁；
- Server 状态与 Agent identity 无法沿用，部署必须整体重建；
- 地址回收、Intermediate 轮换和管理锁增加新的 crash-consistency 状态机；
- 单节点 leaf/key 复用意味着一次私钥泄露同时影响 Control、Relay 和 P2P；
- 没有外部 generation witness 或硬件单调计数时，无法自动识别完整旧快照回滚；
- NodeUID 不消除中心签发服务的信任，单信任域仍可被协调服务或在线 issuer 冒充。

## 接受条件

在状态改为 Accepted 前，必须冻结以下决策：

- NodeUID 编码、NodeName 规范和 lifecycle/epoch 不变量；
- TrustDomainName/Id、v3 certificate profile、canonical URI 和 issuer manifest；
- 地址状态机、CIDR 容量、reservation、release/tombstone、期限水位、隔离期和时钟规则；
- server-local 管理 CLI、Agent re-enroll 和双方独占状态锁语义；
- Root/Intermediate 初始化、备份、issuer manifest 和轮换流程，包括 Root key 替换
  等同新信任域；
- state generation、快照回滚检测边界和 fail-closed recovery mode；
- v3 protocol/config/state 版本号及无兼容切换步骤。
