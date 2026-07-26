# 路线图

> **文档适用性：Current；适用范围：阶段顺序；设计评审状态：N/A；ADR 决策状态：
> N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。** 本页只维护阶段和决策顺序；
> 详细计划定义 Gate 与阶段状态，[`verification/`](verification/README.md) 保存目标 commit
> 的执行证据，[`compatibility.md`](compatibility.md) 裁决支持等级。

路线图只描述阶段目标和决策顺序，不承诺日期；精确退出门禁及状态由详细计划维护。
代码存在不等于阶段完成；只有实现、CI、真实网络证据和文档全部满足门禁后，才能将
能力标记为已验证。

当前、Proposed 和后续方向的完整分层见 [`design-overview.md`](design-overview.md)。

## `0.3.0-alpha.1`：破坏式 v2 切换

<!-- stellaris-release-status:roadmap-status:start -->
当前 v2 的交付状态是 Implemented，验证状态是 Unverified，发布状态是 Unreleased，
只能称为“运行时实现已接入、发布门禁未完成”，不能称为稳定可用。
<!-- stellaris-release-status:roadmap-status:end -->

实际运行边界由 [`design-overview.md`](design-overview.md#当前-v2) 和各 Current 规范
描述；实现清单、开放 Gate ID 及精确判定条件只由
[`distributed-network-plan.md`](distributed-network-plan.md) 维护。本页不复制第二份
勾选状态。当前 v2 的开放门禁只描述其验证事实，不是 Proposed v3 的实现前置条件。

## Proposed `0.4.0-alpha.1`：`V3-R1 Identity & Relay Core`

下一次破坏式改造由
[`ADR 0004`](adr/0004-durable-node-identity-and-address-leases.md) 与
[`单一身份、信任、动态地址与 Relay 闭环改造计划`](node-identity-trust-addressing-plan.md)
定义，分为两个阶段：

- `V3-R0`：Design-only 冻结公共接口、安全/恢复语义和原子 Gate，接受 ADR 0004，并在
  后续 Design-only 中把详细设计标为 Approved、记录批准 commit；
- `V3-R1`：在一个纵向 Implementation 阶段中切换唯一的 v3 类型、配置、协议、PKI、
  store、管理 CLI 和 Relay runtime；合入时只能是 Implemented、Unverified；
- Evidence-only：对已合入的目标 commit 执行并归档全部冻结 Gate，再单独推进验证状态。

候选能力包括：

- `TrustDomainId + NodeUid` 长期 principal 与独立规范化 NodeName；
- 持久动态 IPv4 lease、allocation/key/revocation epoch 和安全隔离回收；
- 离线 Root、在线 Intermediate、Root 签名 issuer manifest 与短期 leaf；
- 显式 enroll/re-enroll、完整节点生命周期、原子快照、外部 witness 和备份恢复；
- 只开放 enrollment、control、Relay 三条 v3 ALPN，overlay 数据只经可信 Relay；
- 不保留 P2P listener、ALPN、消息、配置或运行时占位；
- 一次性建立唯一 v3 schema/state/wire 模型，不提供迁移、双栈 listener 或降级。

本阶段状态仍是 Proposed、Not started、Unverified。在 ADR Accepted、设计 Approved、
实现和目标 commit 门禁完成前，v2 仍是唯一 Current，不能把本节当作发布支持。

## 后续方向（未编号）

candidate/NAT、其他数据路径、30 分钟 soak、256 在线 Agent、HA、多信任域、ACL、IPv6、
远程管理、Relay 端到端加密、指标/健康检查、跨平台运行支持和稳定发布全部后置，且尚未
分配版本号。每项必须另行完成文档分级、ADR/设计和原子 Gate，不能阻塞或扩张
`V3-R1` 的退出范围。
