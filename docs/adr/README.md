# 架构决策记录

> **文档适用性：Current；适用范围：ADR 索引；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：N/A；验证状态：N/A；发布状态：N/A。**

<!-- stellaris-release-status:adr-status:start -->
当前 ADR 索引分别记录决策、交付和验证状态：Accepted 不等于 Implemented，Implemented
也不等于 Verified；合并一份 Proposed ADR 只表示保留候选记录，不等于 Accepted。

| ADR | 验证状态 | 发布状态 |
| --- | --- | --- |
| [0001](0001-central-relay-for-0.1.md) | N/A | N/A |
| [0002](0002-transport-backend-boundary.md) | Unverified | Unreleased |
| [0003](0003-coordinator-p2p-relay-fallback.md) | Unverified | Unreleased |
| [0004](0004-durable-node-identity-and-address-leases.md) | Unverified | Unreleased |
<!-- stellaris-release-status:adr-status:end -->

ADR 一经接受不直接重写历史结论；被新决策替代时标为 Superseded。状态规则见
[`../documentation-workflow.md`](../documentation-workflow.md)。

| ADR | 适用性 | 决策状态 | 交付状态 | 决策与替代关系 |
| --- | --- | --- | --- | --- |
| [0001](0001-central-relay-for-0.1.md) | Historical | Superseded | N/A | 中心 Relay 历史基线；由 ADR 0003 替代 |
| [0002](0002-transport-backend-boundary.md) | Current | Accepted | Implemented | 保留传输抽象；runtime selection 条款由 ADR 0003 替代 |
| [0003](0003-coordinator-p2p-relay-fallback.md) | Current | Accepted | Implemented | 协调服务、可信 Relay、按需 LAN P2P 和单路径回退 |
| [0004](0004-durable-node-identity-and-address-leases.md) | Proposed | Proposed | Not started | 不可变 NodeUID、持久地址租约、分层节点 CA 和破坏式 v3 候选 |

何时必须新增 ADR 只由
[`documentation-workflow.md`](../documentation-workflow.md#变更分级) 定义；本索引不
复制另一份可能失步的触发清单。
