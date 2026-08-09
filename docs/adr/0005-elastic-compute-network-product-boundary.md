# ADR 0005：弹性算力网络产品边界与优先级

- 文档适用性：Proposed
- 适用范围：Stellaris 产品边界、信任模型、规模目标与方向优先级
- 设计评审状态：N/A
- ADR 决策状态：Proposed
- 替代关系：不直接替代 ADR 0001–0004；既有技术路线另行决策
- 交付状态：Not started
- 验证状态：Unverified
- 发布状态：Unreleased
- 日期：2026-08-09
- 最后核对：working tree（2026-08-09）

## 背景

Current v2 和既有 Proposed v3 都围绕单租户、小规模 overlay 展开，没有把“弹性算力
调度平台的网络底座”作为产品边界。缺少稳定定位时，后续设计容易继续优化通用组网或
单机原型，而不是完成调度器到动态算力节点的安全、可靠连接闭环。

本 ADR 只决定产品责任、信任边界和优先级，不选择 wire、配置、数据库、复制算法或 NAT
实现。完整目标、AI 迭代合同和 Gate 见
[`产品方向与 AI 迭代治理计划`](../product-direction-and-iteration-governance-plan.md)。

## 提议决策

### 产品责任

Stellaris 是面向弹性算力调度平台的托管网络底座。它通过任务级授权，可靠连接调度器与
动态、默认不互信的算力节点，并负责：

- enrollment、网络 principal、短期凭据和在线 session；
- 节点发现及调度器所需的稳定网络接口；
- tenant/task 授权的验证、撤销、过期和审计；
- 控制 Relay，以及任务数据路径的建立、切换、回退和回收；
- 网络可达性、路径、故障转移和恢复观测。

外部调度器负责资源发现、placement、任务 desired state、任务生命周期、业务重试、
工作负载执行和计费。Stellaris 不成为调度结果权威，也不允许调度器绕过网络授权。

### Product Goal 与优先级

| Product Goal | 决策结果 |
| --- | --- |
| **PD-ISOLATION-01** | 节点默认隔离，授权绑定 tenant/task 且可撤销 |
| **PD-CONTROL-01** | 调度器与动态节点之间具有可靠、可恢复、可审计的双向控制连接 |
| **PD-SCALE-01** | 核心版本按单地域多副本、100,000 注册和 10,000 在线设计与验证 |
| **PD-DATA-01** | 任务数据按策略使用 NAT/P2P，失败确定性回退受控 Relay |
| **PD-PLATFORM-01** | Linux 优先，Windows 和 macOS 分别后续验证 |
| **PD-OPS-01** | 连接、撤销、故障转移、恢复和回滚可观测、可审计、可演练 |

安全隔离是不可妥协的不变量。先建立 tenant/task capability、默认拒绝和撤销，再依次
推进可靠控制、单地域多副本与目标规模、任务数据路径、跨平台 Agent 和 Production
Preview。实现按小切片推进，但不能把长期模型悄悄冻结为单 Coordinator 或 256 节点。

### 授权与数据路径

节点部署形态不代表信任等级，所有节点彼此默认不可信。同租户 enrollment 不自动获得
互通权。每次控制或数据授权至少绑定 tenant、task、subject、audience node、允许的
action/data scope、签发和到期时间、唯一 capability ID、issuer 与撤销状态。

IP、证书、在线 session、locator 和路径 Ready 都不能单独授予访问权限。授权必须短期、
最小权限、可撤销并默认拒绝；状态分叉、到期、撤销或不可判定时 fail-closed。

控制消息固定经平台 Relay。任务数据只有策略与 capability 同时允许时才能尝试 NAT/P2P，
失败确定性回退受控 Relay。路径切换只能改变传输选择，不能扩大 scope、延长期限或主动
复制同一个用户包。平台控制面、capability issuer 和 Relay 是首阶段受信边界。

### 规模、平台与版本

后续核心技术设计必须冻结单地域多副本、100,000 个 durable registrations、10,000 个
concurrent online nodes、状态与 session ownership、幂等、故障转移、恢复、资源预算和
SLO。跨地域双活后置。

Linux 首先取得真实网络、故障和规模证据；Windows 与 macOS 使用独立 Gate。
compile-only 不表示运行支持。Production Preview 前必须完成外部人类安全与网络评审并
关闭全部 Critical/High；其他风险只能由人类 owner 明确接受。

Stable 前允许版本化的破坏式 wire、schema、state 和 Agent 重建。除非新的 Accepted
ADR 改变规则，不实现 v1/v2/v3 migration、dual-stack listener、protocol downgrade 或
隐藏旧输入。

## 与既有 ADR 的关系

本 ADR 不改变 ADR 0001–0004 的状态。ADR 0003 继续描述 Current v2；ADR 0004 和旧 v3
设计继续保持 Proposed / Draft / Not started。

如果本 ADR Accepted，未来技术设计必须符合本文产品边界。ADR 0004 与该边界存在冲突，
但只能通过后续独立 Design-only 选择修改仍为 Proposed 的候选，或拒绝并新增技术 ADR。
接受本 ADR 不自动执行其中任一选择，也不授权 runtime 实现。

## 非目标

- 通用 VPN、员工远程办公、默认全互通、永久 full-mesh TUN 或任意路由；
- 调度算法、资源模型、placement、排队、工作负载生命周期和计费；
- 首阶段跨地域双活、多云 federation 或 Stable 兼容承诺；
- 在本 ADR 中决定具体协议、存储、共识算法或 NAT 库；
- 把 Product Goal 描述成 Current runtime 能力或发布支持。

## 备选方案

- **继续通用 overlay：**复用现有方向，但无法稳定约束调度器接口、任务隔离和规模优先级。
- **把调度器并入 Stellaris：**减少一个接口，却扩大领域耦合和故障范围。
- **先冻结单机小规模模型：**短期简单，但可能形成无法演进的状态与 session 语义。
- **所有任务数据永久经 Relay：**授权简单，但形成确定的带宽和可用性瓶颈。
- **enrollment 后同租户全互通：**运维简单，但违反任务最小权限和默认不信任。

## 后果

产品方向、调度器边界、信任模型和长期规模将有稳定裁决来源；代价是首个核心设计必须
较早处理 capability、多副本和容量模型，NAT/P2P 与 Relay fallback 也会增加状态与负向
测试。Windows/macOS、跨地域和 Stable 明确后置。

## 接受条件

在改为 Accepted 前，维护者必须确认：

- 产品责任、六个 Product Goal、优先级和 Now / Next / Later / Never 已冻结；
- tenant/task capability、默认拒绝、撤销和 fail-closed 语义已冻结；
- 控制 Relay、策略 NAT/P2P 与 Relay fallback 的边界已冻结；
- 100,000 注册、10,000 在线、单地域多副本和 Linux 优先已冻结；
- 通用 VPN、调度/计费、跨地域和隐藏兼容等非目标已冻结；
- dogfood、外部安全/网络评审和人工风险接受边界已冻结；
- 与 ADR 0003/0004 的关系及后续独立技术决策方式已冻结；
- 关联 Draft 已冻结后续治理和技术 Design-only 顺序；
- 提案已公开评论至少 7x24 小时，实质异议已解决；
- 维护者对本 ADR 路径给出明确、作用域限定的 Accepted 授权。

合并 Proposed 文件、普通“继续”或要求“实现计划”均不构成接受。
