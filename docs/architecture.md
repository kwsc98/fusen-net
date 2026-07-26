# 架构

> **文档适用性：Current；适用范围：v2；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**

本文描述 Stellaris `0.3.0-alpha.1` 的破坏式 v2 架构。代码已经包含 Quinn 协调、
可信 Relay 和局域网 P2P 运行时；这表示实现边界，不表示真实 TUN、故障恢复或
soak 发布门禁已经通过。实际验证状态见 [`compatibility.md`](compatibility.md)。

## 系统边界

Stellaris 是单租户、单信任域、IPv4-only 的三层 overlay。Server 是单实例协调
服务与可信 Relay 的组合；Agent 创建本地 TUN，同时建立出站连接并在一个 Quinn
UDP socket 上监听/拨号 P2P。

```text
                         +--------------------------------+
                         |             Server             |
                         | enrollment / identity / state  |
                         | control directory / Relay      |
                         +------+-----------+-------------+
                         :7000  :7001       :7002
                           |      |            |
                           |      +-----+------+  Relay fallback
                           |            |
                     +-----+-----+  +---+-------+
                     |  Agent A  |==|  Agent B  |  preferred LAN P2P
                     | TUN :7100 |  | TUN :7200 |
                     +-----------+  +-----------+
```

Server 的 enrollment、control 和 Relay 使用三个互不冲突的 UDP bind。端口不是
QUIC 后端选择器；v2 运行时固定使用 Quinn。Agent 的 `p2p.bind` 是第四类 endpoint，
同一 socket 同时接受和发起 P2P 连接，并可在证书续期后更新 TLS 配置而不重新绑定。

## 核心组件

| 组件 | 职责 |
| --- | --- |
| `StaticNodeRegistry` | 只读加载 node ID、静态 overlay IPv4、enrollment token 摘要和 enabled 状态 |
| `CoordinatorStore` | 持久化 incarnation、撤销、已消费 token、授权 SPKI 和幂等 enrollment 结果 |
| `NodeIdentityStore` | 在 Agent 本地管理 P-256 私钥、待完成 enrollment 请求、节点证书和节点 CA |
| `ServerRuntime` | 绑定三类 listener，完成 enrollment、控制目录、连接计划和可信 Relay |
| `AgentRuntime` | 管理身份、control/relay 重连、TUN 生命周期、P2P endpoint 和路径切换 |
| `AuthenticatedConnection` | 将确定的 ALPN 和已验证 peer certificate 封装为认证后的强类型连接 |
| `HybridQuinnEndpoint` | 在同一 UDP socket 上监听、拨号并热更新节点 TLS 身份 |
| `PeerManager` | 按目标 overlay IP 保存 descriptor、候选、证书期限、拨号动作和当前路径 |
| `RouteTable` | 仅为已经 Ready 且仍绑定当前 control lease 的 Relay 会话路由 |
| `PacketValidator` | 校验 IPv4 长度、MTU、overlay 范围以及认证身份对应的源/目标地址 |

在线 control、Relay 和 P2P session 不写入持久快照。Server 重启后所有在线节点必须
重新认证；已有 P2P 连接的重启保持行为仍属于待完成的故障门禁，不能假设已经验证。

## 身份与信任根

Stellaris 分离两个信任根：

- **部署 TLS CA** 由部署者管理，签发 Server 的 service certificate。Agent 通过
  `deployment_ca_file` 和显式 `server_name` 验证 Server。该服务证书用于三个
  Server listener。
- **节点 CA** 由 `stellaris server init` 创建并持久化。它签发 24 小时节点证书，
  用于 control/Relay 客户端认证以及 P2P 双向认证。Agent 在 enrollment 成功时取得
  节点 CA 证书，但永远不会取得 CA 私钥。

节点证书把 node ID、静态 overlay IPv4、公钥和用途写入签名身份。TLS 握手完成后，
Server 还会将证书中的身份与静态注册表及当前授权 SPKI 比对；任何应用消息都不能
重新声明或覆盖连接身份。

首次 enrollment 流程：

```text
Agent                           Enrollment listener                  Durable state
  | deployment TLS + ALPN                 |                               |
  | EnrollRequest(id, node, token, CSR)   | verify token + CSR PoP        |
  |-------------------------------------->| copy next state, persist ---->|
  | EnrollAccepted(cert, node CA, network)|<------------------------------|
  |<--------------------------------------| reply only after commit        |
```

Agent 持久化 enrollment ID、token 和 CSR，以便相同请求安全重试。Server 对相同
enrollment ID、token 和 CSR 返回已提交结果；相同 ID 对应不同内容时拒绝。成功提交
后 token 被消费。管理员在静态节点表中轮换 token 并重启 Server 后，新的 enrollment
可以替换授权 SPKI，旧私钥及旧证书不能再次建立受授权会话。

## 控制与 Relay

持有效节点证书的 Agent 先连接 control listener。Server 验证节点 CA、证书用途、
有效期、签名身份、静态注册表和授权 SPKI，然后增加持久 incarnation，建立随机
control session，并发送包含 overlay CIDR、MTU 和证书期限的 `ControlWelcome`。

Relay 是独立 mTLS 连接：

1. Agent 发送 `RelayBind(control_session_id, incarnation)`。
2. Server 要求该身份仍持有当前 control lease，返回 `RelayAccepted`。
3. Agent 校验 MTU/Datagram 上限并发送 `RelayReady`。
4. 只有 Ready 后，Server 才把该会话放入路由表。

旧 session 清理只删除自身拥有的 route，不能清理替代 session。control lease 结束、
证书到期或连接断开时，Relay 路由立即失效。Relay 收到数据包后校验源地址等于已认证
节点的静态地址；目标必须是 overlay 内另一个 Ready 节点。

## 按需 P2P 和路径选择

Agent 启动时在 `p2p.bind` 创建 `HybridQuinnEndpoint`，向协调服务发布可用 IPv4
host candidate。本阶段不发布或使用 server-reflexive candidate，也不做打洞。

当前枚举发生在 TUN 创建后，且实现尚未排除 overlay/TUN 地址。协议中的 `host` 只
表示候选类型，不证明接口来源；在完成过滤和真实 underlay 路由门禁前，P2P Ready
只能证明节点间 QUIC 路径建立，不能证明它没有经 Relay 承载。

当 TUN 包指向尚无 Ready P2P 的目标时：

1. 当前包选择 Relay，并异步发送 `ConnectRequest`。
2. Server 向两端发送同一 `ConnectPlan`，包含角色、connection ID、peer identity、
   certificate fingerprint、incarnation、control session、证书期限和 host candidates。
3. 两端按计划同时准备入站并尝试拨号。P2P 使用节点 CA mTLS；握手还必须匹配
   descriptor 指纹、session、incarnation 和 connection ID。
4. `P2pReady` 完成后，`PeerManager` 对该目标原子切换为 P2P。

每个出站包只调用一次路径选择。若 P2P queue/send 失败，当前包丢弃并使路径回退；
不得把同一个包补发到 Relay。后续包再选择 Relay。P2P 入站包必须满足：

- 源地址等于已认证 peer 的 overlay IPv4；
- 目标地址等于本 Agent 的 overlay IPv4；
- 完整 IPv4 长度一致且不超过协调服务提供的 MTU。

P2P 断开、撤销、候选或 incarnation 变化、证书到期都会立即使后续流量回退 Relay。
默认连续 5 分钟没有应用数据选择该路径时回收连接；keepalive 不刷新该期限。

## 持久状态与失败语义

所有长期状态都必须先持久化再对外确认。目标写入顺序是“生成下一状态、写临时文件并
fsync、原子替换、fsync 目录、提交内存、应答”。实现包含相应 store 与原子替换路径，
但各 fsync/rename 故障点的完整注入门禁尚未通过。

协调服务不可用时，新 enrollment、证书续期、连接计划和 Relay 回退不可用。已建立
P2P 的连接不应仅因 control 暂时断开而主动重复包，但其完整保持与恢复语义仍需 E2E
验证；证书到期后连接必须关闭。

## 不在本阶段

- NAT 穿透、地址观察、server-reflexive candidate、外部 STUN/TURN；
- 多协调实例、共识、状态复制和自动故障转移；
- Relay 路径端到端加密；
- ACL、多租户、动态地址、管理 API、IPv6、DNS、默认路由和子网路由；
- s2n-quic 或 gm-quic 的 v2 运行路径。

安全边界见 [`security-model.md`](security-model.md)，线格式见
[`protocol.md`](protocol.md)。

当前 v2、Proposed v3 和后续方向的关系见 [`design-overview.md`](design-overview.md)。
不可变 NodeUID、动态地址租约和离线 Root/在线 Intermediate 节点 CA 是 Proposed v3；
多信任域是 Conditional 后续方向。二者都只在
[`node-identity-trust-addressing-plan.md`](node-identity-trust-addressing-plan.md) 中记录，
不属于本 v2 架构。
