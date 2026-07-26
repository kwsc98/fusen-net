# 项目治理

## 原则

Stellaris 采用公开开发、维护者负责的轻量治理模式。技术讨论、决策依据和发布
状态应尽可能保留在公开的 Issue、Pull Request 和 ADR 中。安全漏洞和行为准则
事件除外，它们按各自的私密报告流程处理。

项目采用文档驱动改造。文档权威边界见 [`docs/README.md`](docs/README.md)，具体流程
见 [`docs/documentation-workflow.md`](docs/documentation-workflow.md)。D2-D4
`Implementation` 没有先获得设计批准、冻结公共行为/失败语义/原子 Gate ID，或缺少
流程要求的 Accepted ADR 时不得合并，也不得用 waiver 绕过。`Design-only` 可以先合并
Draft/Proposed/Rejected 等设计记录，但不得夹带代码、配置或 Current 契约变化。

## 角色

- **使用者**：使用项目并提供反馈的人。
- **贡献者**：向文档、代码、测试或社区提交贡献的人。
- **维护者**：拥有合并、发布、安全响应和仓库管理权限的人。

当前维护者为 [@kwsc98](https://github.com/kwsc98)。维护者可以邀请长期提供高
质量贡献、能够独立评审并遵守行为准则的贡献者加入维护团队。

## 决策方式

日常修复和小型改进通过 Pull Request 评审决定。维护者应给出可验证的技术理由，
并尽量解决实质性异议；无法形成共识时，由无利益冲突的维护者作最终决定。

ADR 的唯一触发规则由
[`docs/documentation-workflow.md`](docs/documentation-workflow.md#变更分级) 维护，本页
不复制另一份清单。合并 ADR 文件只表示保留一份决策记录；只有 `ADR 决策状态：
Accepted` 才表示决策已经接受，而且仍不表示实现已经完成。需要推翻既有 ADR 时应新增
一份替代 ADR，保留历史记录。

文档适用性、设计评审状态、ADR 决策状态、交付状态、验证状态和发布状态分别记录。
维护者不得因为代码已经接入就把 Proposed 改成 Current，或在缺少对应 commit 证据时
把 Implemented 写成 Verified。当前破坏式路线不承担旧协议和旧配置兼容成本；改变
该规则必须新增 ADR。

## 合并与发布

- 贡献者不能批准自己的安全敏感变更；具备条件时至少需要另一位维护者评审。
- CI 必须通过，例外情况必须在 PR 中记录原因、风险和后续工作。
- 行为改造 PR 必须声明 `Design-only`、`Implementation` 或 `Evidence-only`；
  Implementation 引用已批准设计、适用的 Accepted ADR 和原子 Gate ID，并同步受影响
  的权威文档；Evidence-only 只追加不可变证据并更新允许的生命周期元数据。
- `main` 的远端分支保护应要求 CI 和维护者评审；仓库内 workflow 不能自行保证该设置
  已在托管平台启用。
- 只有维护者可以创建版本标签、发布制品或发布容器镜像。
- 发布过程遵循 [`docs/releasing.md`](docs/releasing.md)，安全响应遵循
  [`SECURITY.md`](SECURITY.md)。

## 许可与知识产权

项目不要求签署 CLA 或 DCO。贡献者提交贡献即表示其有权提交该内容，并同意按
`Apache-2.0 OR MIT` 提供。不得提交许可证不兼容、来源不明或无法重新分发的代码、
证书、密钥及其他材料。

## 治理变更

治理规则通过普通 Pull Request 修改，但应给予社区至少 7 天公开评论期。紧急
安全响应可以先采取临时措施，随后补充公开说明。维护者长期无法履职时，活跃贡献者
可以在公开 Issue 中提出交接方案；仓库权限的最终转移仍需现有所有者或托管平台完成。
