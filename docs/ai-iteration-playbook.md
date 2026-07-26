# Stellaris 单人 + AI 迭代手册

> **文档适用性：Current；适用范围：开发协作流程；设计评审状态：N/A；ADR 决策状态：
> N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**

本手册规定项目维护者独立使用 AI 编码 Agent 时，一次迭代应如何重建上下文、判断工作
是否合法、控制变更范围、验证结果并提交。它适用于代码、测试、配置、文档、设计和证据
任务，但不定义任何产品行为。

文档状态、权威边界和冲突处理以[文档中心](README.md)为准；D0-D4、PR purpose、设计
审批和证据规则以[文档驱动改造流程](documentation-workflow.md)为准。本手册只规定 AI
协作的执行纪律，不能替代 Proposed 设计、ADR、Current 契约、Gate 定义或
[`verification/`](verification/README.md) 中的执行证据。

本手册只约束 AI 的本地任务执行与本地 commit，不改变
[`GOVERNANCE.md`](../GOVERNANCE.md) 和 [`CONTRIBUTING.md`](../CONTRIBUTING.md) 规定的
维护者角色、公开评论、PR 评审、合并或发布流程，也不向 AI 授予任何决策或远端权限。

本手册不保存“当前进度”副本。分支、HEAD、工作区、生命周期状态、权威设计、ADR 和
Gate 必须在每次迭代开始时从 Git 与权威文档重新推导，不能沿用旧会话的口头结论。

## 每次迭代的固定流程

### 1. 完整读取上下文

AI 开始任务时，必须先完整读取以下内容；读取这些文件和检查 Git 状态所需的只读命令
属于允许的启动动作：

1. 根目录 [`AGENTS.md`](../AGENTS.md)；
2. 本手册；
3. [文档中心](README.md)；
4. [文档驱动改造流程](documentation-workflow.md)；
5. 本次目标对应的 Current 契约、Proposed 设计和设计列出的全部 ADR；
6. 涉及 Gate 时的权威 Gate 定义和[验证记录规则](verification/README.md)。

若任务不关联 Proposed 设计、ADR 或 Gate，AI 必须说明 `N/A` 及理由，不能虚构关联项。
通用 AI 客户端可能不会自动发现 `AGENTS.md`，因此应使用本手册后面的完整提示词启动
会话；识别仓库指令的 Agent 可以把“按手册继续”作为日常提示词的简写。

### 2. 从仓库重建状态

至少检查当前分支、完整 HEAD SHA、工作区、暂存区和未跟踪文件，并从文档顶部元数据
确认适用性、设计评审、ADR 决策、交付、验证和发布六个独立状态。只读取当前分支及
本次任务所需的历史；除非用户在本次请求中明确要求，不得查看、切换、比较、合并或从
`v6` 分支取文件。

所有迭代开始时，先向用户报告：

```text
分支：<branch>
HEAD：<full 40-character SHA>
工作区：<unstaged clean，或逐项列出现有未暂存变更及保护方式>
暂存区：<staged clean，或逐项列出现有暂存内容及保护方式>
未跟踪文件：<None，或逐项列出并标明保护方式>
生命周期：<Current/Proposed 文档的六维状态；不适用项写 N/A>
权威来源：<Current 契约、目标设计和 ADR 路径>
本次工作：<目标；D0-D4；PR purpose；Gate ID 或 N/A + 理由>
合法下一步：<一个小型切片，或只能进行的审计/Design-only 工作>
```

报告必须来自本次检查，不能照抄手册、历史交接或旧对话中的状态。

### 3. 冻结本次任务契约

开始编辑前明确以下内容：

- 一个可观察、可验收的目标；
- D0-D4 分类及分类依据；
- PR purpose，只能是 `Design-only`、`Implementation` 或 `Evidence-only`；
- 关联设计、完整批准 commit、全部所需 ADR 及其当前状态；
- 一个原子 Gate ID；D0/D1 没有适用 Gate 时写 `N/A: <具体理由>`；只有权威 Evidence-only
  或 D4 状态投影规则要求完整冻结集合时，才可以声明精确的多 Gate 集合；
- 本次允许修改的文件，以及文档影响矩阵中需要检查的文件；
- [`CONTRIBUTING.md`](../CONTRIBUTING.md) 中适用的构建、lint、测试、rustdoc，六项固定仓库
  检查，以及明确不在本地执行的 Gate。

D0/D1 的 PR purpose 按仓库规则使用 `Implementation`。其中 D0 可以注明
`Documentation-only` 作为变更范围说明，但它不是第四种 PR purpose。分类或影响范围在
实现中发生变化时，必须先暂停并重新判断，不能沿用较低等级继续提交。

### 4. 只选择一个小型切片

一次迭代只处理一个约 1-3 人日复杂度、可编译、可测试、可回滚的小型切片，并映射到
一个 Gate。同一个 Gate 可以由多次迭代逐步完成，但每次都必须保持仓库处于可继续开发的
一致状态，不得留下不可运行 stub、两套公开模型或临时兼容入口。唯一例外是权威
Evidence-only 或 D4 状态投影规则要求精确声明完整冻结 Gate 集合；该例外不允许混入多个
实现切片。

切片必须遵守以下边界：

- 只实现本次目标需要的最小范围，不顺带重构或处理其他 Gate；
- 行为变化同步增加测试，并按文档影响矩阵同步权威文档、示例和 CHANGELOG；
- 不为了减少当次工作量而添加 v1/v2 兼容、迁移、双 listener、协议降级或隐藏旧输入；
- 不查看或切换 `v6`，也不从该分支复制实现；
- 特权、长时间或需要专用环境的 Gate 可以留待后续执行，但必须报告为未执行，不能声称
  Passed，也不能创建通过证据。

### 5. 先通过合法性门槛

- **D0：** 只做权威流程定义的错字、断链或不改变既有含义的澄清；Markdown-only 或
  “流程文档”标签本身不构成 D0 分类依据。
- **D1：** 只按既有 Current 契约修复行为，并具备问题证据和回归测试。
- **D2-D4 Design-only：** 只推进允许范围内的设计、ADR 或导航索引，不修改运行代码、
  Current 产品契约、配置、示例、能力声明或交付/验证/发布状态。
- **D2-D4 Implementation：** 目标设计必须 Approved，记录已进入当前历史的完整批准
  commit，全部关联 ADR 必须 Accepted，目标 Gate 必须已冻结。缺少任一条件时，只能审计
  或准备合法的 Design-only 变更。
- **Evidence-only：** 变更分类始终为 D4，只针对已经是 PR base 祖先的精确 target
  commit 执行并追加真实证据；不得同时修实现或改冻结契约。

不得用 waiver、模糊的 `N/A`、用户说过“继续”或 AI 自己判断“方案没问题”来绕过这些
前置条件。

### 6. 保护现有工作区

迭代开始前已经存在的修改一律视为用户所有，除非当前会话能够证明它们由本次迭代
产生。AI 必须：

- 先列出已修改、已暂存和未跟踪文件，再确定本次允许路径；
- 不覆盖、删除、还原、stash 或顺带格式化用户已有改动；
- 不把用户已有改动加入本次暂存区或 commit；
- 文件重叠时先判断能否逐块隔离；不能可靠隔离时停止编辑或保留为未提交状态并报告；
- 提交前只显式暂存本次文件，并逐行复核 staged diff 与 staged 文件列表；
- 确认本次新增且被其他文件引用的未跟踪文件已经进入 staged 集合；
- 工作区仍有未纳入提交的用户改动时，在不包含这些改动的隔离环境中验证精确的
  “HEAD + 本次迭代选择”候选提交树；必要时使用临时 checkout 或 alternate index，不能
  保持用户工作区和暂存区原样时不自动提交。

不得使用 `git reset --hard`、`git clean`、`git checkout --`、`git restore`、强制切换或其他
可能丢失工作的命令。用户明确授权某项 Git 操作时，也只对经过只读确认的精确目标执行。

### 7. 实现、验证和复核

实现期间同步完成适用测试与文档，不做无关清理。准备提交时按以下顺序执行：

1. 按 [`CONTRIBUTING.md`](../CONTRIBUTING.md) 运行本切片适用的构建、Clippy、单元、
   集成、属性、模糊、场景和 rustdoc 检查；
2. 另外始终运行仓库规定的六项固定检查：

   ```bash
   cargo fmt --all -- --check
   ruby scripts/test-pr-design-contract.rb
   ruby scripts/test-release-evidence.rb
   ruby scripts/test-release-workflow.rb
   bash scripts/check-docs.sh
   git diff --check
   ```

3. 复核工作区 diff、staged diff、文件范围、生成物和文档影响矩阵；
4. 检查是否包含凭据、token、私钥、证书私钥材料、包内容、个人信息或其他秘密；
5. 确认没有把未执行、跳过、ignored、失败、Partial 或带 waiver 的 Gate 写成 Passed。

检查结果必须对应精确待提交树。含范围外改动的组合工作区通过检查，不足以证明 staged
内容可以独立提交；必须在隔离的临时 checkout/index 上重跑适用检查。若暂存后工作区除
staged 内容外完全干净，可以通过 staged 文件集合、完整 staged diff 和 staged
whitespace 检查证明两者一致。

缺少所需工具或环境时可以保留变更并报告，但缺失的必跑本地检查视为未通过，禁止自动
提交。特权或长时间 Gate 不属于每个小切片的本地自动提交前置条件时，可以明确留作
未运行 Gate；它们仍不能推进验证状态或支持声明。

Evidence-only 必须区分“被测 Gate 的结果”和“证据记录本身的完整性检查”。目标 Gate
可以真实得到 Failed、Partial、跳过或未执行结果；需要归档该次执行时，按
[`verification/README.md`](verification/README.md) 写成准确的 Failed/Partial 记录并保留
失败历史，绝不能写成 Passed。只有证据格式、目标 commit 约束和本节六项仓库检查通过后，
这样的失败事实记录才可以提交，也不得借此提升验证或支持状态。

### 8. 自动提交边界

只有同时满足以下条件时，AI 才自动创建 Conventional Commit：

- 任务范围、分类、purpose 和合法前置条件明确；
- `CONTRIBUTING.md` 中适用的构建、lint、测试、rustdoc，六项固定仓库检查和本次变更的
  完整性测试全部成功；非 Evidence-only 工作的适用行为测试也全部成功；
- Evidence-only 若归档 Failed/Partial Gate，记录与实际结果一致，且没有把失败当作
  Passed 或推进验证状态；
- staged 内容只包含本次迭代，且已复核完整 diff；
- 精确待提交树已独立通过所需检查，没有依赖范围外工作区改动；
- 没有秘密信息、未解决失败、不可隔离的用户改动或意外生成物；
- commit message 准确描述该切片，不冒充已验证或已发布。

仓库检查、记录完整性检查或非 Evidence-only 的适用行为测试失败，以及范围漂移或工作区
无法隔离时，均不得提交；应保留可审查状态，说明失败和修复建议。不得通过修改测试、
降低检查强度或跳过命令来换取提交。Evidence-only 中如实归档的 Failed/Partial Gate
结果不是这里所说的提交检查失败。

自动化到本地 commit 为止。AI 不得自动执行 push、merge、tag、release、amend、rebase、
force push 或其他历史改写，也不得创建远端 PR、发布制品或改变远端仓库状态，除非用户
针对该动作另行明确授权。即使获得授权，Accepted/Approved 和 Evidence-only 的独立规则
仍然有效。

### 9. 人工决策不可推断

项目维护者是 D2-D4 的最终 reviewer 和批准人。ADR `Accepted` 与设计 `Approved` 都
必须来自用户对具体路径或冻结 SHA 的明确授权；以下表达均不构成批准：

- “继续”、“下一步”、“按手册继续”；
- “实现这个计划”或要求处理某个 Gate；
- AI 的审计结论、测试通过或 Design-only 文档已经合入；
- 对别的 ADR、设计版本、分支或 commit 的批准。

AI 可以准备 Proposed/Draft 内容、审计接受条件并指出合法下一步，但不能代表用户接受
ADR 或批准设计。用户可以使用本手册的专用提示词，也可以用同样明确的措辞授权；接受
ADR 必须指定 ADR 路径，批准设计必须指定 reviewer 和完整 40 位冻结 commit，授权范围
只覆盖明确列出的对象。

### 10. 最终交接

每次迭代结束时报告：

```text
结果：<完成、未提交或只读审计>
Commit：<full SHA，未提交写 N/A>
变更：<文件与行为/文档影响>
检查：<逐项列出命令和结果>
未运行 Gate：<Gate ID、原因和所需环境；没有则写 None>
残余风险：<已知限制；没有则写 None>
下一项合法工作：<同一 Gate 的下一个切片，或下一个可进入的 Gate>
远端动作：None
```

报告不得把本地测试说成目标 commit 的归档证据。若自动提交成功，提交后的工作区状态也
必须再次检查；若仍有用户改动，逐项说明它们未被包含。

## 可直接使用的提示词

### 日常继续

```text
按仓库 AI 迭代手册执行下一项合法的小型工作切片。先完整读取 AGENTS.md 和
docs/ai-iteration-playbook.md，并从 Git 与权威文档重建当前状态。每次只推进一个
可编译、可测试、可回滚的切片，并映射到一个 Gate。遵守 D0-D4 以及
Design-only / Implementation / Evidence-only 边界。完成适用测试和仓库必跑检查后
自动提交，但不得 push、merge、tag 或 release。不要修改或提交本次迭代之外的现有改动。
```

### 指定 Gate

```text
按 AI 迭代手册处理 Gate <GATE_ID> 的下一个最小切片。
本次目标：<EXPECTED_BEHAVIOR>。
先验证该 Gate 的设计、ADR 和批准前置条件；前置条件不足时只报告并准备合法的
Design-only 工作。不要顺带处理其他 Gate。检查通过后自动提交并报告下一切片。
```

### 只读审计

```text
完整读取 AI 迭代手册和当前权威契约，只读审计 <SCOPE>。不要编辑或提交。
先列出按严重度排序的缺陷、文件行号、失败场景和缺失测试；无问题时明确说明残余风险。
```

### 接受 ADR

```text
我以项目维护者身份明确决定接受 <ADR_PATH>。请先验证其接受条件、关联设计和取代关系，
然后只通过合法的 Design-only 变更推进 Accepted 状态。不得修改运行代码或提前批准设计。
检查通过后自动提交，不得 push。
```

### 批准设计

```text
我以 reviewer <NAME> 身份明确批准目标分支中的冻结设计 commit <FULL_40_CHAR_SHA>。
请验证该 SHA 已进入当前历史、全部关联 ADR 已 Accepted、正文和 Gate 未漂移，然后只通过
Design-only 元数据变更记录 Approved 和批准 commit。检查通过后自动提交，不得 push。
```

若目标 commit 中有多份候选设计，必须先让用户另行明确设计路径，不能根据 SHA 猜测批准
对象。记录 Approved 前，还必须按权威流程验证关联 ADR 在批准快照、PR base 和当前 HEAD
均为 Accepted。

### 执行证据

```text
按 AI 迭代手册对目标 commit <FULL_40_CHAR_SHA> 执行 Gate <GATE_ID>。
只记录真实执行结果；失败、跳过、环境缺失或带 waiver 的结果不得写成 Passed。
不要修改实现或冻结契约，证据必须遵守 Evidence-only 和不可变记录规则。
```

该提示词始终按 D4 `Evidence-only` 处理。创建记录前必须验证 target commit 已经是 PR
base 的祖先；如果 Gate 实际为 Failed、Partial、跳过或未执行，按权威格式保留真实结果，
不得推进验证状态。

## 手册验收场景

手册或 `AGENTS.md` 发生变化后，至少用全新会话人工验证以下场景：

1. 只输入“按手册继续”时，识别仓库指令的 AI 在编辑前报告当前分支、HEAD、未暂存、
   暂存、未跟踪文件、六维生命周期、权威设计/ADR、目标 Gate 和合法下一步；通用客户端
   使用“日常继续”完整提示词得到相同结果。
2. 目标设计仍为 Draft/Proposed 或所需 ADR 未 Accepted 时，要求 runtime 实现会被拒绝，
   Agent 只报告或准备合法 Design-only 工作。
3. 预先创建与任务无关的脏工作区后，Agent 不覆盖、不暂存、不提交这些改动；无法隔离
   重叠文件时不提交。
4. 任一必跑检查失败时不创建 commit；全部检查成功时只提交本次文件，提交信息符合
   Conventional Commits，且没有 push。
5. 用户未明确使用“接受 ADR”或“批准设计”授权时，Agent 不推进 ADR `Accepted` 或设计
   `Approved`，普通“继续”和测试成功都不被解释为批准。
6. Evidence-only 的目标 Gate 得到 Failed/Partial 时，Agent 可以在记录完整性检查和六项
   仓库检查通过后提交准确的失败事实，但不会写成 Passed 或提升验证状态。
7. 普通实现仍只映射一个 Gate；只有 Evidence-only 或 D4 状态投影被权威流程要求时，
   Agent 才声明精确的完整冻结 Gate 集合，且不混入多个实现切片。
8. 批准 SHA 包含多份设计、设计路径不明确，或关联 ADR 未在批准快照、PR base、HEAD
   三处保持 Accepted 时，Agent 拒绝记录 Approved 并报告具体失败条件。
9. 范围外用户改动使组合工作区通过、但精确 staged 树单独失败时，Agent 不提交，也不把
   用户改动混入 staged 集合来换取通过。

验收会话的观察结果只验证协作流程是否按预期工作，不是产品 Gate 证据，不得写入
`docs/verification/`。
