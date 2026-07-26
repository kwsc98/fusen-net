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
勾选状态。若 ADR 0004 被接受，v2 不发布为稳定版本，只完成下一节的可复用基线；若
ADR 被拒绝，才继续关闭 v2 完整发布门禁。

## 下一决策点：v2 可复用基线

在决定 ADR 0004 前，只关闭能降低 v3 风险并可直接复用的最小基线。所有原子 Gate ID、
判定条件和状态只由
[`distributed-network-plan.md`](distributed-network-plan.md#v3-决策前的可复用基线)
定义；本页不复制第二份清单。

全部 `V2-B*` 原子 Gate 完成后进入唯一的 `V3-P0`：冻结 ADR 0004 的候选编码、状态机、
PKI、地址隔离、管理和恢复决策，再评审 ADR。不得为同一冻结动作建立第二个 Gate ID。

此处不要求先完成 v2 的完整恶意输入矩阵、30 分钟 soak 或 256 在线节点资源门禁。
macOS 和 Windows 保持编译通过、运行未验证。

决策分支：

- ADR 0004 接受：不发布稳定 v2，直接进入一次破坏式 v3，完整门禁只对 v3 执行；
- ADR 0004 拒绝：继续关闭 v2 计划中的全部发布门禁，再重新排定 NAT 和规模版本号。

## Proposed `0.4`：v3 身份、信任与地址基础

在 ADR 0004 接受分支中，扩展 NAT 和规模前的下一次破坏式改造是
[`ADR 0004`](adr/0004-durable-node-identity-and-address-leases.md) 与
[`节点身份、信任域与地址分配演进计划`](node-identity-trust-addressing-plan.md)：

- 不可变 NodeUID 与规范化 NodeName 分离；
- 持久动态 IPv4 lease、allocation/key/revocation epoch 和安全隔离回收；
- 离线 Node Root CA 与在线 Intermediate CA；
- v3 协议、配置、状态、证书 profile 和本地管理入口一次性切换；
- 删除 v2，不提供迁移、双栈 listener 或降级。

本阶段状态仍是 Proposed。在 ADR 接受、实现和门禁完成前，v2 静态 node ID/IP 仍是
唯一当前能力，不能把本节当作发布支持。

## Conditional `0.5`：NAT 穿透

`0.5` 编号以 ADR 0004 被接受且 Proposed `0.4` 完成为前提；若 ADR 被拒绝，后续版本号
重新排定。v2 可复用基线和 Proposed `0.4` 均完成后再设计和实现：

- P2P socket 的服务端观察地址与 server-reflexive candidate；
- 双向 UDP 打洞、候选优先级和路径健康探测；
- 常见 NAT 实验矩阵，以及对称/不可穿透 NAT 的稳定 Relay 回退；
- 地址变化、丢包、乱序和 MTU 黑洞后的恢复。

本阶段不依赖外部 STUN；若要加入 STUN/TURN，需新的 ADR 和威胁模型。

## Conditional `0.6`：规模与加固

- 256 在线节点容量与资源 soak；
- 指标导出发布门禁、健康检查和运维告警；
- 持续 fuzz、故障注入、连接抖动和协调服务重启；
- 内存、任务、文件描述符和队列不得持续增长。

## `1.0`：分布式稳定版

在 Linux 稳定门禁基础上完成部署/恢复演练、威胁模型复核、安全审查和明确支持矩阵。
macOS/Windows 只有在各自真实 TUN/P2P 证据完成后才提升为运行支持。

上述 `0.5`/`0.6` 编号是 Proposed `0.4` 路线被接受后的条件编号，不是已冻结发布承诺。
多协调服务 HA、Relay 路径端到端加密、多信任域、ACL、IPv6、远程管理 API、DNS、
默认/子网路由、外部 STUN/TURN 和其他 QUIC 实现的运行支持留到后续版本。
