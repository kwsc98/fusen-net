# 兼容性

本文区分“可以编译”“计划支持”和“已经通过发布门禁”。feature 或平台代码存在
不表示已经达到生产支持等级。

## 版本规则

- 软件版本遵循 SemVer。`0.x` 的 minor 版本可以包含不兼容变化，patch 版本应
  保持同一线协议和配置 schema 兼容。
- 线协议独立版本化，0.1 使用 `fusen-net/1` 和帧头版本 `1`，不隐式降级。
- 配置以顶层 `version = 1` 标识 schema。未知版本必须拒绝，不能猜测字段含义。
- 建议 Relay 和全部 Edge 使用同一 release。至少必须使用同一协议版本。

## 旧版迁移

0.1 不兼容旧 TCP 端口代理的 CLI、配置、镜像和线协议，也不接受客户端自选 TUN
IP。迁移必须重新部署：

1. 为旧主线保留 `legacy-tcp-v4` Git 标签，以便审计和紧急回退。
2. 为每个 Edge 分配唯一 overlay IPv4 地址并生成独立 token。
3. 部署新的 Relay UDP listener、TLS 证书和静态注册表。
4. 逐台安装 0.1 Agent，并以 overlay ping/TCP/UDP 验证。

新旧客户端不能连接同一个 listener。升级时应使用不同 UDP 端口并行验证，而不是
原地复用旧 TCP 端口。

## QUIC 后端

| 后端 | Cargo feature | 0.1 角色 | 稳定版要求 |
| --- | --- | --- | --- |
| Quinn | `backend-quinn` | 默认参考实现 | 完整契约测试和三平台 E2E |
| s2n-quic | `backend-s2n` | 可选 listener/Agent | 与 Quinn 相同的契约测试 |
| gm-quic | `backend-gm-quic` | 预发布测试用 listener/Agent | 本地 fork 契约测试、三平台真实 TUN、背压及 soak 全部通过 |
| 全部 | `all-backends` | 正式 CLI 构建 | 三后端全部通过才发布稳定版 |

每条 Agent-Relay 链路必须连接同后端 listener。Relay 可以同时运行三种 listener，
并在不同后端会话之间路由，所以测试矩阵包括 3 x 3 的源/目标组合；这不等于承诺
不同 QUIC 库直接建立连接。

任一后端在连接、TLS/SNI/ALPN、双向控制流、Datagram、关闭、超时或错误语义的
契约测试失败时，该后端不得标为稳定。稳定版要求三后端全部通过。

## 平台

| 平台 | Relay | Agent/TUN | 0.1 稳定版门禁 |
| --- | --- | --- | --- |
| Linux x86_64 | 目标支持 | TUN + route | 真实 ping/TCP/UDP、重连、回滚及 soak |
| macOS x86_64 | 目标支持 | utun + route | 真实 ping/TCP/UDP、重连和回滚 |
| Windows x86_64 | 目标支持 | Wintun + route | 真实 ping/TCP/UDP、重连和回滚 |

其他架构可能可以编译，但不发布预构建制品，也不属于 0.1 支持范围。macOS 和
Windows 原生支持不代表支持在这些平台运行 Agent 容器。

当前 commit 的确切验证结果以 CI 和对应 Release Notes 为准。尚未完成三平台真实
TUN 门禁的 alpha/beta 版本只能用于测试。

## 网络兼容

- Overlay：仅 IPv4 单播；默认 MTU 1100。
- Underlay：需要 Edge 到 Relay UDP listener 的双向可达性。
- NAT：普通出站 NAT 通常可用，但不提供 NAT 穿透或 Edge 入站监听。
- Proxy：不支持 TCP/HTTP/SOCKS 代理作为 QUIC underlay。
- IPv6 underlay 是否可用取决于所选后端和 listener 地址，但不改变 IPv4-only
  overlay 承诺；发布前需有对应测试才能声明支持。
