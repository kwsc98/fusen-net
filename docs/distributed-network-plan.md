# Stellaris v2 分布式网络实施与验收计划

> **文档适用性：Current；适用范围：v2 验收计划；设计评审状态：N/A；ADR 决策状态：
> N/A；交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**
> `0.3.0-alpha.1` 实现已接入。本文冻结破坏式 v2 已落地边界和验收 Gate；顶部生命周期
> 元数据投影文档整体状态，每个 Gate 的 current result 只由
> [`verification/`](verification/README.md) 中适用目标 commit 的 latest result 计算。当前线格式以
> [`protocol.md`](protocol.md) 为准，配置以 [`configuration.md`](configuration.md)
> 为准。本文不构成稳定支持或发布日期承诺。

本文定义“若要发布 v2”必须提供的证据。若 ADR 0004 接受，进入 v3 前只执行
本文“v3 决策前的可复用基线”定义的原子 Gate，完整发布门禁转移到 v3，不在将被删除的
v2 协议上重复执行。[`roadmap.md`](roadmap.md) 只维护这个决策顺序，不定义 Gate。

## 目标与固定边界

Stellaris 面向自托管的单一信任域，使用“单实例协调服务 + 可信 Relay + 按需 LAN
P2P”。Agent 对没有 Ready 直连的流量先使用 Relay，同时请求连接计划；节点间 Quinn
mTLS Ready 后，按目标 overlay IP 原子切换为 P2P。

当前固定边界：

- 单租户、IPv4-only、静态 node ID/overlay 地址、最多 256 个节点；
- 地址容量按 CIDR 的可用普通 host 计算；默认 `/24` 只能容纳 254 个普通地址，验证
  256 条完整 node/IP 记录时必须使用至少 `/23`；
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
- Agent 发布可用 IPv4 host candidate。目标流量选择 Relay 的同时请求 `ConnectPlan`；
  双方准备入站并尝试候选拨号。
- `PeerManager` 保存实际候选、证书期限、plan、拨号退避、重复连接仲裁和路径状态。
- P2P 必须完成节点 CA mTLS、descriptor 指纹/session/incarnation 校验和 Ready 后才
  切换。断线、撤销、候选变化、替代 incarnation、证书到期和空闲超时均回退 Relay。
- 每包只选择一次路径。P2P 发送失败时当前包丢弃，后续包走 Relay，禁止补发同一包。
- P2P 入站严格要求 source 等于认证 peer IP、destination 等于本机 IP。

host candidate 过滤与真实 underlay 路径证明是两个独立门禁。在
`V2-B2-CANDIDATE-01` 和 `V2-B2-UNDERLAY-01` 的适用 latest result 均通过前，P2P
路径计数只证明建立了节点间 QUIC，不能证明地址已经排除 TUN/overlay 或数据面已经绕过
Relay。

### 资源边界

- 节点、候选、控制事件、并发 enrollment、每 IP 连接和数据 queue 都有上限。
- 指标核心记录 control/Relay/P2P session、路径包、分类丢包、路径切换、enrollment、
  续期、P2P 结果和 queue 高水位，并可通过有界 HTTP `/metrics` 导出。
- 在线 session 不进入快照；Server 重启后必须重新认证。

## v3 决策前的可复用基线

以下是评审 ADR 0004 前必须关闭的最小基线，不等于 v2 完整发布门禁。每个 ID 只对应
一个可独立判定的结果；执行记录必须绑定目标 commit，并写入
[`verification/`](verification/README.md)。表中的 Open 是计划冻结时的初始状态，不回写
为 Passed；本地工作区检查或测试文件存在也不能改变 current result。

| 原子 Gate ID | 可独立判定的结果 | 初始状态 |
| --- | --- | --- |
| `V2-B1-FORMAT-01` | `cargo fmt --all -- --check` 对目标 commit 通过 | Open |
| `V2-B1-CLIPPY-01` | workspace all-targets/all-features Clippy 以 `-D warnings` 通过 | Open |
| `V2-B1-RUSTDOC-01` | workspace all-features rustdoc 以 `-D warnings` 通过 | Open |
| `V2-B1-TEST-01` | workspace all-features test 对目标 commit 通过且没有以 ignored/skip 代替规定场景 | Open |
| `V2-B2-RELAY-01` | 两 Agent 可重复完成 enrollment、control、Ready Relay 和双向 IPv4 最小闭环 | Open |
| `V2-B2-P2P-01` | 两 Agent 可重复完成 LAN P2P Ready、按目标切换和断线后续包回退 | Open |
| `V2-B2-CANDIDATE-01` | Agent 发布的 host candidate 明确排除 TUN 与 overlay 地址 | Open |
| `V2-B2-UNDERLAY-01` | 路由或抓包证据证明 P2P 用户包使用真实 underlay 且不经过 Server 数据面 | Open |
| `V2-B3-PATH-01` | 包 ID 证明每包只选一次路径，失败 P2P 包不补发 Relay 且无主动环路/重复包 | Open |
| `V2-B3-SESSION-ABA-01` | 延迟的旧 session/route/path 清理不删除或替换新 session 状态 | Open |
| `V2-B4-STATE-256-01` | 至少 `/23` overlay 的 256 条目录/持久状态记录边界通过，不启动 256 个在线 Agent | Open |
| `V2-B5-PERSIST-01` | 写入、文件 fsync、rename 和目录 fsync 故障矩阵证明持久提交前不确认 | Open |
| `V2-B5-TUN-CLEANUP-01` | Linux 原生 TUN/route 生命周期 harness 实际通过并证明正常与异常退出均清理路由 | Open |

同一次测试运行可以为多个 Gate 提供证据，但必须逐项记录预期、实际结果和 artifact，
不能用一个宽泛的“network passed”结果同时关闭整组 Gate。全部 `V2-B*` 原子 Gate 关闭后
才进入唯一的 `V3-P0` 决策冻结。

## 完整发布验收门禁

以下 `[ ]` 是冻结计划时的初始标记，不是实时勾选状态，也不在证据归档后回写。实现进入
源码不表示门禁通过；current result 一律按验证目录中的适用 latest result 计算。

### 协议与身份

Gate group：`V2-FULL-PROTOCOL`。

- [ ] `V2-FULL-PROTOCOL-01`：错误 ALPN/帧版本、未知/重复字段、方向错误、超限 payload
  和 0-RTT 拒绝；
- [ ] `V2-FULL-PROTOCOL-02`：CSR PoP、错误 token、一次性消费、幂等重试和内容冲突；
- [ ] `V2-FULL-PROTOCOL-03`：错误 CA、node/IP/SPKI 不匹配、续期越权、到期关闭和旧
  证书重连拒绝；
- [ ] `V2-FULL-PROTOCOL-04`：请求窗口、candidate epoch、session/incarnation 和重复连接
  状态机乱序；
- [ ] `V2-FULL-PROTOCOL-05`：禁用、token/SPKI 轮换及撤销传播的端到端证据。

### 持久化

Gate group：`V2-FULL-PERSISTENCE`。

本组首先要求上方基线表的 `V2-B5-PERSIST-01` 关闭，不在此复制其定义或状态。额外
Gate 为：

- [ ] `V2-FULL-PERSISTENCE-01`：内存状态不超前于磁盘，持久 store 失败后拒绝继续
  mutation；
- [ ] `V2-FULL-PERSISTENCE-02`：Server 重启读取一致状态，缺失、损坏、旧格式、symlink
  和不安全权限 fail-closed；
- [ ] `V2-FULL-PERSISTENCE-03`：节点 CA、授权 SPKI、消费 token 和 incarnation 的备份
  恢复演练。

### Relay 与 P2P 数据面

Gate group：`V2-FULL-DATA-PLANE`。

本组首先要求上方基线表的 `V2-B2-RELAY-01`、`V2-B2-P2P-01`、
`V2-B2-CANDIDATE-01`、`V2-B2-UNDERLAY-01`、`V2-B3-PATH-01` 和
`V2-B3-SESSION-ABA-01` 关闭；这些 Gate 的定义和状态不在此复制。额外 Gate 为：

- [ ] `V2-FULL-DATA-PLANE-01`：Relay Ready 前丢弃、源伪造、MTU 和背压行为通过；
- [ ] `V2-FULL-DATA-PLANE-02`：多节点 LAN P2P、同时拨号、候选轮换和空闲回收通过；
- [ ] `V2-FULL-DATA-PLANE-03`：续期时替代 control/Relay/P2P 身份且 P2P socket 不
  rebind；
- [ ] `V2-FULL-DATA-PLANE-04`：协调服务、Relay、Agent 各自中断后的明确恢复行为。

### Linux 真实网络与容量

Gate group：`V2-FULL-LINUX`。

单机原生 TUN/route 生命周期 smoke harness 只是测试基础设施；仅存在 ignored test 或
harness 不能关闭 Gate。实际执行和清理结果由 `V2-B5-TUN-CLEANUP-01` 判定。
本组还要求上方基线表的 `V2-B4-STATE-256-01` 关闭，不在此复制其定义或状态。额外
Gate 为：

- [ ] `V2-FULL-LINUX-01`：Linux network namespace 中的完整 v2
  enrollment/control/Relay/P2P ping、TCP、UDP；
- [ ] `V2-FULL-LINUX-02`：Server 重启、Agent 重启、路由回滚、丢包、乱序、MTU 黑洞
  和恢复；
- [ ] `V2-FULL-LINUX-03`：至少 30 分钟连接抖动 soak，检查 RSS、任务、线程、文件
  描述符和 queue。完整 256 在线 Agent soak 留到规模加固阶段。

`tests/e2e/run-real-tun.sh linux all` 必须在任一规定命名测试缺失、跳过或失败时
fail-closed；失败结果不是通过证据。任何文档或 Release Notes 都不得声称这些测试已
执行成功，除非对应 commit 有可审计 CI/runner 结果。

## 交接与权威边界

本文到 v2 验收为止，不再复制后续版本表：

- 当前、Proposed 和后续方案总览见 [`design-overview.md`](design-overview.md)；
- 条件版本顺序只由 [`roadmap.md`](roadmap.md) 维护；
- Proposed v3 的身份、PKI、地址与 `V3-P0..V3-P6` 只由
  [`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 维护；
- ADR 0004 接受时不发布稳定 v2；ADR 拒绝时才继续完成本文全部 v2 门禁。
