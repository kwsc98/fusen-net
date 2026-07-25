# Stellaris

[English](README.en.md)

Stellaris 是一个基于 QUIC Datagram 和 TUN 的分布式 IPv4 overlay 网络。当前
`0.3.0-alpha.1` 采用单实例协调服务、可信 Relay 和按需局域网 P2P：Agent 首先
建立可用的 Relay 路径，发现同一局域网内的目标后尝试 Quinn 直连，并在 P2P
Ready 后按目标 overlay IP 切换路径。

> **项目状态：早期预览。** v2 运行时代码已经接入 CLI，但完整集成测试、Linux
> 真实 TUN、故障注入和资源 soak 门禁尚未完成。不要把当前 alpha 用于关键生产
> 流量，也不要根据本文推断某个平台已经通过发布验证。

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

> **可信 Relay 边界：** P2P 包由节点间 QUIC mTLS 保护，不经过 Server 数据面；
> Relay 回退包会在 Server 上解密，Server 可以看到完整 overlay IPv4 包及流量
> 元数据。本版本不提供 Relay 路径端到端加密。

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
TUN；Agent 在 Linux 上需要 `/dev/net/tun` 和 root 或 `CAP_NET_ADMIN`。

## 快速开始

示例位于 [`configs/`](configs/)。以下流程会创建全新的 v2 状态；旧配置、旧状态和
旧 Agent 不能继续使用。

1. 准备部署服务证书和私钥。证书 SAN 必须匹配 Agent 配置中的 `server_name`，
   Agent 的 `deployment_ca_file` 必须信任其签发 CA。部署 CA 与稍后生成的节点 CA
   是两个不同的信任根。

2. 为每个节点生成一次性 enrollment token：

   ```bash
   mkdir -p ./configs/secrets
   stellaris token generate \
     --node-id edge-a --output ./configs/secrets/edge-a.token
   ```

   将输出的 `enrollment_token_sha256`、节点 ID 和静态 IPv4 地址写入
   `configs/nodes.example.toml` 的副本。不要把 token 明文写入节点表。

3. 调整 Server 配置，校验后显式初始化节点 CA 与协调状态：

   ```bash
   stellaris config check --config ./configs/server.example.toml
   stellaris server init --config ./configs/server.example.toml
   stellaris server run --config ./configs/server.example.toml
   ```

   `server init` 不覆盖或轮换已有状态。若上次初始化在完整、严格校验通过的节点 CA
   certificate/key 已落盘后中断，且协调状态仍缺失，重试会保留该 CA 并补建协调
   状态；其他不完整、无效或已有协调状态的组合均拒绝。普通 `server run` 不会自动
   生成缺失、损坏或权限不安全的节点 CA/协调状态。

4. 调整 Agent 配置，在具备 TUN 权限的主机上运行：

   ```bash
   stellaris config check --config ./configs/agent.example.toml
   sudo stellaris agent run --config ./configs/agent.example.toml
   ```

5. 为第二个节点使用独立 token、identity 目录、overlay 地址和 P2P bind。防火墙
   放行 Server 的 enrollment/control/relay UDP 端口，以及节点间需要直连的 P2P
   UDP 端口。Stellaris 不修改默认路由或 DNS。

首次注册完成且 identity 目录已持久备份后，可从 Agent 配置中删除
`identity.enrollment_token_file` 并移除 token secret。证书失效后重新注册需要管理员
轮换静态摘要并提供新的 token。

`--config` 是运行命令唯一的配置覆盖项，也可由 `STELLARIS_CONFIG` 提供。字段、
路径和权限规则见[配置参考](docs/configuration.md)。

## 文档

- [架构](docs/architecture.md)
- [v2 线协议](docs/protocol.md)
- [配置参考](docs/configuration.md)
- [安全模型](docs/security-model.md)
- [兼容性](docs/compatibility.md)
- [部署](docs/deployment.md)
- [故障排查](docs/troubleshooting.md)
- [路线图](docs/roadmap.md)
- [分布式组网计划与未完成门禁](docs/distributed-network-plan.md)
- [架构决策记录](docs/adr/README.md)
- [发布流程](docs/releasing.md)
- [贡献指南](CONTRIBUTING.md)
- [安全漏洞报告](SECURITY.md)

## 开源许可

Stellaris 采用 `Apache-2.0 OR MIT` 双许可。你可以选择任一许可证使用、修改和分发
本项目。详见 [LICENSE-APACHE](LICENSE-APACHE) 和 [LICENSE-MIT](LICENSE-MIT)。
提交到本仓库且未明确标记为“非贡献”的内容默认按同一双许可提供；项目不要求 CLA
或 DCO。
