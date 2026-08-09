# Stellaris 文档中心

> **文档快照：2026-08-09。文档适用性：Current；适用范围：文档目录与状态模型；
> 设计评审状态：N/A；ADR 决策状态：N/A；交付状态：N/A；验证状态：N/A；发布状态：
> N/A。**

<!-- stellaris-release-status:documentation-status:start -->
当前运行边界仍是 `0.3.0-alpha.1` v2，代码已经接入，发布门禁尚未完成。目标版本
`0.4.0-alpha.1` 的 `V3-R1 Identity & Relay Core` 仍是 Proposed、Not started、
Unverified。文档中的“当前”、“已实现”和“已验证”是不同状态，不能互相替代。
<!-- stellaris-release-status:documentation-status:end -->

本页是第一方文档的统一目录。`docs/` 根目录只保留日常设计、实现和治理需要直接引用的
核心权威文档；调研等非规范背景材料按类别归入子目录。开始设计或实现改造前，先阅读
[`documentation-workflow.md`](documentation-workflow.md)，并从下表找到本次变更的
权威文档。使用 AI 参与开发时，还必须先完整阅读
[`ai-iteration-playbook.md`](ai-iteration-playbook.md)。若两份文档冲突，不按“更新时间
较新”猜测，应按权威边界修正文档。

## 阅读路径

| 目的 | 建议顺序 |
| --- | --- |
| 了解项目和当前能力 | [`../README.md`](../README.md) -> [`design-overview.md`](design-overview.md) -> [`compatibility.md`](compatibility.md) |
| 理解当前 v2 实现 | [`architecture.md`](architecture.md) -> [`protocol.md`](protocol.md) -> [`security-model.md`](security-model.md) |
| 部署或排障 | [`configuration.md`](configuration.md) -> [`deployment.md`](deployment.md) -> [`troubleshooting.md`](troubleshooting.md) |
| 评审产品方向与 AI 治理 Draft | [`design-overview.md`](design-overview.md) -> [`product-direction-and-iteration-governance-plan.md`](product-direction-and-iteration-governance-plan.md) -> [`adr/0005-elastic-compute-network-product-boundary.md`](adr/0005-elastic-compute-network-product-boundary.md) 与 [`adr/0006-human-controlled-ai-iteration.md`](adr/0006-human-controlled-ai-iteration.md) |
| 评审既有 v3 技术候选 | [`design-overview.md`](design-overview.md) -> [`adr/0004-durable-node-identity-and-address-leases.md`](adr/0004-durable-node-identity-and-address-leases.md) -> [`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) |
| 调研现代方案 | [`research/modern-distributed-network-survey.md`](research/modern-distributed-network-survey.md) -> [`design-overview.md`](design-overview.md) |
| 使用 AI 开始一次迭代 | [`../AGENTS.md`](../AGENTS.md) -> [`ai-iteration-playbook.md`](ai-iteration-playbook.md) -> [`documentation-workflow.md`](documentation-workflow.md) -> 本次目标设计/ADR/Gate |
| 参与开发或发布 | [`documentation-workflow.md`](documentation-workflow.md) -> [`../CONTRIBUTING.md`](../CONTRIBUTING.md) -> [`releasing.md`](releasing.md) |

## 状态模型

文档使用六个相互独立的维度，不能把它们压缩成一个含混的“完成”状态。某个维度不适用
时写 `N/A`，不得借用其他维度的状态值。

**文档适用性：**

| 状态 | 含义 |
| --- | --- |
| Current | 描述当前代码和输入格式；不自动表示发布门禁通过 |
| Proposed | 候选方案；不能用于解释当前配置、协议或支持承诺 |
| Conditional | 只有前置决策和阶段完成后才成立的后续方向 |
| Historical | 仅保留历史，不再指导当前实现 |

**设计评审状态：** `Draft`、`Approved`、`Rejected`、`Superseded` 或 `N/A`。它用于
不一定需要 ADR 的设计/计划；`Approved` 必须记录 reviewer 和批准 commit。

**ADR 决策状态：** `Proposed`、`Accepted`、`Rejected`、`Superseded` 或 `N/A`。

**交付状态：** `Not started`、`In progress`、`Implemented` 或 `N/A`。

**验证状态：**

| 状态 | 含义 |
| --- | --- |
| Unverified | 对适用的目标 commit，尚无原子 Gate 的 latest result 同时满足 `Passed + 豁免 None`；没有记录、只有 Failed/Partial 或只有带 waiver 的 Passed 都保持此状态 |
| Partially verified | 对适用的目标 commit，至少一个但并非全部规定 Gate 的 latest result 满足 `Passed + 豁免 None` |
| Verified | 对适用的目标 commit，全部规定 Gate 的 latest result 都满足 `Passed + 豁免 None` |
| N/A | 此文档不描述需要验证的交付物 |

同一 Gate 的 current result 按验证记录首次进入目标分支 first-parent 历史的顺序取最新
结果；新的 Failed、Partial 或 waiver 会覆盖旧 Passed，并要求验证状态相应降级。记录
格式、目标 commit 约束和不可变规则见 [`verification/README.md`](verification/README.md)。

**发布状态：** `Unreleased`、`Prerelease`、`Stable` 或 `N/A`。发布与验证分开：预发布
可以仍是 Partially verified，但必须醒目标出未完成门禁；Stable 必须先达到规定的
Verified 状态。

具体版本和 ADR 的组合状态不在状态模型示例中重复，分别以本页顶部投影、
[`design-overview.md`](design-overview.md) 和 [`adr/README.md`](adr/README.md) 为准。
`Approved` 或 ADR `Accepted` 只表示允许按设计实施，不等于 Implemented、Verified 或
已经发布。

## 权威边界

| 范围 | 权威文档 | 说明 |
| --- | --- | --- |
| 文档状态模型、目录和冲突处理 | [`README.md`](README.md) | Current process |
| 改造分级、设计审批和文档影响矩阵 | [`documentation-workflow.md`](documentation-workflow.md) | Current process |
| 单人使用 AI 的状态重建、切片、自动提交和交接规则 | [`ai-iteration-playbook.md`](ai-iteration-playbook.md) | Current process；不保存当前进度副本 |
| 项目入口和快速开始 | [`../README.md`](../README.md) | Current |
| 当前系统组件与数据路径 | [`architecture.md`](architecture.md) | Current v2 |
| 当前线格式、消息方向和 ALPN | [`protocol.md`](protocol.md) | Current v2，唯一线协议规范 |
| 当前 CLI、配置 schema 和字段 | [`configuration.md`](configuration.md) | Current v2 |
| 当前威胁模型和信任边界 | [`security-model.md`](security-model.md) | Current v2 |
| 平台、传输和运行支持等级 | [`compatibility.md`](compatibility.md) | 派生支持状态以链接页投影为准 |
| 部署与故障恢复操作 | [`deployment.md`](deployment.md)、[`troubleshooting.md`](troubleshooting.md) | Current v2 |
| 当前 v2 剩余验证门禁 | [`distributed-network-plan.md`](distributed-network-plan.md) | Current planning |
| 当前、Proposed 与后续方案关系 | [`design-overview.md`](design-overview.md) | Navigation snapshot |
| 现代方案调研与非规范参考 | [`research/modern-distributed-network-survey.md`](research/modern-distributed-network-survey.md) | Current background；不属于根目录核心规范 |
| Proposed v3 单一身份、动态地址、分层 PKI 与 Relay-only 闭环；`V3-R0`/`V3-R1` | [`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) | Proposed `0.4.0-alpha.1` |
| Proposed 产品方向、Product Goal、单一 Active Work Item 与 AI 权限；`GOV-R0..R4` | [`product-direction-and-iteration-governance-plan.md`](product-direction-and-iteration-governance-plan.md) | Proposed / Draft；不改变 Current 或既有 v3 状态 |
| 决策记录 | [`adr/README.md`](adr/README.md) | 按单个 ADR 状态 |
| 门禁执行证据 | [`verification/README.md`](verification/README.md) | 按目标 commit 记录 |
| 条件版本顺序 | [`roadmap.md`](roadmap.md) | Current planning |
| 发布命令和证据要求 | [`releasing.md`](releasing.md) | Current process |
| 版本变化记录 | [`../CHANGELOG.md`](../CHANGELOG.md) | Unreleased / released history |
| 贡献与评审要求 | [`../CONTRIBUTING.md`](../CONTRIBUTING.md) | Current process |
| 项目决策治理 | [`../GOVERNANCE.md`](../GOVERNANCE.md) | Current process |
| 漏洞报告与受支持版本 | [`../SECURITY.md`](../SECURITY.md) | Current policy |

配置示例位于 [`../configs/`](../configs/)，容器示例位于
[`../deploy/docker/`](../deploy/docker/)，Linux 特权门禁说明位于
[`../tests/e2e/README.md`](../tests/e2e/README.md)。示例和测试脚本必须服从上表中的
规范；脚本存在不等于相应场景已经通过。

`vendor/` 下的 Markdown 属于第三方源码快照和补丁说明，不是 Stellaris 当前能力或支持
承诺；除同步上游或维护补丁外，不为统一措辞而编辑。

## 文档分层

```text
README / design-overview        入口与状态摘要
        |
        +-- ai-iteration-playbook  AI 协作执行纪律
        +-- documentation-workflow 改造分级与生命周期
        +-- architecture        当前组件和运行边界
        +-- protocol            当前线协议
        +-- configuration       当前 CLI/schema
        +-- security-model      当前威胁与信任边界
        +-- compatibility       当前支持证据
        |
        +-- ADR                       决策及其历史
        +-- distributed-network-plan  Current v2 验收门禁
        +-- product direction plan    Proposed 产品方向与 AI 迭代治理
        +-- Proposed plans            候选设计与退出门禁
        +-- roadmap                   条件实施顺序
        |
        +-- deployment          操作手册
        +-- troubleshooting     排障手册
        +-- releasing           发布证据
        |
        +-- research/          非规范调研与背景材料
```

同一个协议字段、配置字段或状态机不得在多份文档中各自形成规范。其他文档可以摘要，
但必须链接到权威来源，并明确摘要的适用版本和状态。

## 维护规则

- D2-D4 可先用 `Design-only` PR 合并 Draft/Proposed/Rejected 等设计记录，但只有设计
  Approved、所需 ADR Accepted 后才能用 `Implementation` PR 改变公共行为。D1 修复
  服从既有 Current 契约，不要求虚构 Proposed 方案。
- 协议、配置和持久状态发生破坏式变化时同时升级版本，并明确拒绝旧输入。
- 本项目当前不承担 v1/v2 兼容成本；除非新的 ADR 明确推翻该决策，不增加迁移器、
  双栈 listener、协议降级或兼容 feature。
- 代码接入后只能写“已实现、门禁未完成”；只有目标 commit 的可审计证据齐全后才能
  写“已验证”或提升平台支持等级。
- 关闭门禁时在 [`verification/`](verification/README.md) 记录 commit、环境、命令、
  结果和 artifact；勾选框本身不是证据。
- 每次变更按 [`documentation-workflow.md`](documentation-workflow.md) 的影响矩阵同步
  文档，并运行本地链接检查、格式检查和 `git diff --check`。
