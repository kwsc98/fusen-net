# Fusen Net

[English](README.en.md)

Fusen Net 是一个基于 QUIC Datagram 和 TUN 的三层虚拟网络。`0.1` 使用中心
Relay 转发 IPv4 包：Edge 从本机 TUN 读取完整 IPv4 包，经 QUIC 发送到 Relay，
Relay 再按照目标 overlay 地址把包转发给另一个在线 Edge。

中文 README 和 `docs/` 是 0.1 行为的权威说明；英文 README 仅提供精简入口。

> **项目状态：早期预览。** `0.1.0-alpha` 正在重构协议、安全边界和跨平台
> 适配，尚未经过独立安全审计，不应直接暴露在不受信任的生产网络中。旧版
> TCP 端口代理的命令、配置和协议均不兼容。

```text
Edge A                  Relay                         Edge B
10.88.0.2/24            UDP listeners                10.88.0.3/24
TUN <-> IPv4 packet <-> QUIC Datagram <-> route <-> QUIC Datagram <-> TUN
            quinn --------^                 ^-------- s2n
```

每条 Edge 到 Relay 的链路必须选择相同的 QUIC 后端；Relay 可以同时监听
`quinn`、`s2n` 和 `gm-quic`，并在不同后端的已认证会话之间转发数据。

## 0.1 能力边界

- 中心 Relay、单租户、IPv4-only。
- 静态绑定 `node_id + token_sha256 + overlay IP`。
- TLS 服务端认证和每节点 256-bit token 鉴权。
- 原始 IPv4 Datagram 转发；默认 MTU 为 1100。
- Linux、macOS 和 Windows 原生 Agent 是首个稳定版的目标平台。
- 不包含 TCP 端口代理兼容层、ACL、IPv6、DNS、默认路由接管、NAT 穿透、
  节点发现或完整 mesh。

当前实现和已验证的平台/后端状态见
[兼容性说明](docs/compatibility.md)，不要仅根据 feature 存在与否判断生产可用性。

## 构建

需要 Rust 1.97.0 或更高版本。建议使用仓库中的工具链文件：

```bash
rustup show
cargo build --workspace --all-features --locked
cargo test --workspace --all-features --locked
```

正式二进制由 `fusen-net-cli` package 生成，名称为 `fusen-net`：

```bash
cargo build --release -p fusen-net-cli --all-features --locked
./target/release/fusen-net --version
```

Relay 不创建 TUN，通常不需要管理员权限。Agent 需要创建 TUN 和 overlay
路由：Linux 需要 root 或 `CAP_NET_ADMIN` 及 `/dev/net/tun`，macOS 需要允许
创建 utun/路由，Windows 需要管理员权限和可用的 Wintun 驱动。

## 快速开始

示例配置位于 [`configs/`](configs/)。开始前准备一个 SAN 与 Agent 使用的
`server_name` 一致的服务端证书，并让 Agent 信任其签发 CA。证书私钥不得提交
到仓库。

1. 为每个 Edge 生成独立 token：

   ```bash
   mkdir -p ./secrets
   cargo run --locked -p fusen-net-cli -- token generate \
     --node-id edge-a --output ./secrets/edge-a.token
   ```

2. 将命令输出的 `sha256:<hex>` 摘要和分配的 overlay IP 写入
   `configs/nodes.example.toml` 的副本；调整 Server 和 Agent 配置中的证书、
   token、地址及 backend。

3. 启动 Relay；监听地址使用 UDP，而不是 TCP：

   ```bash
   cargo run --locked -p fusen-net-cli -- \
     config check --config ./configs/server.example.toml
   cargo run --locked -p fusen-net-cli -- \
     server --config ./configs/server.example.toml
   ```

4. 在 Edge 主机上校验并以所需权限启动 Agent：

   ```bash
   cargo run --locked -p fusen-net-cli -- \
     config check --config ./configs/agent.example.toml
   sudo ./target/release/fusen-net agent \
     --config ./configs/agent.example.toml
   ```

5. 启动两个不同 overlay 地址的 Edge 后，从一端 ping 另一端的 overlay IP。
   Relay 防火墙必须放行对应 `[[listeners]]` 的 UDP 端口。Fusen Net 只添加
   overlay CIDR 路由，不会修改默认路由或 DNS。

完整字段、相对路径和环境变量规则见
[配置参考](docs/configuration.md)，生产部署见
[部署指南](docs/deployment.md)。

## 文档

- [架构](docs/architecture.md)
- [线协议 v1](docs/protocol.md)
- [配置参考](docs/configuration.md)
- [安全模型](docs/security-model.md)
- [兼容性](docs/compatibility.md)
- [部署](docs/deployment.md)
- [故障排查](docs/troubleshooting.md)
- [路线图](docs/roadmap.md)
- [架构决策记录](docs/adr/README.md)
- [发布流程](docs/releasing.md)
- [贡献指南](CONTRIBUTING.md)
- [安全漏洞报告](SECURITY.md)

## 开源许可

Fusen Net 采用 `Apache-2.0 OR MIT` 双许可。你可以选择任一许可证使用、修改
和分发本项目。详见 [LICENSE-APACHE](LICENSE-APACHE) 和
[LICENSE-MIT](LICENSE-MIT)。提交到本仓库且未明确标记为“非贡献”的内容，
默认按同一双许可提供；项目不要求 CLA 或 DCO。
