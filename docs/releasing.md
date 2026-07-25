# 发布流程

只有维护者可以发布。版本遵循 SemVer；`0.x` 可以包含破坏式协议、配置和状态变化。
核心 `stellaris` crate 在 API 稳定前保持 `publish = false`，发布物是 CLI 二进制、
Linux 容器和源代码。

当前 workspace 版本为 `0.3.0-alpha.1`。它是早期预览，不得在 Release Notes、镜像
说明或支持矩阵中标记为稳定生产版本。

## 发布前一致性

1. 从 `main` 的干净、已评审 commit 发布。
2. workspace 中所有 package 使用同一版本，tag 与 Cargo 版本完全一致。
3. `CHANGELOG.md` 记录破坏式切换、无迁移路径、安全边界和已知限制。
4. README、schema v2 示例、四条 ALPN、CLI 命令和支持矩阵与实现一致。
5. `Cargo.lock` 无意外变化；新增依赖有许可证和来源说明。
6. 搜索并拒绝旧命令、旧 schema/ALPN、backend 配置和历史项目名的当前能力声明。
7. 检查 Markdown 本地链接并运行 `git diff --check`。

文档必须区分“实现存在”和“门禁已通过”。仓库中有 ignored test、workflow 或脚本
不是执行成功的证据；只有目标 tag commit 对应的可审计 CI/runner 结果才算门禁证据。

## 无特权门禁

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets --all-features --locked -- -D warnings
cargo test --workspace --all-features --locked
RUSTDOCFLAGS="-D warnings" \
  cargo doc --workspace --all-features --no-deps --locked
cargo check -p stellaris --all-targets --locked \
  --no-default-features --features backend-quinn
cargo check -p stellaris --all-targets --locked \
  --no-default-features --features backend-s2n
cargo check -p stellaris --all-targets --locked \
  --no-default-features --features backend-gm-quic
cargo deny check
cargo audit
git diff --check
```

还要执行 vendored `qconnection`/`qunreliable` 的 fmt、Clippy 和测试，并在 Linux、
macOS、Windows 目标编译 CLI。s2n-quic 和 gm-quic feature 只证明保留的抽象可编译，
不能在 Release Notes 中描述成 v2 可选运行后端；v2 runtime 固定使用 Quinn。

协议/身份测试至少覆盖四 ALPN 隔离、严格 JSON/方向、enrollment 一次性消费、CSR
PoP、SPKI 轮换、续期、撤销、session/incarnation 重放、Relay Ready 和 P2P descriptor
验证。CI 会对严格解码器执行固定种子的 4096 输入 fuzz smoke；长时间、覆盖率引导的
持续 fuzz 仍是后续门禁，不能用 smoke 结果代替。

## Linux 特权门禁

首轮运行支持只以 Linux 为门禁。使用隔离、一次性 self-hosted runner，具备 root、
`/dev/net/tun`、`ip`、`ping`，完整场景还需要 `tc`：

```bash
sudo tests/e2e/run-real-tun.sh linux native
sudo tests/e2e/run-real-tun.sh linux all
```

`native` 当前只验证本机 TUN/route 及内核 ping/TCP/UDP 生命周期，不经过完整 Stellaris
网络。`all` 必须额外存在并通过以下 exact tests：

- `linux_v2_overlay_e2e`
- `linux_v2_server_restart`
- `linux_v2_agent_restart`
- `linux_v2_fault_injection`
- `linux_v2_soak`

当前这些完整测试尚未全部实现，脚本会 fail-closed。因此 `0.3.0-alpha.1` 没有完整
Linux 真实网络门禁通过声明。实现完成后，soak 必须至少运行 30 分钟并监控 RSS、
任务、线程、文件描述符、queue 和路径类型。

macOS/Windows 当前只要求编译成功，原生 TUN/P2P 未验证。未来若提升其支持等级，
必须新增各自真实双 Agent/多主机门禁，不能沿用 Linux 结果推断。

## 候选版验收

在描述 alpha 为“可运行预览”前，至少需要：

- enrollment -> control -> Relay -> TUN 双 Agent 双向 IPv4；
- Ready 前丢弃、源地址伪造、MTU、背压、重连和 session ABA；
- 两节点/多节点 LAN P2P、同时拨号仲裁和 Relay/P2P 切换；
- 包 ID 证明无主动重复包、无环路，P2P 失败包不补发；
- 证书续期替代、到期关闭、token/SPKI 轮换和撤销；
- Server/Agent 重启、持久化故障点和路由回滚；
- Linux `all` 与 30 分钟 soak 完整通过。

任一项缺少证据时必须在 Release Notes 顶部列为已知限制。预发布可以在普通 CI 通过
后发布用于开发评估，但不能暗示跳过的特权门禁成功。

## 标签与制品

创建与 Cargo 版本一致的 signed annotated tag：

```bash
git tag -s v0.3.0-alpha.1 -m "Stellaris 0.3.0-alpha.1"
git push origin v0.3.0-alpha.1
```

自动发布必须验证 tag 签名、确认 commit 可从 `origin/main` 到达，并从该 commit
重建。第三方 Actions 固定完整 commit SHA。预发布标记为 prerelease；不得移动 tag
或静默替换同一 tag 下的二进制。

目标制品包括 Linux/macOS/Windows x86_64 archive、`SHA256SUMS`、SPDX SBOM、第三方
许可证清单、build provenance，以及 Server/Agent Linux amd64 镜像。平台 archive
存在只表示构建产物，不表示原生运行支持。容器和自动化部署固定完整 tag 或 digest，
不使用 `latest`。

发布后下载制品验证 checksum、attestation 和 `stellaris --version`，再用发布制品
重复所有声明通过的 smoke。发现安全或完整性问题时停止推荐并发布新版本，不修改
既有不可变制品。
