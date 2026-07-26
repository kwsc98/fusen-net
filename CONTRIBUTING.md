# 贡献指南

感谢你参与 Stellaris。提交代码前请阅读
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) 和本指南。安全漏洞不要提交公开
Issue，请使用 [`SECURITY.md`](SECURITY.md) 中的私密渠道。

行为改造必须遵循 [`docs/documentation-workflow.md`](docs/documentation-workflow.md)：
先冻结 Proposed 方案、ADR 和验收门禁，再实现，最后根据可审计证据升级文档状态。

## 开发环境

- Rust 1.97.0 或更高版本；优先使用 `rust-toolchain.toml` 指定的工具链。
- Linux Agent 测试需要 `/dev/net/tun`、`iproute2`，以及 `CAP_NET_ADMIN` 或 root。
- macOS/Windows 的真实 TUN 测试需要管理员权限；普通单元测试不应要求提权。

```bash
git clone https://github.com/kwsc98/Stellaris.git stellaris
cd stellaris
cargo build --workspace --all-features --locked
```

## 提交变更

1. 先为行为变化创建 Issue，并从 [`docs/README.md`](docs/README.md) 找到受影响的权威文档；
   小型 D0 文档修复可直接提交 Pull Request。
2. D1 修复引用既有 Current 契约并增加回归测试；D2-D4 先创建或更新 Proposed 设计，
   写清当前证据、目标、非目标、原子 Gate ID 和退出门禁。D2 必须说明为何不触发 ADR；
   命中 [`documentation-workflow.md`](docs/documentation-workflow.md#变更分级) 的权威
   触发规则时改分为 D3。Draft/Proposed/Rejected 等纯设计记录使用 `Design-only` PR，
   不得同时修改代码、配置或 Current 契约。
3. Proposed 设计记录 reviewer/批准 commit 并变为 Approved、所需 ADR 变为 Accepted
   后，再用单独的 `Implementation` PR 实现对应阶段；缺任一前置条件的 D2-D4
   Implementation 不得合并。合并 Design-only PR 本身不表示批准设计或接受 ADR。
4. 同步当前规范、示例、部署/排障文档和 `CHANGELOG.md` 的 `Unreleased` 部分。纯错字、
   断链等 D0 修正可以不写 changelog，但 PR 必须说明没有用户可见行为变化。
5. 在本地运行与 CI 等价的检查，并列出未执行或仍失败的门禁。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
ruby scripts/test-pr-design-contract.rb
ruby scripts/test-release-evidence.rb
ruby scripts/test-release-workflow.rb
bash scripts/check-docs.sh
git diff --check
```

请勿通过 `#[allow]`、跳过测试或吞掉错误来绕开失败。需要特权的真实 TUN E2E
应标记并交由专用 runner 执行；PR 中说明未在本机执行的测试。

## 代码要求

- 网络输入必须有长度和类型校验，不能使用 `panic!`、`todo!`、`unwrap` 或
  `expect` 处理不可信输入。
- 不记录 token、私钥、完整控制帧或用户包内容。
- 队列必须有界，并明确背压或丢弃策略。
- 新增源码文件使用 SPDX 标识：

  ```text
  // SPDX-License-Identifier: Apache-2.0 OR MIT
  ```

- 新依赖需说明用途，并通过漏洞、来源和许可证检查。

## Pull Request

PR 描述应明确 `Design-only`、`Implementation` 或 `Evidence-only` purpose，并包含问题、
方案文档/ADR、对应阶段或 Gate ID、受影响的当前规范、破坏性/安全/持久化影响、精确
验证结果和未完成门禁。涉及用户行为时提供可复现步骤；涉及
资源清理时说明异常退出和重连场景。维护者先评审文档契约，再评审实现是否满足契约。
维护者可能要求拆分无关改动。

本项目不要求 CLA 或 DCO，也不要求 `Signed-off-by`。除非明确标记为
“Not a Contribution”，提交到本仓库的贡献默认按 `Apache-2.0 OR MIT` 双许可
提供。你必须有权按这些条款提交所有内容。
