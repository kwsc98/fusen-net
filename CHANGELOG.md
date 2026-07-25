# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 和
[语义化版本](https://semver.org/lang/zh-CN/)。`0.x` 期间允许破坏式变更。

## [Unreleased]

## [0.3.0-alpha.1] - 2026-07-25

### Added

- 新增 Quinn 驱动的协调服务、可信 Relay 与按需局域网 P2P 运行时。
- 新增独立 enrollment、control、relay、P2P ALPN 和严格的版本 2 帧协议。
- 新增显式 `stellaris server init`，持久化节点 CA 与协调状态；普通启动只加载且
  校验现有状态。
- 新增一次性 enrollment token、CSR PoP、24 小时节点 mTLS 证书、续期和授权 SPKI
  持久化。
- 新增同 socket 监听与拨号的 Quinn Hybrid endpoint、完整候选目录、重复连接仲裁、
  Relay/P2P 原子路径选择、断线回退及五分钟空闲回收。
- 新增有界控制、Relay 和 P2P 队列、会话/注册限流、运行指标以及事务式协调状态和
  Agent 身份存储。
- 新增有界的 Prometheus HTTP `/metrics` 端点；仅在配置 `observability.metrics_bind`
  时监听。

### Changed

- 项目、crate、CLI、镜像和部署路径统一命名为 Stellaris，仓库地址为
  `https://github.com/kwsc98/Stellaris`。
- CLI 固定为 `server init|run`、`agent run`、`config check` 和 `token generate`；
  运行时只接受 `--config` 或 `STELLARIS_CONFIG`。
- Server 与 Agent 配置以及静态节点表统一使用严格 schema 版本 2。
- 分布式运行时只使用 Quinn；s2n-quic 和 gm-quic 继续参与传输抽象编译检查，但
  不能由运行配置选择。
- 首轮正式运行门禁限定 Linux；macOS 和 Windows 当前只要求编译通过。

### Removed

- 删除旧线协议、旧配置、旧运行时、地址租约分配器、后端 CLI 选择和所有兼容适配。
- 不提供状态迁移器、配置迁移器、双栈 listener、协议降级或混合集群模式。
- NAT 穿透、server-reflexive candidates、外部 STUN、HA、ACL、IPv6、动态地址和
  管理 API 不进入本版本。

### Security

- 注册端点使用部署服务 TLS；control/relay 使用节点 CA mTLS；P2P 双方只信任节点
  CA，并校验证书身份、overlay 地址、指纹、有效期和当前协调计划。
- Relay 仍属于可信边界，回退流量对 Relay 可见；本版本不提供 Relay 端到端加密。
- 本版本仍是 alpha，尚未完成 Linux 真实 TUN、完整 E2E、故障注入和长时间 soak
  发布门禁，不应直接用于不受信任的生产网络。
