# 路线图

路线图描述目标和退出门禁，不承诺日期。代码存在不等于阶段完成；只有实现、CI、
真实网络证据和文档全部满足门禁后，才能将能力标记为已验证。

## `0.3.0-alpha.1`：破坏式 v2 切换

当前代码目标是一次完成新的分布式运行边界：

- [x] CLI 使用 `server init|run`、`agent run`、`config check`、`token generate`。
- [x] 严格 schema v2、静态节点表、三个 Server UDP listener 和 Agent P2P bind。
- [x] 四类独立 ALPN、固定 v2 帧、方向/大小/JSON 严格校验。
- [x] 节点 CA 初始化、Agent 本地密钥、一次性 enrollment、24 小时节点证书和持久
  授权 SPKI/幂等结果。
- [x] Quinn control 与 Relay runtime、control lease 绑定、Ready 后 Datagram 路由。
- [x] Quinn Hybrid endpoint、host candidate connection plan、按需 LAN P2P、单路径选择、
  Relay 回退和五分钟默认空闲回收。
- [x] 会话、路径包、丢包、证书操作、P2P 结果、路径切换和队列高水位计数核心。

本 alpha **尚未完成**：

- [ ] 完整 workspace test/Clippy/rustdoc 在最终改造树上全部通过；
- [ ] enrollment/control/Relay/P2P 网络集成及恶意输入矩阵；
- [ ] 写入、fsync、rename、目录 fsync 的逐点持久化故障注入；
- [ ] 两节点和多节点 LAN P2P、同时拨号、续期替换、撤销和 session ABA E2E；
- [ ] 用包 ID 证明切换无主动重复包、无环路，P2P 失败包不补发；
- [ ] Linux namespace 真实 TUN 的 ping/TCP/UDP、Server/Agent 重启、故障注入；
- [ ] 至少 30 分钟连接抖动和资源 soak；
- [ ] 256 节点容量边界验证。

因此当前只能称为“运行时实现已接入、发布门禁未完成”，不能称为稳定可用。

## 下一阶段：`0.3` LAN P2P 加固

下一步优先关闭当前门禁，而不是扩展协议范围：

1. 完成真实 enrollment -> control -> Relay -> TUN 双向闭环测试。
2. 完成双/多 Agent LAN P2P 和 Relay/P2P 原子切换证据。
3. 完成证书续期替代会话、到期关闭、token/SPKI 轮换和撤销测试。
4. 完成持久化 crash consistency、恶意输入、背压和资源泄漏检查。
5. 让 Linux `tests/e2e/run-real-tun.sh linux all` 的全部命名门禁真实存在并通过。

macOS 和 Windows 在本阶段保持编译通过、运行未验证，不阻塞首轮 Linux alpha 门禁。

## `0.4`：NAT 穿透

LAN 门禁完成后再设计和实现：

- P2P socket 的服务端观察地址与 server-reflexive candidate；
- 双向 UDP 打洞、候选优先级和路径健康探测；
- 常见 NAT 实验矩阵，以及对称/不可穿透 NAT 的稳定 Relay 回退；
- 地址变化、丢包、乱序和 MTU 黑洞后的恢复。

本阶段不依赖外部 STUN；若要加入 STUN/TURN，需新的 ADR 和威胁模型。

## `0.5`：规模与加固

- 256 在线节点容量与资源 soak；
- 指标导出发布门禁、健康检查和运维告警；
- 持续 fuzz、故障注入、连接抖动和协调服务重启；
- 内存、任务、文件描述符和队列不得持续增长。

## `1.0`：分布式稳定版

在 Linux 稳定门禁基础上完成部署/恢复演练、威胁模型复核、安全审查和明确支持矩阵。
macOS/Windows 只有在各自真实 TUN/P2P 证据完成后才提升为运行支持。

多协调服务 HA、Relay 路径端到端加密、多租户、ACL、IPv6、动态地址、管理 API、
DNS、默认/子网路由、外部 STUN/TURN 和其他 QUIC 实现的运行支持留到后续版本。
