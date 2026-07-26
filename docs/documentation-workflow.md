# 文档驱动改造流程

> **文档适用性：Current；适用范围：工程流程；设计评审状态：N/A；ADR 决策状态：
> N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**

本流程适用于 Stellaris 的功能、协议、配置、身份、安全、持久状态、平台支持和发布
门禁变更。目标是让文档先定义问题、边界和可验证结果，代码实现再服从这些约束。

文档入口和权威边界见 [`README.md`](README.md)。本流程是工程规则，不是当前协议或
配置规范。

## 核心规则

1. **先记录当前事实。** 提案必须引用当前权威文档和可复现证据，不能把计划当现状。
2. **先冻结外部行为，再写实现。** 公共接口、状态机、安全不变量和退出门禁必须在
   实现前进入 Proposed 文档；重大变化还必须有 ADR。
3. **一个语义只有一个权威来源。** 摘要只能链接规范，不复制一套可独立演进的定义。
4. **决策、交付和验证分开。** ADR Accepted、代码 Implemented、门禁 Verified 是
   三件事，必须分别记录。
5. **破坏式升级必须显式。** 当前路线不兼容旧协议、旧 schema、旧状态或旧 Agent。
   除非新 ADR 明确授权，不实现迁移器、双栈、降级或兼容 feature。
6. **无法证明就降级表述。** 没有对应 commit 的 CI/runner 记录时，文档只能写
   “未验证”或“门禁未完成”。
7. **设计、实现和证据分开提交。** `Design-only` PR 只推进设计/ADR 生命周期；
   `Implementation` PR 修改代码、配置和 Current 契约；`Evidence-only` PR 只追加不可变
   门禁证据并推进验证/发布元数据。

中文 `docs/` 是架构、协议、配置和安全的权威正文；`README.en.md` 是英文入口摘要，
必须与中文状态一致但不单独定义规范。代码标识、CLI、字段、ALPN 和协议消息保持原始
大小写。`vendor/` 中的上游 README/PATCHES 是第三方快照说明，不构成 Stellaris 能力声明，
也不为统一文风而改写。

## 变更分级

| 级别 | 例子 | 实现前必须具备 |
| --- | --- | --- |
| D0 文档修正 | 错字、断链、不改变含义的澄清 | 指明权威来源；确认不改变行为 |
| D1 当前行为修复 | 在既有 Current 契约内修复 bug、补测试 | Issue/PR 中的问题证据、权威契约、回归测试；不要求 Proposed 设计 |
| D2 公共行为改造 | 不触发下列 ADR 规则的 CLI/诊断/指标或局部行为变化 | Approved 设计、受影响规范、原子验收门禁，并写明不触发 ADR 的理由 |
| D3 ADR 级改造 | 协议、核心配置/接口、CA、身份、授权、持久状态、数据路径、HA、兼容策略 | Accepted ADR、Approved 设计、威胁边界、失败语义、回滚/恢复方案 |
| D4 发布支持变化 | 提升平台、规模或稳定等级 | 全部门禁证据、兼容矩阵、部署与恢复演练、Release Notes |

以下变化必须新增 ADR，不能只改 roadmap：

- 线协议、ALPN、版本或兼容策略；
- 身份、证书、信任根、授权或撤销模型；
- 持久状态不变量、地址所有权、回滚和恢复语义；
- Server/Agent 角色、数据路径、故障语义或核心公共接口；
- 支持平台、发布门禁或治理规则的实质变化。

小型实现细节不需要 ADR，但仍应更新其权威规范和测试。D2 评审中一旦发现命中上述
任一条件，必须改分为 D3；不得保留 D2 分类再把 ADR 填成 `N/A`。

## 标准流程

```text
问题与当前证据
      |
      v
Proposed 设计 + 非目标 + 验收门禁
      |
      v
ADR 决策（需要时）
      |
      v
按阶段实现 + 测试
      |
      v
同步 Current 规范，标记 Implemented
      |
      v
执行真实门禁并记录证据
      |
      v
标记 Verified / 更新支持等级 / 发布
```

### 1. 建立改造记录

D1 修复在 Issue 或 PR 中记录复现证据，引用既有 Current 契约并增加回归测试即可；若
预期行为本身需要改变，立即升级为 D2。D2-D4 在代码修改前必须新建或更新一份
Proposed 设计文档，至少包含：

```markdown
# 标题

- 文档适用性：Proposed
- 适用范围：<涉及的组件/协议/版本>
- 目标版本：候选版本或“未冻结”
- 方案快照：YYYY-MM-DD
- 设计评审状态：Draft
- 设计批准：<reviewer + approval commit，未批准时写 N/A>
- 关联 ADR：docs/adr/NNNN-name.md[, docs/adr/NNNN-name.md] 或“N/A: <具体理由>”
- ADR 决策状态：Proposed / Accepted / N/A
- 交付状态：Not started
- 验证状态：Unverified
- 发布状态：Unreleased
- 最后核对：<commit SHA 或 working tree>

## 问题与证据
## 当前行为
## 目标与非目标
## 公共接口和版本边界
## 身份、安全与信任边界
## 状态机、持久化和失败语义
## 实施阶段与退出门禁
## 测试与验收
## 部署、恢复与回滚
## 受影响文档
## 未决问题
```

当前行为必须链接 `architecture.md`、`protocol.md`、`configuration.md` 等权威来源；
不要在 Proposed 文档中悄悄重定义当前输入。

D2-D4 可以先用 `Design-only` PR 合并 Draft 设计、Proposed ADR，或记录后续的
Approved/Accepted/Rejected/Superseded 状态。Design-only 合并只表示仓库保留这份设计
记录，不表示维护者已经批准设计、接受 ADR 或授权实现。它只能修改 PR 中声明的设计、
ADR，以及 `docs/README.md`、`docs/design-overview.md`、`docs/roadmap.md`、
`docs/adr/README.md` 四个导航索引；不得夹带源码、配置、示例、Current 权威契约、部署
步骤、能力声明或 changelog。新设计/ADR 必须保持 `Not started + Unverified +
Unreleased`；既有记录的交付、验证和发布状态也不得在 Design-only 中升级或降级。需要
推进交付时另开 `Implementation` PR；推进验证或发布状态时另开 `Evidence-only` PR。

`关联 ADR` 是机器可读的唯一集合：D2 必须写 `N/A: <具体理由>`，D3-D4 必须使用逗号
加空格分隔的仓库相对 ADR 路径。PR 的 `Required ADR(s)` 必须与该集合精确相等，不能
只声明其中一部分。ADR-only Design-only PR 可以暂时没有详细设计；一旦建立详细设计，
关联集合即按上述规则校验。

### 2. 决策与冻结

所有 D2-D4 设计先处于 `Draft`。维护者确认当前证据、公共行为、非目标、安全/恢复语义
和原子 Gate ID 已冻结后，将设计评审状态改为 `Approved`，并记录 reviewer 与批准
commit。批准 commit 指向维护者实际评审的冻结 Proposed 内容；随后提交的 Approved
元数据记录该已合入当前历史的完整 SHA，避免要求一个 commit 在自身内容中引用自己的
哈希。采用 squash merge 时应记录 squash 后进入目标分支的 commit，而不是已被压缩掉的
分支 SHA。
Rejected/Superseded 保留历史且不得继续指导实现。没有 ADR 的 D2 也必须经过这一批准
步骤。合并 ADR 文件本身不改变决策状态；只有元数据明确写为 `Accepted` 才表示决策
已经接受。Rejected/Superseded 的设计和 ADR 应把文档适用性改为 Historical。

需要 ADR 时，ADR 从 `Proposed` 开始，至少记录背景、决策、备选方案、后果、破坏式
边界和接受条件。设计只能在相关 ADR 改为 `Accepted` 后批准；如果取代旧 ADR，新 ADR
显式列出被替代条款，旧 ADR 保持历史内容并标记 `Superseded`。
自动校验会在设计从 Draft 进入 Approved 时读取 `关联 ADR`，并要求每个 ADR 在批准
快照、PR base 和当前 HEAD 均为 Accepted；`Approved + Proposed ADR` 是非法状态。

设计 Approved 或 ADR Accepted 只表示“允许按该契约实现”。它们不允许 README、兼容
矩阵或部署指南提前声称能力可用。

批准 commit 冻结设计正文、公共行为和 Gate 定义。批准后只允许更新顶部的文档适用性、
设计评审状态、交付、验证、发布和最后核对等生命周期元数据；正文或不可变元数据发生
变化时必须回到 Draft，重新评审并记录新的批准 commit。Accepted ADR 的决策正文和
不可变元数据不得原地重写；交付/验证等生命周期元数据可以更新，推翻决策必须新增替代
ADR。设计一旦定稿，原 `设计批准` 记录也属于不可改写的历史：Superseded 保留原批准
记录，未曾 Approved 的 Draft/Rejected/Superseded 必须记录 `设计批准：N/A`。

设计和 ADR 的仓库路径一经合并即保持稳定，不通过 rename 改写历史；名称或范围需要
替换时新增记录并将旧记录标为 Superseded。单份设计当前使用一个聚合 ADR 决策状态，
所以同一 PR 声明的多个 ADR 必须处于同一状态；不同状态的决策应分开推进。

### 3. 在计划中定义验收

每个实施阶段必须使用稳定 ID，并同时给出交付和退出门禁。一个 Gate ID 只能对应一个
可以独立判定 Passed/Failed/Partial 的结果；阶段包含多个行为时使用子 ID，例如
`V3-P4-RELAY-01`，不能只用 `V3-P4` 代表一整组测试。后续验证记录引用相同 ID。门禁
应是可观察结果，例如：

- 指定输入被接受或 fail-closed 拒绝；
- 指定状态转换在重启、重试和故障点后保持不变量；
- 两节点/多节点网络行为和包路径证据；
- 明确节点数、持续时间、资源项和允许增长；
- 明确平台、权限、网络拓扑和测试命令。

“增加测试”“优化稳定性”或“支持大规模”不是可验收门禁。

### 4. 实现期间保持文档同步

`Implementation` PR 必须引用已 Approved 的设计、其批准 commit、原子 Gate ID 和适用的
Accepted ADR，并只实现一个明确阶段。D2 必须明确写出“不触发 ADR”的理由；D3-D4
必须引用 Accepted ADR。D2-D4 缺少任一前置条件时不得合并，也不能用 waiver 或
`Not applicable` 绕过。公共格式一旦开始切换，同一阶段不得存在两个权威身份或协议
模型。Stellaris 的 v3 路线要求一次破坏式切换：旧 v2 输入应明确拒绝，而不是保留隐藏
兼容路径。

Implementation 可以推进交付状态，但不得新增 `docs/verification/` 记录，也不得改变
任何文档的验证或发布状态。即使测试在同一 PR 中通过，也只能先合并实现，再对已进入
目标分支历史的实现 commit 运行门禁，随后用独立 `Evidence-only` PR 记录结果。

实现完成但门禁未关闭时：

- 当前规范更新为实际代码行为；
- 详细计划写“Implemented, gates incomplete”；路线图只更新阶段与决策位置；
- 兼容矩阵继续写“未验证”；
- 未执行的真实测试保持未勾选；
- `CHANGELOG.md` 的 `Unreleased` 记录用户可见变化和破坏边界。

### 5. 验证与状态升级

只有目标 commit 对应的 CI、特权 runner 或人工演练结果可以关闭门禁。证据写入
[`verification/`](verification/README.md)，至少记录 commit、环境、命令、持续时间、
结果和 artifact。失败、跳过、ignored test、空测试名称或只有 harness 均不能计为通过。

验证结果使用 D4 `Evidence-only` PR。该 PR 必须：

- 只新增 `docs/verification/YYYY-MM-DD-<scope>-<short-sha>.md`，不得修改、rename 或删除
  已合并记录；失败后重跑必须新增另一份记录并保留失败历史；
- 在顶部写唯一 `证据目标：<40 位小写 commit SHA>` 和
  `总体结果：Passed|Failed|Partial`，并写 `豁免：None` 或具体 waiver；目标 commit
  必须已经是 PR base 的祖先；
- 使用固定的 `Gate ID / 命令或场景 / 预期 / 实际 / 结果 / Artifact / hash` 六列表格，
  每行 artifact 必须包含 `sha256:<64 位小写十六进制>`，PR 的 Atomic Gate ID 集合与
  本次新增记录的行集合精确相等；
- 只允许修改声明设计、ADR 和 Current 文档顶部的验证、发布及最后核对元数据；不得混入
  源码、配置、Current 契约正文、交付状态或 Gate 定义变更；
- 只有至少一个 Gate 有 Passed 证据时才能进入 Partially verified；进入 Verified 时，
  同一证据目标的追加记录必须覆盖冻结设计中的全部原子 Gate；Stable 还要求同一文档
  已为 Verified。每个 Gate 按记录首次进入 first-parent 历史的顺序采用最新结果，只有
  `Passed + 豁免 None` 才计为通过；后续 Failed、Partial 或 waiver 覆盖旧 Passed，状态
  必须在同一 Evidence-only PR 中相应降级。同一 PR 的多份新记录不得重复 Gate ID。

证据目标可以早于 PR base，但从目标到 base 之间只能存在追加证据及验证/发布元数据
变化；出现源码、配置或契约正文变化时，旧结果不能证明新实现，必须对新的 commit 重跑。
证据记录本身使用 `N/A` 交付/验证/发布状态，因为它保存事实，不代表产品生命周期。

### 当前 v2 的稳定资格路径

[`distributed-network-plan.md`](distributed-network-plan.md) 是
`Current + 设计评审 N/A` 的 v2 实施/验收基线，不是 Approved Proposed 设计，
因此不能直接作为 `Evidence-only` PR 声明的 Proposed design。如果 ADR 0004
最终 Rejected，且项目决定继续发布稳定 v2，必须先按 D4 `Design-only`
流程新建一份“v2 稳定资格设计”；冻结快照进入目标分支后，再用后续
`Design-only` PR 记录批准。它的 `关联 ADR` 必须列出资格范围内全部仍有效的
Accepted ADR（当前为 ADR 0002 和 ADR 0003），不得把 Rejected ADR 0004
当作已接受前置条件。

资格设计必须链接 v2 计划中的权威 Gate 定义，不再复制命令、预期或退出
条件；但为了让 PR 契约检查器能冻结和比对范围，它必须包含一份完整的原子
Gate ID manifest。该 manifest 只列 ID 和权威来源链接，其集合必须与 v2 稳定发布
门禁完全一致。设计按正常快照流程 Approved 后，才能执行门禁并由 D4
`Evidence-only` PR 引用它。这条路径没有自举 waiver；资格设计、Gate 范围或
Current 契约变化后，必须重新批准并对新 commit 重跑证据。

全部门禁完成后，`Evidence-only` 只更新详细计划、设计、ADR 和声明的
Current 文档顶部验证/发布/最后核对元数据。详细计划中的 Gate 定义、初始
`Open` 状态和阶段正文都属于已冻结契约，不回写为 `Passed`；每个 Gate 的
当前结果只按 [`verification/`](verification/README.md) 中不可变记录的 latest-result
规则计算。这样不会因“记录门禁已通过”而立即改写被验证的契约、使证据失效。

Evidence-only 为了可自动审计，不修改 README、兼容矩阵表格或部署正文。元数据升级合并
后，再使用独立、纯文档的 D4 Implementation PR 同步人类可读状态：

- `compatibility.md` 只更新预留的支持状态投影；
- `roadmap.md` 只更新预留的路线状态投影，不改条件顺序或 Gate；
- README 和部署文档只更新预留的可用性投影；
- `CHANGELOG.md` 和 Release Notes 记录已有证据支撑的状态。

该 PR 不得再次改变验证/发布元数据，也不得夹带 runtime/config 变化，并必须引用
已经合并的证据记录。这样入口文档不会长期停留在旧状态，也不会把能力声明与证据
生成混成一个提交。

PR 模板的 `Evidence record(s)` 在 D0-D3 Implementation 以及尚无证据的 D4 实现阶段写
`Not applicable: <具体理由>`。D4 Implementation 可以列出证据，但每个路径都必须是
PR base 已存在、保持逐字节不变的规范 `docs/verification/...md` 记录。为避免投影 PR
靠声明绕过检查，校验器会把每个 changed path 的 PR base/HEAD 内容分别解析，并将已存在
的 allowlisted marker 内容替换为相同占位符。只有每个 changed path 都是下表中的
marker 文档或根目录附属投影 `README.md`、`README.en.md`、`CHANGELOG.md`，两侧 marker
集合相同、marker 文档归一化后逐字节一致，并且至少一个 marker 内容实际改变时，才判定
为状态投影并强制至少列出一份已合并证据。证据不能在该 PR 中新增、修改、rename 或删除。

投影引用的所有证据记录必须使用同一个证据目标；该目标必须是 PR base 的祖先，且从
目标到 base 之间不得出现 runtime、配置或归一化后仍有差异的契约变化。每份引用记录的
`总体结果` 必须为 `Passed`、`豁免` 必须为 `None`，记录中的 Gate 行并集必须无重复，
并同时精确等于 PR 声明的 `Atomic Gate ID(s)` 和 Approved 设计冻结的完整原子 Gate
集合。每个 Gate 还必须是 PR base first-parent 追加顺序下的最新 `Passed + 豁免 None`
结果；每份历史记录必须只有一次 first-parent 追加并从追加 commit 到 base 保持存在且
逐字节不变。后续 `Failed`、`Partial` 或 waiver 会使更早的 Passed 失效，不能用于状态
投影，删除或改写较新的结果同样会 fail-closed。

三份根目录文档不是权威状态源，不能单独推进验证、支持或 Stable 声明；这类状态变化
必须与同一 PR 中对应的 `docs/` marker 投影一起发生。它们可以作为该投影的附属摘要，
但不能通过多改一份根文档让整个 PR 逃离投影识别。

新增/删除 marker、修改块外正文或混入其他文件都不是状态投影。这样的 D4
Implementation 可以在执行证据前保持 `Not applicable`，用于先建立 Current 契约或预留
marker；但它会让针对旧契约的证据 stale，后续必须以新 commit 重跑门禁。若非投影 D4
在证据生成后引用已有结果，仍只能列 PR base 中的不可变记录。

`docs/` 内的状态投影必须在证据目标 commit **之前**预留以下严格命名区块；D4 投影 PR
只能替换 start/end 之间的人类可读验证、发布或支持状态，不能新增、删除、移动 marker，
也不能改区块外正文：

```markdown
<!-- stellaris-release-status:<allowlisted-id>:start -->
<derived human-readable status; no heading or Gate ID>
<!-- stellaris-release-status:<allowlisted-id>:end -->
```

| 文档 | 必需的 projection ID |
| --- | --- |
| `docs/README.md` | `documentation-status` |
| `docs/design-overview.md` | `design-status` |
| `docs/compatibility.md` | `support-status`、`protocol-status`、`transport-status`、`platform-status` |
| `docs/deployment.md` | `deployment-status` |
| `docs/roadmap.md` | `roadmap-status` |
| `docs/releasing.md` | `release-status` |
| `docs/adr/README.md` | `adr-status` |
| `docs/verification/README.md` | `verification-status` |

表中每个 path/ID 必须恰有一个区块，marker 必须严格成对且区块非空；区块内禁止 Markdown
标题和原子 Gate ID。compatibility 的四个区块分别只投影总体、协议、传输和平台支持
状态，不能借机改写线协议或运行边界。`distributed-network-plan.md`、ADR 正文、协议、
配置、安全、架构以及其他 Current 契约没有投影例外。`scripts/check-release-evidence.rb`
会同时归一化证据生命周期元数据和上述区块；marker 不在证据目标中、重复/不配对、
使用未授权 ID、块内出现 Gate，或块外任意 `docs/` 正文变化都会使证据失效。若需要新的
投影边界，应先在普通 D4 设计/实现流程中加入并冻结，再以该 commit 重跑门禁，不能在
证据归档后补 marker。

## 文档影响矩阵

| 改动 | 必须检查或更新 |
| --- | --- |
| CLI 或配置 | `configuration.md`、示例 TOML、README、deployment、troubleshooting、CHANGELOG |
| 线协议或 ALPN | ADR、`protocol.md`、architecture、security、compatibility、协议测试、CHANGELOG |
| 身份、PKI、授权或撤销 | ADR、security、architecture、protocol、deployment、恢复与恶意输入门禁 |
| 持久状态或地址分配 | ADR、architecture、configuration、deployment、troubleshooting、故障注入门禁 |
| Relay/P2P 数据路径 | ADR、architecture、protocol、security、E2E 和无重复包证据 |
| 平台或 QUIC runtime | compatibility、releasing、CI、部署说明和真实平台门禁 |
| 版本或阶段顺序 | design overview、roadmap、详细计划、compatibility、CHANGELOG |
| 发布支持声明 | compatibility、releasing、README、deployment、Release Notes 和证据记录 |

不适用的文档应在 PR 中说明原因，不能静默遗漏。

## Pull Request 要求

每个行为改造 PR 应包含：

- 问题与范围；
- PR purpose：`Design-only`、`Implementation` 或 `Evidence-only`；
- Proposed 设计/ADR 链接和本 PR 对应阶段；
- 当前规范中受影响的章节；
- 破坏性、安全、持久化和运维影响；
- 已运行的精确命令与场景；
- 未运行或仍失败的门禁；
- 文档适用性、设计评审、ADR 决策、交付、验证和发布状态各自发生的变化。

评审时先判断文档契约是否完整，再判断代码是否满足契约。代码与文档冲突时，不以代码
“已经能跑”为由跳过决策；应先确认是实现错误还是设计需要重新评审。

CI 会检查 PR 模板的变更级别和 purpose。D0 强制为 Markdown-only；D0/D1 均不得修改
Proposed 设计或 ADR 记录。Design-only 基于 PR base 到 HEAD 的实际 diff，
只允许声明的设计/ADR 和四个导航索引，并拒绝把 base 中的 Current/Approved 文档降级
伪装成新提案。Implementation 检查实际设计文件、完整批准 commit、Approved 状态、
冻结正文和原子 Gate ID；D2 必须给出不触发 ADR 的具体理由，D3-D4 必须链接在批准时和
当前都为 Accepted、且正文未被改写的 ADR。Evidence-only 校验证据目标、固定 Gate 表、
追加不可变性、ADR 集合、允许路径和生命周期单调升级；所有其他 purpose 都不能新增、
改写或删除证据记录。D4 Implementation 只能引用 PR base 中已合并的不可变证据；只有
实际 diff 严格限于预留 allowlisted marker 内容及可选的三份根目录附属投影，且至少
一个权威 marker 实际改变时，才强制引用同一目标、覆盖冻结设计全部 Gate、结果为最新
Passed 且无 waiver 的证据。该检查会在 PR 正文编辑后重新执行，但仍不能判断贡献者是否
错误地把 D3 写成 D1/D2，最终分类、语义和链接内容仍由维护者评审。
仓库分支保护应把此 CI job 设为必需检查；远端保护规则不由本仓库文件自动创建。

四个导航索引的自动检查只证明“路径在 allowlist 中”，不能判断索引文字是否偷换 Current
能力、支持或发布语义；维护者必须人工逐行评审这些 diff，并以权威 Current 文档为准。

引入本校验器的首次 PR 是唯一自举边界：只有 PR base 已经包含
`scripts/check-pr-design-contract.rb` 时才执行正文契约校验；CI 会先从 PR base 导出并
执行受保护的校验器和解析依赖，再执行 HEAD 版本，PR 不能通过同时削弱校验器与测试来
取消已有规则。base 尚无脚本时仍必须运行校验器回归测试、文档检查和其余 CI。首次合并
进入受保护的 `main` 后，后续 PR base 都包含脚本，该条件自动永久失效；校验器本身不
提供 waiver。

## 校对与检查

提交前至少执行：

```bash
cargo fmt --all -- --check
ruby scripts/test-pr-design-contract.rb
ruby scripts/test-release-evidence.rb
ruby scripts/test-release-workflow.rb
bash scripts/check-docs.sh
git diff --check
```

CI 与本地共用 `scripts/check-docs.sh` 检查第一方 Markdown 本地链接、尾随空白、旧项目
名称和六维状态枚举；rustdoc 由单独的 Cargo 命令构建。人工校对还必须确认：

- 所有 Current/Proposed/Conditional 表述明确；
- 版本、ALPN、schema、CLI 和示例与对应权威来源一致；
- “已支持”“已验证”“生产可用”都有证据；
- 数量门禁写明对象，例如目录记录、持久 lease 或在线 Agent；
- 计划阶段和关键门禁使用稳定 ID，证据记录引用同一 ID；
- 安全边界同时说明保护内容和明确不保护内容；
- 没有把未来方案写入当前配置或部署步骤；
- 新增文档已经加入 [`README.md`](README.md) 或相关索引。

发布前还必须执行 [`releasing.md`](releasing.md) 的完整检查和真实网络门禁。
