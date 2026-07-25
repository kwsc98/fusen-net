# ADR 0003：协调服务、按需 P2P 与 Relay 回退

- 状态：Accepted and implemented; verification gates incomplete
- 日期：2026-07-25

## 背景

只依赖中心 Relay 会形成带宽瓶颈和单实例可用性依赖。能够直接互通的节点应使用
P2P，但仍需要可验证短期身份、节点发现、重复连接仲裁、单路径切换和确定性回退。

Accepted 表示架构方向已经确定；implemented 表示相应 v2 运行时代码已接入 CLI。
两者都不等于真实 TUN、故障注入、容量或 soak 发布门禁已经通过。

## 决策

### 角色和部署边界

- `server` 是单实例协调服务与可信 Relay 的组合，负责静态 enrollment、短期证书、
  在线目录、host candidate 交换、connection plan 和回退转发。
- 节点继续使用静态 node ID、一次性 token 和唯一 overlay IPv4。最多 256 个节点，
  不增加动态地址或管理 API。
- Agent 在本地 TUN 与一个 Quinn Hybrid P2P endpoint 之间管理按目标地址的路径。
- v2 运行时只使用 Quinn。其他 QUIC 依赖和抽象保留编译检查，但配置不能选择。

### 信任与协议

- 部署 service certificate 与节点 CA 分离。enrollment 使用部署 TLS、token 和 CSR
  proof-of-possession；control、Relay 和 P2P 使用节点证书 mTLS。
- `server init` 显式创建节点 CA 与协调状态，普通启动不自动生成或替换信任根。
- 节点证书有效期 24 小时并绑定 node ID、overlay IP、公钥和用途；续期必须保留公钥。
- enrollment、control、Relay、P2P 使用 `stellaris/enroll/2`、
  `stellaris/control/2`、`stellaris/relay/2`、`stellaris/p2p/2` 四个独立 ALPN。
- 协议不降级，0-RTT 不承载身份、状态变更或用户包；网络输入和队列有界。
- 具体线格式由 [`../protocol.md`](../protocol.md) 唯一定义。

### 路径状态机

```text
RelayOnly -> Probing -> P2PReady
     ^          |          |
     +----------+----------+
     failure / disconnect / change / expiry / idle
```

- `RelayOnly` 和 `Probing` 中，每个用户包只选择 Relay；探测在后台进行。
- P2P 完成 mTLS、descriptor 绑定和 Ready 后，路径按目标 overlay IP 原子切换。
- P2P send 失败的当前包丢弃；后续包回退 Relay，禁止补发同一个包。
- 同时建立的重复连接使用确定性仲裁只保留一个。应用数据空闲默认五分钟后回收。
- 候选、incarnation、撤销或证书期限变化立即使旧 P2P 失效。

### 当前候选范围

当前只发布和拨号可直接到达的 IPv4 host candidate。NAT 映射观察、
server-reflexive candidate、UDP 打洞和外部 STUN/TURN 不属于本决策的当前实现阶段，
将在独立 NAT 里程碑中设计和验证。

### 故障语义

- Relay 必须绑定当前 control lease，Ready 前不能路由；control 结束会关闭绑定 Relay。
- Relay 不可用时，已有 P2P 可以继续到断线或证书到期；没有 Ready P2P 的包丢弃。
- 协调服务不可用时不能 enrollment、续期、发现新 peer 或取得新 plan。
- 已有 P2P 不应仅因控制面短暂故障而复制用户包，但完整保持/恢复行为仍需 E2E 证明。
- 在线 session 不持久化，Server 重启后所有节点重新认证。

### 信任边界

- P2P 数据由节点间 QUIC mTLS 保护，不经过 Server 数据面。
- 协调服务和节点 CA 可以签发/发布身份，被攻破后能够冒充节点。
- Relay 回退是可信中继，能够看到完整 overlay 包和流量元数据。
- 系统单租户全互通，不提供 ACL 或租户隔离。

## 后果

局域网可直连节点能够绕过中心数据转发，同时保留 Relay-first 的确定性回退。代价是
证书生命周期、持久状态、连接仲裁和故障测试明显更复杂。

当前剩余门禁由 [`../distributed-network-plan.md`](../distributed-network-plan.md)
跟踪。在 LAN P2P、无重复包、Linux真实 TUN和资源 soak 全部具备可审计证据前，不能
把本 ADR 的实现状态描述为稳定支持。
