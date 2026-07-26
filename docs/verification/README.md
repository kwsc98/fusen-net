# 验证证据记录

> **文档适用性：Current；适用范围：门禁证据格式；设计评审状态：N/A；ADR 决策状态：
> N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**

<!-- stellaris-release-status:verification-status:start -->
本目录保存 Stellaris 发布门禁的可审计结果。当前仓库还没有任何绑定目标 commit 的
执行记录，所以 v2 的验证状态为 Unverified，Proposed v3 同样是 Unverified。
<!-- stellaris-release-status:verification-status:end -->

不能仅根据计划中的勾选框、测试文件或 CI workflow 声称能力已验证。

## 记录规则

- 每份记录绑定一个不可变 commit SHA；工作区结果不能作为发布证据。
- 文件名使用 `YYYY-MM-DD-<version-or-scope>-<short-sha>.md`。
- 记录只能由 `Evidence-only` PR 新增；一经合并，任何 PR purpose 都不得修改、rename
  或删除。失败结果通过后续新记录保留历史，不能回写旧记录。
- 一个记录可以覆盖多个 Gate ID，但每个 Gate ID 必须分别写预期、实际结果和 artifact。
- 失败、跳过和未执行项原样保留，不能从记录中删除后宣称整体通过。
- 验证状态按适用目标 commit 的 latest result 计算：没有 Gate 满足
  `Passed + 豁免 None` 时仍为 Unverified；部分 Gate 满足时为 Partially verified；全部
  规定 Gate 满足时才是 Verified。仅有 Failed、Partial 或带 waiver 的 Passed 证据不会
  把状态提升为 Partially verified。
- waiver 只能说明已接受的局限和后续责任；存在 waiver、Failed 或 Partial 的门禁不能
  把总体状态提升为 Verified、稳定支持或生产可用。
- 日志、配置和 artifact 必须脱敏，不得提交 token、私钥、CSR、完整证书、用户包或
  未公开的真实网络地址。
- CI run、artifact 和镜像使用不可变 URL、digest 或校验值；临时链接只能作为辅助信息。

## 模板

```markdown
# <版本或范围> 验证记录

- 文档适用性：Current
- 适用范围：<版本或 Gate 范围>
- 设计评审状态：N/A
- ADR 决策状态：N/A
- 交付状态：N/A
- 验证状态：N/A
- 发布状态：N/A
- 证据目标：<full lowercase 40-character commit SHA>
- 总体结果：Passed / Failed / Partial
- 豁免：None

- 执行时间：YYYY-MM-DD HH:MM TZ
- 执行环境：<OS / architecture / kernel / privileges / topology>
- 执行人或 CI：<actor / run URL>

| Gate ID | 命令或场景 | 预期 | 实际 | 结果 | Artifact / hash |
| --- | --- | --- | --- | --- | --- |
| `V3-P4-RELAY-01` | `<exact command>` | 两 Agent 双向 IPv4 | <observed> | Passed | sha256:<64 lowercase hex> |

## 资源与持续时间

- 节点数：
- 持续时间：
- RSS / tasks / threads / file descriptors / queues：

## 未完成项和 waiver

- <none, or explicit limitation with owner and follow-up>

## 评审

- Reviewer：
- Reviewed at：
```

模板中的 Gate ID 单元格必须写成反引号包裹的原子 ID，例如
`` `V3-P4-RELAY-01` ``。固定六列表头、非空命令/预期/实际/artifact、逐行结果和
`总体结果` 由文档检查器解析；每个 artifact 单元格必须包含不可变内容摘要
`sha256:<64 位小写十六进制>`，可以同时附带不可变 URL。`总体结果` 在任一 Gate Failed
时为 Failed，否则任一 Gate
Partial 时为 Partial，只有全部 Gate Passed 时才为 Passed。文件名末尾 short SHA
必须是 `证据目标` 的前缀。
证据记录不得使用 HTML、HTML comment 或 fenced code block 隐藏 metadata 或 Gate 表；
精确命令写在表格单元格或链接的不可变 artifact 中。

`豁免` 无例外时固定写 `None`；其他非空值表示该记录存在 waiver，并必须具体说明关联
Issue、范围和责任人。存在 waiver 的记录即使行为结果为 Passed，也不能支撑 Verified 或
Stable。对同一 target 和 Gate 的多次执行不删除旧记录：按记录首次进入目标分支
first-parent 历史的顺序，以最新记录为准；同一 Evidence-only PR 不得重复同一 Gate ID。
因此后续 Failed、Partial 或带 waiver 的结果会覆盖旧 Passed，并要求状态同步降级。
Stable checker 还会从 first-parent 历史恢复曾新增的全部记录，逐字节比对首次加入版本；
删除、改写、rename、重复加入或文件名 short SHA 与证据目标不一致都会失败关闭，不能靠
移除较新的失败记录让旧 Passed 重新生效。

计划文档负责定义 Gate ID 和退出条件，本目录只保存执行结果，不重新定义门禁。支持
等级仍由 [`../compatibility.md`](../compatibility.md) 裁决，发布流程见
[`../releasing.md`](../releasing.md)。
