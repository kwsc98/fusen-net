# ADR 0005：弹性算力网络产品边界与优先级

- 文档适用性：Proposed
- 适用范围：Stellaris 产品边界、信任模型、规模目标与方向优先级
- 设计评审状态：N/A
- ADR 决策状态：Proposed
- 替代关系：不直接替代 ADR 0001–0004；未来技术路线 ADR 另行处理与既有网络决策的关系
- 交付状态：Not started
- 验证状态：Unverified
- 发布状态：Unreleased
- 日期：2026-08-03
- 最后核对：working tree（2026-08-03）

## 背景

当前 v2 是自托管、单租户、单协调实例、最多 256 个静态节点的 IPv4 overlay，使用可信
Relay 与按需 LAN P2P。既有 Proposed v3 选择单租户、单 Coordinator、最多 256 个持久
节点和 Relay-only。两者都没有把“平台统一托管的弹性算力网络底座”作为产品边界，也
没有把租户加任务 capability、单地域多副本和 100,000 注册/10,000 在线作为首个核心
版本约束。

如果不先冻结产品责任、信任边界和优先级，后续技术设计可能继续围绕通用 overlay 或
单机原型优化，而不是完成调度器到动态算力节点的可靠连接闭环。本 ADR 只决定产品和
架构方向，不选择最终 wire、配置、数据库、复制算法或 NAT 实现。

详细治理接口和实施顺序由
[`产品方向与 AI 迭代治理改造计划`](../product-direction-and-iteration-governance-plan.md)
定义。

## 提议决策

### 产品责任边界

Stellaris 定位为平台统一托管的弹性算力调度系统之独立网络底座，服务平台调度和运维
团队。它负责节点网络身份、发现、连接、隔离、路由、回收、观测和稳定集成接口。

外部调度器独立负责资源发现、placement、任务 desired state、任务生命周期、业务重试
和计费。Stellaris 不实现排队、调度算法、资源模型、工作负载执行器或计费系统；在线
节点目录也不能成为调度结果的权威。

节点部署形态包括边缘算力、C 端算力、个人终端和集群节点。部署形态不表示信任等级，
所有节点彼此默认不可信。

### Product Goal 与优先级

采用以下稳定 Product Goal：

| Product Goal | 决策结果 |
| --- | --- |
| **PD-CONTROL-01** | 调度器与动态算力节点之间具有可靠、可恢复的双向控制连接 |
| **PD-ISOLATION-01** | 节点默认隔离，授权绑定租户和任务且可撤销 |
| **PD-SCALE-01** | 首个核心版本直接按单地域多副本、100,000 注册和 10,000 在线设计与验证 |
| **PD-DATA-01** | 任务数据按策略使用 NAT/P2P，失败确定性回退受控 Relay |
| **PD-PLATFORM-01** | Linux 优先，Windows 和 macOS 分别后续验证 |
| **PD-OPS-01** | 连接、撤销、故障转移、恢复和回滚可观测、可审计、可演练 |

安全隔离是不可妥协的不变量，必须在任何可靠性、规模或数据路径阶段开始前先建立
tenant/task capability、默认拒绝和撤销底座。在该底座上，优先级依次为可靠控制闭环、
最终规模与单地域多副本、任务数据路径、跨平台 Agent 和 Production Preview。

### 身份、信任与授权边界

平台控制面、capability issuer 和受控 Relay 属于首阶段受信平台边界。每次控制或任务
数据授权至少绑定 tenant、task、发起 subject、目标 audience node、允许 action/data
scope、签发和到期时间、唯一 capability ID 与撤销状态。

capability 必须短期、最小权限、可撤销并默认拒绝。同租户 enrollment 不自动获得互通
权；underlay locator、overlay IP、节点证书、在线 session 和路径 Ready 都不能单独授予
访问权限。授权服务不可判定、状态分叉、到期或撤销时必须 fail-closed。

### 控制面与数据路径

首条产品闭环固定为：调度器授权某租户任务访问指定算力节点，控制消息经平台 Relay
可靠送达；任务数据仅在策略与 capability 同时允许时尝试 NAT/P2P，失败回退受控 Relay；
到期或撤销后所有路径关闭。

控制流固定经平台 Relay。任务数据的 Relay 和 NAT/P2P 必须使用同一授权语义；路径切换
只能改变传输选择，不得扩大 scope、延长期限或主动复制同一个用户包。系统不提供永久
full-mesh、通用 VPN、默认路由或用户自定义任意路由。

### 规模、可用性与平台顺序

首个核心技术设计必须直接冻结单地域多副本，目标容量为 100,000 个 durable
registrations 和 10,000 个 concurrent online nodes。不能先把产品模型冻结为单
Coordinator、256 节点或单机 snapshot，再把最终规模作为没有 Gate 的未来优化。

后续设计必须定义副本状态所有权、session ownership、幂等、故障转移、重连风暴、单
副本丢失、恢复、资源预算和 SLO，并以最终容量执行 Gate。跨地域双活后置。

Linux 首先取得真实网络、故障和规模证据；Windows 与 macOS 分别使用独立 Gate。
compile-only 不表示运行支持。

### 版本、证据与人工评审边界

Stable 前允许版本化的破坏式 wire、schema、state 和 Agent 重建。除非新的 Accepted ADR
改变规则，不实现 v1/v2/v3 migration、dual-stack listener、protocol downgrade 或隐藏
兼容输入。

先使用内部 dogfood 获得真实调度任务证据。Production Preview 前必须完成外部人类安全
和网络评审，关闭全部 Critical/High。其他发现只能由人类 owner 明确接受并记录；AI
不得签发 waiver、接受风险或把缺少证据的能力写成已支持。

## 破坏式边界与 ADR 关系

本 ADR 不直接替代 ADR 0001–0004，也不改变它们的生命周期。ADR 0003 继续描述 Current
v2；ADR 0004 继续保持 Proposed，旧 v3 设计继续保持 Draft / Not started。

如果本 ADR Accepted，任何未来技术设计都必须满足本 ADR 的产品边界。现有 ADR 0004 与
这些边界存在冲突，但处理方式必须在后续独立技术 Design-only 中由维护者决定：可以先
修改仍为 Proposed 的候选内容再评审，或明确拒绝 ADR 0004 并新增技术 ADR。本 ADR 的
接受本身不自动执行其中任一选择，也不授权 runtime 实现。

Current v2 与未来产品方向不一致只表示遗留实现尚未替换，不允许提前把 Current 文档
改写成新能力。

## 非目标

- 通用 VPN、员工远程办公、默认全互通、永久全网 TUN 或任意路由产品；
- 调度算法、资源模型、placement、计费和工作负载生命周期；
- 首阶段跨地域双活、多云 federation 或 Stable 兼容承诺；
- 在本 ADR 中选择 wire、ALPN、配置 schema、持久数据库、共识算法或 NAT 库；
- 直接接受、拒绝、取代或改写 ADR 0004；
- 将 Product Goal 描述为当前 runtime 能力或发布支持。

## 备选方案

- **继续构建通用自托管 overlay：**复用现有定位，但无法为调度器、租户任务隔离和弹性
  节点规模提供明确优先级。
- **把调度器并入 Stellaris：**减少一个系统边界，但会把资源和工作负载领域引入网络
  基础设施，扩大耦合与故障范围。
- **先按 100 或 1,000 在线节点实现，再决定最终模型：**短期更容易，但可能冻结单机
  权威和无法演进的 session/state 语义，无法证明核心模型适用于目标规模。
- **所有任务数据永久经 Relay：**授权路径简单，但形成确定的带宽与可用性瓶颈，不能
  满足长期弹性算力数据路径目标。
- **节点 enrollment 后同租户全互通：**运维简单，但违反节点默认不可信和任务最小权限。
- **首阶段直接跨地域双活：**扩大失效和一致性设计范围，推迟首个单地域可靠闭环。

## 后果

正面结果：

- 调度器与 Stellaris 的责任和权威不再混淆；
- 产品目标、信任边界、最终规模和数据路径具有稳定决策依据；
- 未来技术方案可以被明确判定为符合或偏离方向；
- P2P 只作为受策略约束的任务路径，不再滑向通用全网互联；
- Preview 具备真实 dogfood、外部评审和故障恢复门槛。

代价与风险：

- 首个核心设计必须处理多副本、最终规模和 capability，前期设计与验证成本更高；
- 控制 Relay 仍是受信平台边界并暴露流量元数据；
- NAT/P2P 与 Relay fallback 增加路径状态、恢复和负向安全测试；
- 当前 v2 和既有 Proposed v3 不能直接作为目标实现，需要单独处理基线和技术路线；
- Windows/macOS 和跨地域能力会明确后置。

## 接受条件

在 ADR 决策状态改为 Accepted 前，必须满足：

- 产品定位、调度器/Stellaris 责任边界和六个 Product Goal 已冻结；
- 节点默认不可信、tenant/task capability 最低字段和 fail-closed 语义已冻结；
- 控制 Relay、策略 NAT/P2P 与 Relay fallback 的路径边界已冻结；
- 100,000 注册、10,000 在线、单地域多副本和 Linux 优先顺序已冻结；
- 通用 VPN、调度/计费、首阶段跨地域和隐藏兼容等非目标已冻结；
- Production Preview 前的 dogfood、外部安全/网络评审和风险接受权限已冻结；
- tenant/task capability、默认拒绝和撤销底座先于可靠控制、HA 与规模阶段的顺序已冻结；
- 本 ADR 与 ADR 0003/0004 的不替代关系以及未来单独决策方式已冻结；
- 关联 Draft 设计完整冻结治理和后续技术 Design-only 顺序；
- 治理提案已经公开评论至少 7x24 小时，实质异议均已解决；
- 维护者对本 ADR 路径给出明确、作用域限定的 Accepted 授权；合并 Proposed 文件、要求
  “实现计划”或普通“继续”均不构成接受。
