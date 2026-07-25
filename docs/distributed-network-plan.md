# Stellaris v2 分布式网络实施与验收计划

> **状态：`0.3.0-alpha.1` 实现已接入，验证门禁未完成。** 本文记录破坏式 v2
> 改造已经落地的边界、仍需完成的验收，以及后续 NAT 阶段。当前线格式以
> [`protocol.md`](protocol.md) 为准，配置以 [`configuration.md`](configuration.md)
> 为准。本文不构成稳定支持或发布日期承诺。

## 目标与固定边界

Stellaris 面向自托管的单一信任域，使用“单实例协调服务 + 可信 Relay + 按需 LAN
P2P”。Agent 对没有 Ready 直连的流量先使用 Relay，同时请求连接计划；节点间 Quinn
mTLS Ready 后，按目标 overlay IP 原子切换为 P2P。

当前固定边界：

- 单租户、IPv4-only、静态 node ID/overlay 地址、最多 256 个节点；
- 静态节点表只在 Server 启动时加载，没有管理 API 或动态地址分配；
- 单个持久化协调实例，不实现共识、复制或领导选举；
- v2 Server/Agent 运行时只使用 Quinn；其他传输依赖只保留编译边界；
- 默认 MTU 1100，P2P 空闲回收 5 分钟；
- 当前只使用 IPv4 host candidate，不做 NAT 打洞或地址映射观察；
- Linux 是首轮运行门禁，macOS/Windows compile-only、运行未验证。

> **信任边界：** 协调服务和节点 CA 可以签发/发布节点身份；可信 Relay 可以看到
> 完整回退包及流量元数据。只有 P2P 路径具备节点间 QUIC mTLS。当前不防御恶意
> Server、CA 或 Relay，也不提供 Relay 路径端到端加密。

## 已接入实现

### 破坏式公共接口

- 唯一命令树为 `stellaris server init|run`、`stellaris agent run`、
  `stellaris config check` 和 `stellaris token generate`。
- Server/Agent/节点表只接受 schema v2；未知字段和其他版本 fail-closed。
- 运行命令只保留 `--config`/`STELLARIS_CONFIG`，没有细粒度覆盖或传输选择。
- Server 配置 enrollment/control/relay 三个不冲突的 IPv4 UDP bind；Agent 配置三者
  地址和一个 `p2p.bind`。

### 身份与持久状态

- `server init` 显式创建节点 CA 和空协调状态；普通启动不自动生成信任根。
- `NodeIdentityStore` 安全管理 Agent P-256 私钥、持久 enrollment 请求、证书和节点 CA。
- enrollment 使用部署 TLS、一次性 token 和 CSR proof-of-possession。相同完整请求
  幂等返回已提交结果，不一致重试或 token 重用拒绝。
- 节点证书有效期 24 小时；control session 支持续期并要求 CSR 保持当前公钥。
- `CoordinatorStore` 持久化 incarnation、撤销、已消费 token、授权 SPKI 和幂等签发
  结果。变更使用 copy-next-state 与原子文件替换，失败后 fail-closed。
- TLS 连接完成链、用途、ALPN、期限验证后提取签名 node/IP/SPKI；网络消息不能覆盖
  认证身份。

### Control 与 Relay

- control mTLS 注册 session，持久增加 incarnation，并用 `ControlWelcome` 提供权威
  overlay CIDR、MTU、节点 IP 和证书期限。
- 控制协议具有候选 epoch、固定请求重放窗口、peer descriptor、双端 connection plan、
  续期和撤销消息。
- Relay mTLS 必须绑定同节点当前 control lease。只有 `RelayReady` 后进入 route table；
  旧 session 清理不能删除新 session route。
- Relay Datagram 直接承载原始 IPv4 包，并校验完整长度、MTU、认证源地址和目标 route。

### Agent 与 LAN P2P

- Agent 创建/加载本地身份，缺少有效证书时 enrollment，随后建立 control、Relay、TUN
  和独立生命周期的 Hybrid P2P endpoint。
- P2P endpoint 在同一 Quinn UDP socket 上监听和拨号，并可更新 TLS 身份而不 rebind。
- Agent 发布可用 IPv4 host candidate。目标流量选择 Relay的同时请求 `ConnectPlan`；
  双方准备入站并尝试候选拨号。
- `PeerManager` 保存实际候选、证书期限、plan、拨号退避、重复连接仲裁和路径状态。
- P2P 必须完成节点 CA mTLS、descriptor 指纹/session/incarnation 校验和 Ready 后才
  切换。断线、撤销、候选变化、替代 incarnation、证书到期和空闲超时均回退 Relay。
- 每包只选择一次路径。P2P 发送失败时当前包丢弃，后续包走 Relay，禁止补发同一包。
- P2P 入站严格要求 source 等于认证 peer IP、destination 等于本机 IP。

### 资源边界

- 节点、候选、控制事件、并发 enrollment、每 IP 连接和数据 queue 都有上限。
- 指标核心记录 control/Relay/P2P session、路径包、分类丢包、路径切换、enrollment、
  续期、P2P 结果和 queue 高水位，并可通过有界 HTTP `/metrics` 导出。
- 在线 session 不进入快照；Server 重启后必须重新认证。

## 未完成验收

实现进入源码不表示以下行为已由最终测试树证明。

### 协议与身份

- [ ] 错误 ALPN/帧版本、未知/重复字段、方向错误、超限 payload 和 0-RTT 拒绝；
- [ ] CSR PoP、错误 token、一次性消费、幂等重试和内容冲突；
- [ ] 错误 CA、node/IP/SPKI 不匹配、续期越权、到期关闭和旧证书重连拒绝；
- [ ] 请求窗口、candidate epoch、session/incarnation 和重复连接状态机乱序；
- [ ] 禁用、token/SPKI 轮换及撤销传播的端到端证据。

### 持久化

- [ ] 在临时写、文件 fsync、rename、目录 fsync 各点注入失败；
- [ ] 确认落盘前不应答，内存不超前于磁盘，失败 store 不继续 mutation；
- [ ] Server 重启读取一致状态，缺失、损坏、旧格式、symlink 和不安全权限 fail-closed；
- [ ] 节点 CA、授权 SPKI、消费 token 和 incarnation 的备份恢复演练。

### Relay 与 P2P 数据面

- [ ] 两 Agent 经 Relay 双向 IPv4、Ready 前丢弃、源伪造、MTU、背压和 session ABA；
- [ ] 两节点/多节点 LAN P2P、同时拨号、候选轮换、空闲回收和断线回退；
- [ ] 续期时替代 control/Relay/P2P 身份且 P2P socket 不 rebind；
- [ ] 记录包 ID，证明 Relay/P2P 切换无主动重复包、无环路，失败 P2P 包不补发；
- [ ] 协调服务、Relay、Agent 各自中断后的明确恢复行为。

### Linux 真实网络与容量

- [x] 存在单机原生 TUN/route 生命周期 ignored smoke harness。
- [ ] Linux network namespace 中的完整 v2 enrollment/control/Relay/P2P ping、TCP、UDP；
- [ ] Server 重启、Agent 重启、路由回滚、丢包、乱序、MTU 黑洞和恢复；
- [ ] 至少 30 分钟连接抖动 soak，检查 RSS、任务、线程、文件描述符和 queue；
- [ ] 256 节点目录/状态机边界，完整 256 在线 Agent soak 留到规模加固阶段。

当前 `tests/e2e/run-real-tun.sh linux all` 会要求尚未实现的命名测试，因此失败是门禁
未完成的显式信号，不是通过证据。任何文档或 Release Notes 都不得声称这些测试已
执行成功，除非对应 commit 有可审计 CI/runner 结果。

## 后续里程碑

| 阶段 | 交付 | 退出门禁 |
| --- | --- | --- |
| `0.3` LAN P2P 加固 | 关闭上述协议、持久化、Relay/P2P 和 Linux 真实网络缺口 | Linux `all` 门禁、无重复包证据、30 分钟 soak 全部通过 |
| `0.4` NAT 穿透 | P2P socket 观察地址、server-reflexive candidate、双向打洞和探测 | 常见 NAT 直连；对称/不可穿透 NAT 稳定 Relay 回退 |
| `0.5` 规模与运维 | 256 节点容量、指标导出加固、健康检查、持续 fuzz/故障注入 | 资源在连接抖动和 Server 重启中不持续增长 |
| `1.0` 稳定版 | 部署/恢复演练、威胁模型和支持矩阵 | 每个声明支持的平台都有真实 TUN/P2P 与安全复核证据 |

多协调服务 HA、Relay 路径端到端加密、多租户、ACL、IPv6、动态地址、管理 API、
DNS、默认/子网路由、外部 STUN/TURN 和其他 QUIC 运行实现不进入当前阶段。
