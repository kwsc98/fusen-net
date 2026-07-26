# 发布流程

> **文档适用性：Current；适用范围：发布流程；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：N/A；验证状态：N/A；发布状态：N/A。** 发布证据状态按目标 commit 记录在
> [`verification/`](verification/README.md)。

只有维护者可以发布。版本遵循 SemVer；`0.x` 可以包含破坏式协议、配置和状态变化。
核心 `stellaris` crate 在 API 稳定前保持 `publish = false`，发布物是 CLI 二进制、
Linux 容器和源代码。

<!-- stellaris-release-status:release-status:start -->
当前 workspace 候选版本为 `0.3.0-alpha.1`。截至 2026-07-26，当前检出的仓库没有
release tag，因此相关变化仍属于 `CHANGELOG.md` 的 `Unreleased`。它是早期预览，不得
在 Release Notes、镜像说明或支持矩阵中标记为稳定生产版本。
<!-- stellaris-release-status:release-status:end -->

## 发布前一致性

1. 从 `main` 的干净、已评审 commit 发布。
2. workspace 中所有 package 使用同一版本，tag 与 Cargo 版本完全一致。
3. `CHANGELOG.md` 记录破坏式切换、无迁移路径、安全边界和已知限制。
4. README、schema v2 示例、四条 ALPN、CLI 命令和支持矩阵与实现一致。
5. `Cargo.lock` 无意外变化；新增依赖有许可证和来源说明。
6. 搜索并拒绝旧命令、旧 schema/ALPN、backend 配置和历史项目名的当前能力声明。
7. 检查 Markdown 本地链接并运行 `git diff --check`。
8. 按 [`documentation-workflow.md`](documentation-workflow.md) 核对 Current/Proposed、
   ADR 决策、交付和验证状态；将实际门禁结果写入 [`verification/`](verification/README.md)。
9. 稳定版本要求 Current 详细计划已经标记 `Verified + Stable`，且计划中的每个原子 Gate
   都能在验证记录中找到 `Passed` 结果和不可变 artifact/hash。

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
cargo check -p stellaris-cli --all-targets --locked \
  --no-default-features --features backend-quinn
cargo check -p stellaris --all-targets --locked \
  --no-default-features --features backend-s2n
cargo check -p stellaris-cli --all-targets --locked \
  --no-default-features --features backend-s2n
cargo check -p stellaris --all-targets --locked \
  --no-default-features --features backend-gm-quic
cargo check -p stellaris-cli --all-targets --locked \
  --no-default-features --features backend-gm-quic
cargo deny check --all-features
cargo audit
ruby scripts/test-pr-design-contract.rb
ruby scripts/test-release-evidence.rb
ruby scripts/test-release-workflow.rb
bash scripts/check-docs.sh
git diff --check
```

本地 `cargo-deny` 和 `cargo-audit` 必须使用与目标 release CI 固定 action 对应的已审核
版本；个人机器上的任意旧版本不能替代 release workflow 结果。

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

当前脚本硬性要求 effective UID 0。GitHub self-hosted runner 必须作为一次性 root runner
启动；workflow 不会自行提权。不要给持久组织 runner 配置可被仓库代码调用的宽泛
免密 sudo。

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
- host candidate 排除 TUN/overlay 地址，并用路由证据证明 P2P 使用真实 underlay；
- 包 ID 证明无主动重复包、无环路，P2P 失败包不补发；
- 证书续期替代、到期关闭、token/SPKI 轮换和撤销；
- Server/Agent 重启、持久化故障点和路由回滚；
- Linux `all` 与 30 分钟 soak 完整通过。

任一项缺少证据时必须在 Release Notes 顶部列为已知限制。预发布可以在普通 CI 通过
后发布用于开发评估，但不能暗示跳过的特权门禁成功。

稳定 tag 还会执行 `scripts/check-release-evidence.rb`。它要求所有
`Current + Implemented` 第一方规范都达到 `Verified + Stable`，并从当前
[`distributed-network-plan.md`](distributed-network-plan.md) 提取全部原子 Gate ID。
每项 Gate 必须有 `Passed` 证据，且证据目标是 release commit 的祖先。从证据目标到
release commit 之间不得发生非 Markdown 变化；`docs/` 下的规范还只能改变验证、发布、
最后核对元数据、新增不可变证据，或改变证据目标中已经存在的 allowlisted
`stellaris-release-status` 区块。marker 的固定 path/ID、格式和禁止内容见
[`documentation-workflow.md`](documentation-workflow.md#5-验证与状态升级)；Gate、marker
边界和其余契约正文不能变化。根 README、CHANGELOG 等 Markdown 可以在后续 D4 文档
投影 PR 中同步已经归档的状态。源码、配置、测试、脚本或 workflow 一旦变化，旧证据
立即失效。这样可以先测试一个冻结 commit，再用独立 Evidence-only PR 归档结果，而不
要求 commit 在自身内容中引用自己的 SHA。

稳定检查器会沿 release commit 的 first-parent 历史枚举所有曾新增的验证记录，而不是
只扫描当前目录。每份记录必须仍存在、与首次加入 commit 的字节完全一致，且规范文件名
末尾 short SHA 必须匹配 `证据目标` 前缀；删除、改写、rename 或重复加入都会阻断发布。
同一 Gate 以记录首次加入 first-parent 的先后顺序取最新结果，只有最新结果为 Passed 且
`豁免：None` 才满足 Stable，后续 Failed、Partial 或 waiver 不能被旧 Passed 掩盖。

## 标签与制品

创建与 Cargo 版本一致的 signed annotated tag：

```bash
git tag -s v0.3.0-alpha.1 -m "Stellaris 0.3.0-alpha.1"
git push origin v0.3.0-alpha.1
```

创建 tag 前，必须先把 `Unreleased` 内容移动到唯一的
`## [0.3.0-alpha.1] - YYYY-MM-DD` 小节；release workflow 会拒绝缺失或重复的小节。

自动发布必须验证 tag 签名、确认 commit 可从 `origin/main` 到达，并从该 commit
重建。第三方 Actions 固定完整 commit SHA。预发布标记为 prerelease；不得移动 tag
或静默替换同一 tag 下的二进制。

release workflow 按请求 tag 使用 `cancel-in-progress: false` 的 concurrency group；同一
tag 的第二次 push、手动 `workflow_dispatch` 或 re-run 必须排队。公开发布前，preflight
要求该 tag 的 GitHub Release 明确返回 404，且 GHCR 的 Server、Agent 两个同名 semantic
tag 都明确返回 404；200 表示已存在并立即拒绝，鉴权、网络、解析失败或其他 HTTP 状态
也一律失败关闭。preflight 的 `contents: write` 只用于让 GET 能看到 draft Release，包权限
保持 read-only；该 job 不执行写 API。创建 GitHub Release 前还会再次检查 404；随后只
调用一次 create API 并且只接受 HTTP 201。HTTP 422 或任何其他状态都会失败，不查询、
更新或复用既有 Release。workflow 先创建私有 draft，只向该次 201 响应返回的 release ID
上传 URL 编码后的 `dist/` 顶层制品，每个上传必须返回 201；资产名称和数量完全核对后才
发布该 draft。这样同 tag Release 的并发创建会失败或形成彼此隔离的 draft，不会覆写或
混合既有 Release 的资产。

跨 GitHub Releases 与 GHCR 的发布不是单一事务。workflow 会先完成全部平台 archive，
再推送 Server/Agent 镜像，最后创建 GitHub Release；后续步骤失败时仍可能留下部分 GHCR
tag。此时必须停止推荐该版本，记录失败和已发布 digest，按仓库策略隔离或删除不完整
镜像，并用新版本重新发布；不得移动原 tag 或覆写已经公开的制品。
任何一个目标已存在时都不得重跑同版本补齐其余目标；必须保留失败事实并使用新版本，
否则不同用户可能拿到同一语义版本的不同字节。

GitHub Release 的 create-only API 能在竞争发生时拒绝复用已有对象；当前 GHCR manifest
push 没有使用等价的 conditional-create 操作。tag concurrency 只串行化本 workflow 的
同 tag run，不能阻止拥有 package write 权限的外部 workflow 或人工客户端在 GHCR
preflight 与镜像 push 之间写入同名 tag。仓库和 package 权限必须把写入限制到受保护的
release workflow；如果保留其他 writer，这一竞态就是未关闭的发布完整性风险，不能把
preflight 描述为 registry 级不可变保证。

GitHub Release 已返回 201 后，任一资产上传、资产核对或最终发布失败都会保留一个不完整
draft 作为失败事实。不得删除后重跑同版本来拼接制品；应记录 draft ID 和已上传资产，
停止推荐该版本并使用新版本重新发布。

目标制品包括 Linux/macOS/Windows x86_64 archive、`SHA256SUMS`、SPDX SBOM、第三方
许可证清单、build provenance，以及 Server/Agent Linux amd64 镜像。平台 archive
存在只表示构建产物，不表示原生运行支持。容器和自动化部署固定完整 tag 或 digest，
不使用 `latest`。

发布后下载制品验证 checksum、attestation 和 `stellaris --version`，再用发布制品
重复所有声明通过的 smoke。发现安全或完整性问题时停止推荐并发布新版本，不修改
既有不可变制品。
