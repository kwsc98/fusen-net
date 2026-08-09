# Stellaris

[English](README.en.md)

Stellaris 是一个基于 QUIC Datagram 和 TUN 的分布式 IPv4 overlay 网络。当前
`0.3.0-alpha.1` 采用单实例协调服务、可信 Relay 和按需局域网 P2P：Agent 首先
建立可用的 Relay 路径，收到目标流量后尝试 Quinn host-candidate 路径，并在 P2P
Ready 后按目标 overlay IP 切换。真实 underlay LAN 绕行仍需候选过滤和网络门禁证明。

> **项目状态：早期预览。** v2 运行时代码已经接入 CLI，但当前没有绑定目标 commit 的
> 门禁记录，验证状态为 Unverified；完整集成测试、Linux 真实 TUN、故障注入和资源
> soak 门禁尚未完成。不要把当前 alpha 用于关键生产流量，也不要根据本文推断某个平台
> 已经通过发布验证。

```text
                     +--------------------------------+
                     |  Coordinator + trusted Relay   |
                     | enroll :7000 / control :7001   |
                     | relay  :7002                    |
                     +---------------+----------------+
                                     |
                         control + Relay fallback
                          /                         \
                  +-------+-------+         +-------+-------+
                  |    Agent A    |=========|    Agent B    |
                  | TUN + P2P UDP |  Quinn  | TUN + P2P UDP |
                  +---------------+   LAN   +---------------+
```

## 能力边界

- 单租户、单信任域、单协调实例、IPv4-only、静态 overlay 地址，最多 256 个节点。
- 四条隔离链路：注册、控制、Relay 和 P2P；配置使用三个互不冲突的 Server UDP
  listener，每个 Agent 另有一个 P2P UDP bind。
- 首次注册使用部署 TLS、一次性 `stl2_` token 和 CSR proof-of-possession；之后控制、
  Relay 和 P2P 使用节点 CA 签发的短期 mTLS 证书。
- Agent 先通过可信 Relay 发送，再按需请求连接计划并尝试 host candidate P2P。
  P2P 失败、断开、过期或空闲回收后，后续包回退 Relay。
- v2 运行时仅使用 Quinn。s2n-quic 和 gm-quic 依赖及传输抽象仍可编译，但不能由
  v2 配置选择，也不属于当前运行门禁。
- 当前只实现局域网 host candidate。NAT 穿透、server-reflexive candidate、STUN、
  HA、ACL、多租户、IPv6、动态地址、DNS 和子网/默认路由均不在本阶段。

> **可信 Relay 边界：** 使用真实 underlay host candidate 时，P2P 包由节点间 QUIC
> mTLS 保护并可绕过 Server 数据面；Relay 回退包会在 Server 上解密，Server 可以看到
> 完整 overlay IPv4 包及流量元数据。本版本不提供 Relay 路径端到端加密。当前候选枚举
> 尚未证明排除 TUN/overlay 地址，因此 P2P 指标本身不能作为已经绕过 Relay 的证据。

平台状态以[兼容性说明](docs/compatibility.md)为准。Linux 是首轮正式运行门禁；
macOS 和 Windows 当前只要求编译通过，原生 TUN/P2P 行为尚未验证。

## 构建

需要 Rust 1.97.0 或更高版本：

```bash
cargo build --workspace --all-features --locked
cargo test --workspace --all-features --locked
cargo build --release -p stellaris-cli --no-default-features --features backend-quinn --locked
./target/release/stellaris --version
```

正式二进制由 `stellaris-cli` package 生成，名称为 `stellaris`。Server 不创建
TUN；Agent 在 Linux 上需要 `/dev/net/tun`、`iproute2` 提供的 `ip` 命令，以及 root
或 `CAP_NET_ADMIN`。

## 快速开始

示例位于 [`configs/`](configs/)。以下流程会创建全新的 v2 状态；旧配置、旧状态和
旧 Agent 不能继续使用。

1. 创建本地配置副本和安全状态目录。隔离开发环境可以用仓库 helper 生成短期测试
   deployment CA/service certificate；生产部署必须改用组织 CA 或公开 WebPKI：

   ```bash
   cp ./configs/server.example.toml ./configs/server.toml
   cp ./configs/agent.example.toml ./configs/agent.toml
   cp ./configs/nodes.example.toml ./configs/nodes.toml
   install -d -m 0700 ./configs/state
   bash ./scripts/generate-dev-tls.sh ./configs/certs localhost
   ```

   在 `configs/server.toml` 中把 `registry.nodes_file` 改为 `nodes.toml`。证书 SAN 必须
   匹配 Agent 配置中的 `server_name`，Agent 的 `deployment_ca_file` 必须信任其签发
   CA。部署 CA 与稍后生成的节点 CA 是两个不同信任根。helper 生成的 CA 私钥只用于
   本机测试，不能提交或用于生产。

2. 为每个节点生成一次性 enrollment token：

   ```bash
   sudo install -d -o root -g root -m 0700 ./configs/secrets
   sudo ./target/release/stellaris token generate \
     --node-id edge-a --output ./configs/secrets/edge-a.token
   ```

   将输出的 `enrollment_token_sha256`、节点 ID 和静态 IPv4 地址写入
   `configs/nodes.toml`。不要把 token 明文写入节点表。

3. 调整 Server 配置，校验后显式初始化节点 CA 与协调状态：

   ```bash
   ./target/release/stellaris config check --config ./configs/server.toml
   ./target/release/stellaris server init --config ./configs/server.toml
   ./target/release/stellaris server run --config ./configs/server.toml
   ```

   `server init` 不覆盖或轮换已有状态。若上次初始化在完整、严格校验通过的节点 CA
   certificate/key 已落盘后中断，且协调状态仍缺失，重试会保留该 CA 并补建协调
   状态；其他不完整、无效或已有协调状态的组合均拒绝。普通 `server run` 不会自动
   生成缺失、损坏或权限不安全的节点 CA/协调状态。

   `config check`、`server init` 和 `server run` 必须使用拥有 service 私钥和状态目录的
   同一 effective UID。正式部署建议使用同一个专用 Server 账户执行三者。

   `server run` 会持续运行；保持该终端，不要在同一 shell 中继续执行 Agent 命令。

4. 在另一个终端调整 Agent 配置，并在具备 TUN 权限的主机上运行：

   ```bash
   sudo ./target/release/stellaris config check --config ./configs/agent.toml
   sudo ./target/release/stellaris agent run --config ./configs/agent.toml
   ```

   secret 文件和 identity 目录的所有者必须等于进程 effective UID。上例以 root 运行
   Agent，所以 token 也由 root 创建；如果使用具备 `CAP_NET_ADMIN` 的专用账户，token
   生成、配置校验和 Agent 启动都必须使用该账户，不能校验后再切换用户。

5. 为第二个节点使用独立 token、identity 目录、overlay 地址和 P2P bind。防火墙
   放行 Server 的 enrollment/control/relay UDP 端口，以及节点间需要直连的 P2P
   UDP 端口。Stellaris 不修改默认路由或 DNS。

首次注册完成且 identity 目录已持久备份后，可从 Agent 配置中删除
`identity.enrollment_token_file` 并移除 token secret。identity 丢失，或证书已经过期到
无法通过已认证 control 会话续期时，重新 enrollment 需要管理员轮换静态摘要并提供
新的 token。

`--config` 是运行命令唯一的配置覆盖项，也可由 `STELLARIS_CONFIG` 提供。字段、
路径和权限规则见[配置参考](docs/configuration.md)。

## 文档

- [文档中心、状态与权威边界](docs/README.md)
- [文档驱动改造流程](docs/documentation-workflow.md)
- [最新方案总览（当前 v2 / Proposed v3 / 后续方向）](docs/design-overview.md)
- [现代分布式组网方案调研（非规范参考）](docs/research/modern-distributed-network-survey.md)
- [架构](docs/architecture.md)
- [v2 线协议](docs/protocol.md)
- [配置参考](docs/configuration.md)
- [安全模型](docs/security-model.md)
- [节点身份、信任域与地址分配演进计划（Proposed）](docs/node-identity-trust-addressing-plan.md)
- [兼容性](docs/compatibility.md)
- [部署](docs/deployment.md)
- [故障排查](docs/troubleshooting.md)
- [路线图](docs/roadmap.md)
- [分布式组网计划与未完成门禁](docs/distributed-network-plan.md)
- [架构决策记录](docs/adr/README.md)
- [发布流程](docs/releasing.md)
- [验证证据记录](docs/verification/README.md)
- [贡献指南](CONTRIBUTING.md)
- [安全漏洞报告](SECURITY.md)

## 开源许可

Stellaris 采用 `Apache-2.0 OR MIT` 双许可。你可以选择任一许可证使用、修改和分发
本项目。详见 [LICENSE-APACHE](LICENSE-APACHE) 和 [LICENSE-MIT](LICENSE-MIT)。
提交到本仓库且未明确标记为“非贡献”的内容默认按同一双许可提供；项目不要求 CLA
或 DCO。
