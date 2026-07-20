# 贡献指南

感谢你参与 Fusen Net。提交代码前请阅读
[`CODE_OF_CONDUCT.md`](CODE_OF_CONDUCT.md) 和本指南。安全漏洞不要提交公开
Issue，请使用 [`SECURITY.md`](SECURITY.md) 中的私密渠道。

## 开发环境

- Rust 1.97.0 或更高版本；优先使用 `rust-toolchain.toml` 指定的工具链。
- Linux Agent 测试需要 `/dev/net/tun` 和 `CAP_NET_ADMIN` 或 root。
- macOS/Windows 的真实 TUN 测试需要管理员权限；普通单元测试不应要求提权。

```bash
git clone https://github.com/kwsc98/fusen-net.git
cd fusen-net
cargo build --workspace --all-features --locked
```

## 提交变更

1. 先为行为变化创建 Issue；小型文档修复可直接提交 Pull Request。
2. 将变更保持在一个清晰主题内，并为可观察行为增加测试。
3. 协议、安全边界或核心架构变化必须新增 ADR。
4. 更新用户文档和 `CHANGELOG.md` 的 `Unreleased` 部分。
5. 在本地运行与 CI 等价的检查。

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
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

PR 描述应包含问题、方案、兼容性/安全影响和验证结果。涉及用户行为时提供可复现
步骤；涉及资源清理时说明异常退出和重连场景。维护者可能要求拆分无关改动。

本项目不要求 CLA 或 DCO，也不要求 `Signed-off-by`。除非明确标记为
“Not a Contribution”，提交到本仓库的贡献默认按 `Apache-2.0 OR MIT` 双许可
提供。你必须有权按这些条款提交所有内容。
