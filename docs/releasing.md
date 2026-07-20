# 发布流程

只有维护者可以发布。版本遵循 SemVer，变更记录遵循 Keep a Changelog。核心
`fusen-net` crate 在 API 稳定前保持 `publish = false`；0.1 只发布 CLI 二进制、
Linux 容器和源代码。

## 发布前

1. 从 `main` 的干净 commit 发布，确认所有计划变更已经评审合并。
2. 将 workspace 版本更新为目标版本，所有 package 保持一致。
3. 将 `CHANGELOG.md` 的 `Unreleased` 内容移入带日期的版本标题，补充迁移、
   安全和已知限制。
4. 确认 README、配置示例、协议版本和支持矩阵与实现一致。
5. 确认 `Cargo.lock` 没有意外变化，新增依赖有许可证和来源说明。

本地运行无特权门禁：

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
cargo test --manifest-path vendor/qunreliable/Cargo.toml --locked
cargo test --manifest-path vendor/qconnection/Cargo.toml --locked
cargo clippy --manifest-path vendor/qunreliable/Cargo.toml --all-targets --locked -- -D warnings
cargo clippy --manifest-path vendor/qconnection/Cargo.toml --all-targets --locked -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --all-features --no-deps --locked
cargo deny check
cargo audit
```

随后等待托管 CI 的 Linux/macOS/Windows 编译、三个单后端 feature 构建和依赖检查
通过。无预发布后缀的稳定标签还必须通过三个一次性自托管 runner 上的真实 TUN
E2E：ping、TCP、UDP、重连和路由回滚；Linux 另需让 Quinn、s2n 和 gm-quic 分别
通过丢包、乱序、MTU 黑洞和 30 分钟 soak。

仓库提供 ignored 的 `crates/fusen-net/tests/real_tun.rs` 特权 harness 和显式场景脚本。
三平台可以在单机上验证真实 TUN 创建、内核 ping/TCP/UDP 和路由回滚；Linux 通过两个
network namespace 进一步验证完整 Relay/Edge、三后端、重连、故障注入和 soak。
macOS/Windows 的完整双 Edge 数据面仍需双 VM 或双 runner 控制器，且一次性 runner
证据尚未纳入自动发布工作流。因此当前仍只能发布 alpha/beta/rc；这些门禁和 runner
全部就绪前，不能以手工上传制品绕过稳定发布条件。

`gm-quic 0.4` 依赖仓库内 Apache-2.0 的 `qconnection` 和 `qunreliable` 最小 fork，
分别补齐 1-RTT Datagram 组包和收发各 256 项的队列。发布评审必须检查
`vendor/*/PATCHES.md`、第三方许可证、SBOM 和相对上游 0.4.0 的 diff。只有统一后端
契约、真实 TUN 背压及长时间资源测试通过后才能标为稳定支持；升级上游版本时不得在
没有等价测试证据的情况下移除或静默绕过本地 patch。本地 `qconnection` fork 已将
ACK、CRYPTO、stream 和连接控制帧分发限制为每类 256 项，并在可靠帧队列满时关闭
连接。稳定评审仍必须用资源监控下的恶意输入和 soak 证明不会持续增长资源。

发布评审不得把三个后端描述为统一的 256 项队列：s2n 和 vendored gm-quic 的
Datagram 收发队列分别为 256 项，而 Quinn 的收发缓冲分别是 128 KiB 字节预算，
其等效包数随 Datagram 长度变化。Relay 每个目标会话的应用层队列才固定为 256 项。

## 候选版验收

- 三后端连接、TLS/SNI/ALPN、控制流、Datagram、关闭、超时和错误契约全部通过。
- 背压测试分别覆盖应用层 256 项队列、s2n/gm-quic 的 256 项队列和 Quinn 的
  128 KiB 字节预算边界。
- 多 listener 3 x 3 源/目标后端路由组合全部通过。
- 100 次正常条件 ping 零丢包。
- Relay 重启后 Agent 在 30 秒内恢复。
- Agent 退出后 5 秒内清理程序创建的接口和路由。
- 长时间测试没有持续内存/任务/文件描述符增长。

任一目标平台或后端失败都阻塞稳定版。alpha/beta/rc 可以携带明确已知限制，但不能
在 Release Notes 中标为稳定支持，也不能把未通过的原生平台或后端列为稳定能力。

## 标签和自动发布

创建与 Cargo 版本完全一致的签名标签。工作流接受 `vX.Y.Z` 稳定标签，以及
`vX.Y.Z-alpha[.N]`、`vX.Y.Z-beta[.N]`、`vX.Y.Z-rc[.N]` 预发布标签：

```bash
git tag -s v0.1.0-rc.1 -m "Fusen Net 0.1.0-rc.1"
git push origin v0.1.0-rc.1

git tag -s v0.1.0 -m "Fusen Net 0.1.0"
git push origin v0.1.0
```

`v*` 标签触发 GitHub Release workflow。工作流通过 GitHub API 验证 annotated tag
的签名状态，并确认标签 commit 可从 `origin/main` 到达；随后从该 commit 重建，而不是
上传本地二进制。稳定标签在构建和发布制品前会直接调用三个真实 TUN 脚本，只有
Linux、macOS 和 Windows job 全部成功才继续。预发布标签不会占用自托管 runner，
可在普通门禁通过后继续，但 GitHub Release 会自动标为 prerelease，并在发布说明
顶部声明真实 TUN 门禁未执行及平台支持尚未稳定。

全部第三方 GitHub Actions 必须固定到完整 commit SHA，并在行尾保留可读版本注释；
只通过 Dependabot 或经过评审的维护 PR 更新，不直接使用 `main`、`master` 或浮动
major tag。标签验证后，所有后续 job checkout 已验证的 commit SHA，并在创建 Release
前再次确认签名标签仍指向同一 commit，避免可移动标签造成构建竞态。

通过门禁后，工作流生成：

- Linux x86_64、macOS x86_64 的 `.tar.gz` 和 Windows x86_64 的 `.zip`；
- `SHA256SUMS`；
- SPDX JSON SBOM 和第三方许可证清单；
- GitHub build provenance attestation；
- `ghcr.io/kwsc98/fusen-net/server:<tag>` 和
  `ghcr.io/kwsc98/fusen-net/agent:<tag>` Linux amd64 镜像及 digest、SBOM、
  provenance。

发布页应复制对应 changelog，说明协议/配置版本、支持矩阵、升级步骤和已知问题。
自动化部署固定完整版本或镜像 digest，不使用 `latest`。

## 发布后

1. 下载每种制品，验证 checksum、attestation 和 `fusen-net --version`。
2. 使用发布镜像完成一个 Relay + 两个 Fake/真实 Edge 的 smoke test。
3. 检查文档链接和容器命令，并在路线图中更新当前阶段。
4. 观察安全告警和用户回报；修复进入新的 patch 版本，不移动既有标签。

制品有安全或完整性问题时，立即停止推荐该版本、在 GitHub Release 添加醒目说明，
撤回可变容器引用并发布修复版本。不得静默替换同一标签下的二进制或镜像。
