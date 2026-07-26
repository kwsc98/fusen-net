# Stellaris 最新方案总览

> **方案快照：2026-07-26。文档适用性：Current；适用范围：导航摘要；设计评审状态：
> N/A；ADR 决策状态：N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**

<!-- stellaris-release-status:design-status:start -->
当前运行边界是 `0.3.0-alpha.1` v2：交付状态 Implemented，验证状态 Unverified。
NodeUID、动态地址 lease、离线 Root/在线 Intermediate 和线协议 v3 均为 Proposed、
Not started、Unverified。
<!-- stellaris-release-status:design-status:end -->

本文只提供状态摘要和阅读入口，不复制线协议、配置字段、证书编码或完整实施门禁。
文档状态和冲突处理规则见 [`README.md`](README.md)；后续改造必须遵循
[`documentation-workflow.md`](documentation-workflow.md)。

## 核心结论

Stellaris 近期采用“中心化信任与协调、尽量分布式数据面”的混合架构：

- 一个逻辑 Server 负责 enrollment、授权、节点目录、连接计划和可信 Relay；
- 普通节点运行 Agent，不是每个节点都运行 Stellaris Server；
- Agent 先建立 Relay 路径，再按目标按需尝试节点间 QUIC P2P；
- QUIC 已包含 TLS 1.3，不再在同一链路外套 TLS，但仍需证书、proof-of-possession、
  授权状态和 overlay IP 所有权校验；
- 稳态下一个 Agent 使用一把节点私钥和一张当前 leaf，Control、Relay、P2P 共用；
- P2P 使用节点间 mTLS；Relay 回退在 Server 解密，Relay 能看到完整回退包和元数据；
- 推荐下一次改造是 v3 身份、PKI 与地址基础，不是先做 NAT 穿透；
- v3 直接删除 v2，不提供迁移器、双栈 listener、协议降级或兼容 feature。

首轮仍是自托管、单组织、单信任域和单协调实例，不是完全去中心化或多信任域网络。
现代系统与规范的比较及采用理由见
[`modern-distributed-network-survey.md`](modern-distributed-network-survey.md)，该调研不是
当前协议规范。

## 状态矩阵

| 维度 | 当前 v2 | Proposed v3 | 后续方向 |
| --- | --- | --- | --- |
| 长期身份 | 大小写敏感 `node_id` | `TrustDomainId + NodeUID` | 跨域 principal/capability |
| 可读名称 | 与身份共用 `node_id` | 独立 NodeName + name epoch | 跨域命名策略 |
| Overlay 地址 | `nodes.toml` 静态 IPv4 | NodeUID 的持久动态 lease | 每域 IPv6 prefix |
| 节点 PKI | Server 在线持有自签 node CA | 离线 Root + 在线 Intermediate + issuer manifest | 跨域 trust bundle/硬件根 |
| 节点凭据 | 一把 P-256 key、一张当前 leaf | 保持一 key/leaf，增加 UID/IP/epoch claims | 按威胁模型评估硬件密钥 |
| 数据路径 | 可信 Relay + host-candidate P2P | 保持 Relay-first 和单路径语义 | NAT 穿透、多路径、Relay E2E |
| 控制面 | 单 Coordinator | 单 Coordinator | HA/Raft；恶意控制器威胁才考虑 BFT |
| 租户与策略 | 单租户全互通 | 不变 | 多租户、ACL/capability |
| 运行传输 | Quinn | Quinn | 其他 QUIC runtime 后置 |

本表只比较方案边界；生命周期状态以本页顶部投影、[`adr/README.md`](adr/README.md) 和
两份权威计划为准。

## 当前 v2

```text
                     one logical Stellaris Server
          enrollment + control + directory + trusted Relay
                                |
                    control / Relay fallback
                       /                    \
              Agent A                        Agent B
           TUN + Hybrid QUIC ============= TUN + Hybrid QUIC
                    listen + dial       P2P after mTLS + Ready
```

Server 不创建 TUN。Agent 的 Hybrid Quinn endpoint 同时监听和拨号，所以它在某条 P2P
连接上可以是 QUIC client 或 server，但不能签发全网证书、分配地址或充当协调服务。

当前公共接口是 `stellaris server init|run`、`stellaris agent run`、
`stellaris config check` 和 `stellaris token generate`。真实边界如下：

- 单租户、单信任域、单协调实例、IPv4-only、最多 256 条静态节点记录；
- enrollment/control/Relay/P2P 使用四个 `/2` ALPN，运行时固定 Quinn；
- enrollment 使用 deployment service TLS、一次性 token 和 CSR PoP；
- 节点 CA 是 `server init` 创建、由 Server 在线持有私钥的自签名 CA；
- `node_id` 同时是名称和密码学身份，Server 启动时校验 node ID、静态 IPv4 和 token
  digest 唯一；Agent 不在配置中声明 overlay IP；
- 地址断线或证书过期后不释放，已有持久 binding 也不能通过修改节点表原地改绑；
- Relay 必须绑定当前 control lease，Ready 后才进入路由表；
- P2P 完成 mTLS、plan、descriptor 和 Ready 后才切换；每个包只选一次路径，失败的当前
  P2P 包不补发 Relay，后续包再回退；
- 在线 session 不进入快照，Server 重启后节点必须重新认证。

host candidate 排除 TUN/overlay 地址以及真实 underlay 路由证明属于发布门禁；P2P
Ready 或路径计数本身不能替代这些证据。精确线格式、配置和验证状态分别以
[`protocol.md`](protocol.md)、
[`configuration.md`](configuration.md) 和 [`distributed-network-plan.md`](distributed-network-plan.md)
为准。

## Proposed v3

v3 把当前耦合的身份、名称、地址、密钥、证书和会话拆成独立生命周期：

```text
TrustDomainId + NodeUID   durable principal
NodeName + name_epoch     human-readable directory label
AddressLease + allocation_epoch
authorized SPKI + key_epoch
lifecycle + state_epoch + revocation_epoch
short-lived certificate  signed proof of current claims
session/incarnation       online connection state
```

关键设计摘要：

- NodeUID 由首次成功 enrollment 的 Server 使用系统 CSPRNG 生成 UUIDv7，并由持久 store
  的唯一约束和幂等事务最终裁决；
- NodeName 可改名但不是安全身份；改名只增加 name epoch，不改变证书或数据路径；
- 节点生命周期为 Active、Disabled、Revoked、Released；首次 admission 在 NodeUID
  生成前独立存在，replacement admission 绑定已有 NodeUID；
- 地址使用 Free、Reserved、Allocated、Quarantined 持久状态机。lease 属于 NodeUID，
  不是某张短期证书；释放后必须按最后签发期限水位隔离再复用；
- 节点 PKI 改为离线 Root、在线 Intermediate 和 Root 签名 issuer manifest。Agent 只
  保存 Root 公钥/证书与 manifest，在线 Server 不持有 Root 私钥；
- 一把 operational key 和一张当前 leaf 继续供 Control、Relay、P2P 使用；验证还必须
  检查链、issuer manifest、principal、IP、SPKI、epoch 和当前授权状态；
- 地址、名称或证书都不能代替长期 principal；公开证书本身也不能证明仍获授权；
- 本地管理入口负责 create、rename、disable/enable、revoke、rotate-token 和 release；
  Agent 显式 re-enroll 处理安全换钥和不确定提交恢复；
- 备份必须绑定一致 state generation。无法证明快照最新时进入 recovery mode，禁止
  签发、路由和地址复用。

精确字段、证书 profile、隔离公式、事务顺序、管理语义和 Gate ID 只由
[`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 维护。
决策理由和接受条件见
[`ADR 0004`](adr/0004-durable-node-identity-and-address-leases.md)。

### 破坏式边界

若 ADR 0004 被接受，候选版本为 `0.4.0-alpha.1`：

- ALPN、protocol、config、Server store、Agent identity 和 certificate profile 同时
  升为 v3；
- 删除 v2 listener、codec、静态地址 runtime 和 adapter；
- v2 配置、状态、证书和 Agent 全部拒绝；
- 切换部署重新初始化 v3 PKI/state，并重新 enrollment 所有 Agent；
- 中间实现不得发布为 v2/v3 混合可用版本。

ADR 0003 的 Relay-first、按需 P2P、单路径和失败包不补发继续有效。

## 实施顺序

```text
current v2 alpha
       |
       v
reusable v2 baseline gates
       |
       v
V3-P0: freeze candidate semantics -> ADR 0004 decision
       | accepted                     | rejected
       v                              v
destructive v3 V3-P1..V3-P6     finish all v2 release gates
       |
       v
conditional NAT and scale work
```

v3 会删除 v2，所以不先在 v2 重复执行全部长期发布门禁。ADR 决策前先完成可复用的
workspace、最小网络闭环、单路径/session ABA、256 条状态和持久化/TUN harness；这些
`V2-B*` 原子门禁见
[`distributed-network-plan.md`](distributed-network-plan.md#v3-决策前的可复用基线)。
随后由唯一的 `V3-P0` 冻结候选 v3 语义并作出 ADR 决策；全部 `V3-P*` 阶段和原子 Gate
ID 只由
[`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 定义。

三类“256”证据不能混用：

- v2 基线是在至少 `/23` 中验证 256 条目录/状态记录，不启动 256 个在线 Agent；
- `V3-P6` 是至少 `/23` 中的 256 个持久 lease 状态，以及小规模 30 分钟连接稳定性；
- Conditional `0.6` 才是 256 个在线 Agent 的完整资源 soak。

## 后续方向

只有 v3 被接受、实现并通过门禁后，NAT 与规模阶段的条件编号才成立：

- Conditional `0.5`：server-reflexive candidate、同时打洞、NAT 实验矩阵和 Relay 回退；
- Conditional `0.6`：256 在线 Agent、资源 soak、指标/健康检查、持续 fuzz 和故障注入；
- `1.0`：Linux 稳定发布及明确跨平台支持矩阵。

多协调服务 HA、多信任域 federation、ACL、多租户、IPv6、Relay 路径端到端加密、
TPM/attestation、BFT/阈值签名、外部 STUN/TURN、DNS、默认/子网路由和其他 QUIC runtime
均不进入首轮 v3，每项都需要独立 ADR 和威胁模型。

## 权威文档

| 范围 | 权威来源 |
| --- | --- |
| 文档状态、阅读顺序和冲突处理 | [`README.md`](README.md) |
| 后续改造流程 | [`documentation-workflow.md`](documentation-workflow.md) |
| 当前项目入口 | [`../README.md`](../README.md) |
| 当前 v2 架构 | [`architecture.md`](architecture.md) |
| 当前唯一线协议 | [`protocol.md`](protocol.md) |
| 当前 v2 配置 | [`configuration.md`](configuration.md) |
| 当前信任与威胁边界 | [`security-model.md`](security-model.md) |
| 当前平台与支持等级 | [`compatibility.md`](compatibility.md) |
| 当前 v2 未完成门禁 | [`distributed-network-plan.md`](distributed-network-plan.md) |
| 现代方案调研（非规范） | [`modern-distributed-network-survey.md`](modern-distributed-network-survey.md) |
| 已接受的数据路径决策 | [`ADR 0003`](adr/0003-coordinator-p2p-relay-fallback.md) |
| Proposed v3 决策摘要 | [`ADR 0004`](adr/0004-durable-node-identity-and-address-leases.md) |
| Proposed v3 完整模型与 Gate ID | [`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) |
| 条件版本顺序 | [`roadmap.md`](roadmap.md) |
| 门禁执行记录 | [`verification/README.md`](verification/README.md) |
