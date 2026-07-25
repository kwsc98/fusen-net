# 安全模型

本文说明 Stellaris `0.3.0-alpha.1` 保护什么、信任什么以及明确不保护什么。它不是
安全审计报告。实现仍未通过完整恶意输入、真实 TUN、故障注入和资源 soak 门禁，
部署者不得把 alpha 用于关键生产流量。

## 参与者与资产

需要保护的资产包括部署 service 私钥、节点 CA 私钥、Agent 节点私钥、一次性
enrollment token、持久协调状态、overlay 身份、控制面完整性和用户 IPv4 包。

- **部署管理员**管理 Server、部署证书、节点 CA、静态注册表和持久状态，被完全信任。
- **Agent 管理员**管理一个节点的 token、identity 目录和宿主机。
- **同信任域节点**已经取得节点证书，可与其他节点通信，但不得冒充其他 node/IP。
- **网络攻击者**可监听、丢弃、延迟、重放或注入 underlay UDP 包。
- **未认证客户端**可连接公开 listener 并发送畸形或资源消耗型输入。

系统是单租户全互通模型，不提供 ACL、端口过滤或租户隔离。不要把互不信任的组织
放入同一个节点 CA、静态注册表或 overlay。

## 信任根

Stellaris 有两个独立的 CA 边界：

1. **部署 TLS CA**签发 Server service certificate。Agent 用它验证三个 Server
   listener 的 TLS 身份和显式 `server_name`。
2. **节点 CA**由 `stellaris server init` 创建。它签发 24 小时节点证书，用于
   control/Relay 客户端认证和节点间 P2P mTLS。

协调服务、节点 CA 和可信 Relay 都属于首版信任边界。节点 CA 或协调服务被攻破后，
攻击者可以签发或发布伪造节点身份。部署 service 私钥被攻破可冒充 Server endpoint；
结合 enrollment token 泄露可能取得节点证书。

> **Relay 不提供端到端机密性。** P2P 路径由两个 Agent 之间的 QUIC mTLS 保护；
> Relay 回退路径在 Server 上解密，因此 Relay 可以看到完整 overlay IPv4 包、源/目标、
> 长度和时序。需要对 Relay 隐藏内容时，应用必须自行使用 TLS、SSH、WireGuard 等
> 端到端保护。

## Enrollment 与节点身份

- `stellaris token generate` 为每个节点产生独立的 256-bit `stl2_` token。静态节点
  表只保存 SHA-256 摘要，并使用常量时间比较。
- token 仅在已验证的部署 TLS 连接内发送。Server 成功持久提交签发结果后原子标记
  token 已消费；相同持久 enrollment 请求可幂等重试，其他重用拒绝。
- Agent 在私有 identity 目录中生成 P-256 私钥并用 CSR 证明持有它。私钥不得上传。
- Server 从静态注册表覆盖 node ID 和 overlay IPv4，并禁止 CSR 请求 CA 能力或其他
  身份。节点证书把 node ID、overlay IP、公钥和用途放入签名内容。
- Server 持久化当前授权 SPKI。control/Relay TLS 链验证成功后，还必须要求证书身份、
  静态绑定、enabled 状态、撤销状态和授权 SPKI 全部一致。

仅修改静态 token 摘要不会自动废除当前授权公钥。安全轮换需要更新节点表、重启
Server，并由新私钥使用新 token 完成 enrollment；新 enrollment 提交后替换授权 SPKI，
旧证书不能再次建立授权会话。本阶段没有管理 API，也没有双 token 宽限期。

`enabled = false` 只在 Server 重启后阻止该节点新建 enrollment/control/Relay 会话。
它不是 `PeerRevoked`：已建立的节点间 P2P 可能继续到连接断开、证书到期或空闲回收。
当前固定 CLI 没有正式撤销的管理员触发入口；不可逆撤销传播仍属于未完成 E2E 门禁，
不能把可恢复的 `enabled` 开关描述为即时全网撤销。

## 会话与重放

- enrollment、control、Relay 和 P2P 使用独立 ALPN；0-RTT 禁用。
- mTLS 后的 node ID/IP 来自已验证证书，客户端消息不能覆盖。
- control session 使用随机 UUID 和持久单调 incarnation。旧 session 的更新和清理
  不能影响替代 session。
- control 请求 ID 使用固定大小重放窗口；候选 epoch 在 session 内严格递增。
- Relay 必须绑定同一节点的当前 control lease，并在 `RelayReady` 前保持不可路由。
- P2P 必须同时匹配协调方 connection plan、descriptor 指纹、session、incarnation、
  证书期限和 Ready 状态。
- Server 与 Agent 都必须在节点证书 `NotAfter` 主动关闭相关连接，不能只依赖握手时
  的证书有效期检查。

协调 store 有撤销状态和 `PeerRevoked` 传播能力，但当前 CLI 不提供管理员撤销命令。
运维上可将节点 `enabled = false` 并重启 Server 来拒绝新会话；正式撤销管理流程仍是
发布门禁缺口。

## 数据面策略

每个 QUIC Datagram 恰好包含一个原始 IPv4 包。所有路径检查完整头、总长度和 MTU。

- Relay 入站包的源必须等于发送 session 的静态 overlay IP，目标必须是 overlay 内
  另一个 Ready route。
- P2P 入站包的源必须等于 mTLS 认证 peer 的 overlay IP，目标必须等于本机 overlay
  IP。
- IPv6、网络地址、广播、组播、overlay 外地址、畸形和超 MTU 包全部丢弃。
- 每个出站包只选择一个路径。P2P send 失败的当前包不得补发到 Relay，避免 Stellaris
  主动制造重复包；后续包回退 Relay。

QUIC Datagram 自身允许丢包、乱序和重复。Stellaris 不提供 IP 包重传或排序；应用
需要可靠性时应在 overlay 上运行 TCP 或其他可靠协议。

## 持久化与本地权限

节点 CA、协调状态、Agent 私钥和证书安装采用先写临时文件、fsync、原子替换和目录
fsync 的持久路径；变更落盘前不得对网络确认。持久写入失败会使协调 store fail-closed，
避免继续基于未确认内存状态应答。完整故障点注入矩阵尚未达到发布门禁。

Unix 上，service 私钥、节点 CA 私钥、协调状态、token 和 Agent identity 必须由
effective UID 所有且 group/other 不可访问。Windows 使用受限 ACL，但 Windows
运行时目前未验证。Stellaris 不负责磁盘加密、备份密钥、宿主机补丁或硬件密钥保护。

## 资源与拒绝服务

实现限制 JSON payload、候选数量、节点数量、每 IP 连接数、并发 enrollment、控制
队列和数据队列，并拒绝不受支持的方向与类型。运行时计数器覆盖会话、路径包、丢包、
enrollment/续期、P2P 结果、路径切换和队列高水位。

这些边界不能防御大规模带宽或 CPU DDoS。公网部署仍需防火墙和外部 UDP 速率限制。
内建 HTTP `/metrics` 只导出无标签计数器和 gauge，并限制并发、请求大小与读取时间；
它尚未通过发布门禁，只能绑定可信管理接口，不能暴露到不可信网络。

## 明确不保护

- 恶意或被攻破的协调服务、节点 CA、可信 Relay；
- 被攻破的 Agent 操作系统、管理员或应用；
- 同租户节点之间的访问控制；
- Relay 回退包的端到端机密性；
- 匿名性、流量隐藏和抗流量分析；
- 大规模分布式拒绝服务；
- NAT 穿透、Internet 出口、DNS、默认路由或子网路由。

安全问题按 [`SECURITY.md`](../SECURITY.md) 私密报告。
