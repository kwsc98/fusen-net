# Stellaris 最新方案总览

> **方案快照：2026-08-03。文档适用性：Current；适用范围：导航摘要；设计评审状态：
> N/A；ADR 决策状态：N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**

<!-- stellaris-release-status:design-status:start -->
当前运行边界是 `0.3.0-alpha.1` v2：交付状态 Implemented，验证状态 Unverified。
目标版本 `0.4.0-alpha.1` 的 `V3-R1 Identity & Relay Core`，包括 NodeUid、动态地址
lease、分层 PKI、线协议 v3 和 Relay-only 数据路径，均为 Proposed、Not started、
Unverified。
<!-- stellaris-release-status:design-status:end -->

本文只提供状态摘要和阅读入口，不复制线协议、配置字段、证书编码或完整实施门禁。
文档状态和冲突处理规则见 [`README.md`](README.md)；后续改造必须遵循
[`documentation-workflow.md`](documentation-workflow.md)。

## 核心结论

当前 `0.3.0-alpha.1` v2 采用中心化协调、可信 Relay 与按需 P2P 的混合架构：

- 一个逻辑 Server 负责 enrollment、授权、节点目录、连接计划和可信 Relay；
- 普通节点运行 Agent，不是每个节点都运行 Stellaris Server；
- Agent 先建立 Relay 路径，再按目标按需尝试节点间 QUIC P2P；
- QUIC 已包含 TLS 1.3，不再在同一链路外套 TLS，但仍需证书、proof-of-possession、
  授权状态和 overlay IP 所有权校验；
- 稳态下一个 Agent 使用一把节点私钥和一张当前 leaf，Control、Relay、P2P 共用；
- P2P 使用节点间 mTLS；Relay 回退在 Server 解密，Relay 能看到完整回退包和元数据；
- 下一次 Proposed 改造是 `V3-R1 Identity & Relay Core`：统一身份、PKI、动态地址、
  持久恢复和可信 Relay 闭环，数据路径仅保留 Relay；
- v3 直接替换 v2，不提供迁移器、双栈 listener、协议降级或兼容 feature，也不保留
  P2P listener、ALPN、消息、配置或运行时占位。

首轮仍是自托管、单组织、单信任域和单协调实例，不是完全去中心化或多信任域网络。
现代系统与规范的比较及采用理由见
[`modern-distributed-network-survey.md`](modern-distributed-network-survey.md)，该调研不是
当前协议规范。

## Proposed 产品方向与 AI 治理

[`产品方向与 AI 迭代治理计划`](product-direction-and-iteration-governance-plan.md)
是另一份 Proposed / Draft 候选。它提议先冻结 Stellaris 作为弹性算力调度平台网络底座
的产品边界，再通过人工激活的单一 Work Item、单 Gate、精确路径和风险审查约束 AI
迭代。关联的 [ADR 0005](adr/0005-elastic-compute-network-product-boundary.md) 与
[ADR 0006](adr/0006-human-controlled-ai-iteration.md) 均为 Proposed。

该 Draft 与既有 v3 候选在单租户、单 Coordinator、256 节点和 Relay-only 等方向上存在
冲突，因此提议在治理完成后另开技术 Design-only 决策。当前并未接受这两份新 ADR，
也未批准新设计；ADR 0004 仍为 Proposed，既有 v3 仍为 Draft / Not started。新增导航
不改变当前 v2 能力、既有路线顺序或任何支持状态。

## 状态矩阵

| 维度 | 当前 v2 | Proposed v3 | 后续方向 |
| --- | --- | --- | --- |
| 长期身份 | 大小写敏感 `node_id` | `TrustDomainId + NodeUid` | 跨域 principal/capability |
| 可读名称 | 与身份共用 `node_id` | 独立 NodeName + name epoch | 跨域命名策略 |
| Overlay 地址 | `nodes.toml` 静态 IPv4 | NodeUid 的持久动态 lease | 每域 IPv6 prefix |
| 节点 PKI | Server 在线持有自签 node CA | 离线 Root + 在线 Intermediate + issuer manifest | 跨域 trust bundle/硬件根 |
| 节点凭据 | 一把 P-256 key、一张当前 leaf | 一把 operational key、一张当前 leaf，增加 UID/IP/epoch claims | 按威胁模型评估硬件密钥 |
| 数据路径 | 可信 Relay + host-candidate P2P | 仅可信 Relay；绑定 Ready control session 后安装单一路由 | 其他数据路径另行设计和决策 |
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
TrustDomainId + NodeUid   durable principal
NodeName + name_epoch     human-readable directory label
AddressLease + allocation_epoch
authorized SPKI + key_epoch
lifecycle + state_epoch + revocation_epoch
short-lived certificate             signed proof of current claims
ControlSessionId/RelaySessionId     online connection ownership
```

关键设计摘要：

- NodeUid 由首次成功 enrollment 的 Server 使用系统 CSPRNG 生成 UUIDv7，并由持久 store
  的唯一约束和幂等事务最终裁决；
- NodeName 可改名但不是安全身份；改名只增加 name epoch，不改变证书或数据路径；
- 节点生命周期为 Active、Disabled、Revoked、Released；首次 admission 在 NodeUid
  生成前独立存在，replacement admission 绑定已有 NodeUid；
- 地址使用 Free、Reserved、Allocated、Quarantined 持久状态机。lease 属于 NodeUid，
  不是某张短期证书；释放后必须按最后签发期限水位隔离再复用；
- 节点 PKI 改为离线 Root、在线 Intermediate 和 Root 签名 issuer manifest。Agent 只
  保存 Root 公钥/证书与 manifest，在线 Server 不持有 Root 私钥；
- 一把 operational key 和一张当前 leaf 供 Control、Relay 使用；验证还必须
  检查链、issuer manifest、principal、IP、SPKI、epoch 和当前授权状态；
- 地址、名称或证书都不能代替长期 principal；公开证书本身也不能证明仍获授权；
- 本地管理入口负责 admission create/replace/cancel 及 node rename、disable/enable、
  revoke、release；Agent 显式 enroll/re-enroll 处理首次注册、安全换钥和不确定提交恢复；
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
- 只开放 enrollment、control 和 Relay 三条 v3 ALPN，所有 overlay 数据只经可信 Relay；
- 中间实现不得发布为 v2/v3 混合可用版本。

ADR 0003 继续记录当前 v2 数据路径；Proposed v3 不沿用其中的 P2P 公共 surface。只有
ADR 0004 Accepted、详细设计 Approved 并完成实现后，Relay-only 才能成为 Current。

## 实施顺序

```text
current v2 alpha
       |
       v
V3-R0: freeze the Proposed contract and ADR decision
       |
       v
ADR 0004 Accepted + design Approved + approval commit
       |
       v
V3-R1: destructive single-model implementation
       |
       v
Evidence-only records for the merged target commit
```

当前 v2 的开放门禁继续记录当前验证事实，但不是 `V3-R0` 或 `V3-R1` 的进入条件。
`V3-R0` 只负责冻结设计、接受 ADR 和记录批准 commit；`V3-R1` 在一个纵向实现阶段中
完成唯一 v3 模型，并在合入后对目标 commit 单独归档证据。阶段和原子 Gate ID 只由
[`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 定义。

本阶段的容量和网络证据不能混用：

- `V3-R1` 容量 Gate 在至少 `/23` 中验证 256 个持久 lease，不启动 256 个在线 Agent；
- `V3-R1` Linux Gate 只验证两个 Agent 的真实 TUN Relay 闭环；
- 30 分钟 soak 和 256 个在线 Agent 的资源验证均属于未编号后续工作。

## 后续方向

`V3-R1` 之后的工作尚未编号。candidate/NAT、其他数据路径、256 在线 Agent、长期 soak、
指标/健康检查和稳定发布都必须重新设计、单独决策并定义自己的退出门禁，不能作为
`0.4.0-alpha.1` 的隐含能力或前置条件。

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
| Proposed `V3-R1 Identity & Relay Core` 决策摘要 | [`ADR 0004`](adr/0004-durable-node-identity-and-address-leases.md) |
| Proposed `V3-R0`/`V3-R1` 完整模型与 Gate ID | [`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) |
| Proposed 弹性算力网络产品边界决策 | [`ADR 0005`](adr/0005-elastic-compute-network-product-boundary.md) |
| Proposed 人工控制 AI 迭代治理决策 | [`ADR 0006`](adr/0006-human-controlled-ai-iteration.md) |
| Proposed 产品方向、单一 Active Work Item 与治理 Gate | [`product-direction-and-iteration-governance-plan.md`](product-direction-and-iteration-governance-plan.md) |
| 条件版本顺序 | [`roadmap.md`](roadmap.md) |
| 门禁执行记录 | [`verification/README.md`](verification/README.md) |
