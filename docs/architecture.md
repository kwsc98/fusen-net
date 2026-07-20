# 架构

本文定义 Fusen Net 0.1 的目标架构。实现偏离这里列出的安全不变量时，应视为
缺陷；后端和平台是否已经通过发布门禁，以
[`compatibility.md`](compatibility.md) 为准。

## 系统边界

0.1 是中心转发、单租户、IPv4-only 的三层 overlay。Relay 是控制面和数据面
汇聚点，Edge 是创建 TUN 的端节点。Relay 不创建 TUN，也不接管宿主机路由。

```text
                    +----------------------+
                    |        Relay         |
                    |  auth + RouteTable   |
                    |                      |
Edge A -- quinn ----| listener :7000/udp   |
Edge B -- s2n ------| listener :7001/udp   |
Edge C -- gm-quic --| listener :7002/udp   |
                    +----------------------+
```

每条 QUIC 链路的两端使用相同后端。多种后端在 Relay 内汇合到统一路由表，
因此 Edge A 和 Edge B 可以经 Relay 交换包，但 quinn 客户端不直接连接 s2n
监听。不同 QUIC 库的直接互操作不属于兼容承诺。

## 核心组件

| 组件 | 职责 | 不负责 |
| --- | --- | --- |
| `NodeRuntime` | 声明身份以及监听、拨号、转发和 TUN 能力组合 | 直接持有后端或启动任务 |
| `RelayRuntime` / `EdgeRuntime` | 校验所需能力并管理当前中心模式的连接和会话生命周期 | 提供 Hybrid CLI 或节点发现 |
| `TransportFactory` | 按后端创建客户端或服务端 endpoint | 身份鉴权、overlay 路由 |
| `Endpoint` / `Connection` | QUIC 连接、控制流、Datagram、关闭和远端地址 | 解析控制协议或 IPv4 策略 |
| `AddressAllocator` | 将已登记的 node ID 和 token 摘要映射到固定 IPv4 地址 | 动态 DHCP、跨租户地址分配 |
| `RouteTable` | 以 overlay IP 和 session ID 原子注册、查找和清理在线会话 | 修改宿主机路由 |
| `TunFactory` / `PacketDevice` | 创建平台 TUN，统一读写无平台头的 IPv4 包 | DNS 和非 overlay 路由 |
| `RouteManager` | 安装和回滚 Agent 宿主机的 overlay CIDR 路由 | 默认路由和 DNS |
| Control plane | 版本协商、注册、鉴权和 Ready 状态 | 用户流量转发 |
| Data plane | 校验并转发一个 Datagram 中的一个 IPv4 包 | 重传、分片或可靠传输 |

接口保持对象安全，使测试可以注入 Fake Transport、Fake TUN、静态地址分配器
和内存路由表。当前运行模式是：

- `Relay`：一个或多个监听、地址分配器和共享路由管理器；没有 TUN。
- `Edge`：一个出站连接和一个 TUN；不接受其他节点的入站连接。
- `Hybrid`：只预留组合边界，0.1 不提供该模式或相关配置。

## 建链流程

```text
Edge                  RouteManager                 Relay / RouteTable
 | QUIC + TLS/ALPN                                      |
 |----------------------------------------------------->|
 | Register(node, token)                                | verify binding
 |----------------------------------------------------->| reserve session
 | RegisterAccepted                                     |
 |<-----------------------------------------------------|
 | install overlay route  |                             |
 |----------------------->|                             |
 | RouteLease             |                             |
 |<-----------------------|                             |
 | Ready                                                |
 |----------------------------------------------------->| activate session
```

路由只有在 token 验证成功且收到 `Ready` 后才可见。重复 node ID 采用
`reject-new`：现有会话继续工作，新连接收到错误。每个连接使用随机 session ID；
断开时只能删除与自身 session ID 一致的路由，防止旧任务清理新连接状态。

Edge 在建链失败后使用带抖动的指数退避重连，基准从 1 秒增长到最多 30 秒。
程序创建的 TUN 和路由必须在正常退出或失败后 5 秒内回滚。清理失败应记录资源
名称和错误，但不能记录 token 或包内容。

## 数据流

发送路径：

1. Edge 从 TUN 读取一个平台帧，移除 Linux PI 或 macOS 地址族头等平台元数据。
2. Edge 校验其为不超过 MTU 的完整 IPv4 包，并作为一个 QUIC Datagram 发送。
3. Relay 校验 IPv4 头、总长度、源地址、目标地址和 overlay 范围。
4. Relay 通过目标 IP 查找 Ready 会话，将同一 Datagram 放入目标连接的发送队列。
5. 目标 Edge 再次校验目标地址，补充平台头并写入 TUN。

数据面不提供重传或排序，丢包语义与 IP 网络一致。应用需要可靠性时应在 overlay
上运行 TCP 或其他可靠协议。QUIC 控制流只承载注册消息，不承载用户包。

Relay 为每个目标会话使用容量 256 项的应用层队列。传输后端内部缓冲采用各自库的
有界模型：s2n 的 Datagram 收发队列分别为 256 项，Quinn 的收发缓冲分别采用
128 KiB 字节预算，所以 Quinn 的等效包数取决于 Datagram 长度。应用层队列或后端
缓冲没有空间时，按后端契约拒绝或丢弃新 Datagram，不阻塞其他会话，也不记录
包体。应用路由队列和后端发送失败会增加计数器；后端未暴露的接收淘汰计数仍是
稳定版前的可观测性缺口。断线、接收错误和关闭信号必须使生产者和消费者全部退出，
避免悬挂任务。

仓库内的 `qconnection` 和 `qunreliable` Apache-2.0 本地 fork 为 `gm-quic 0.4`
补齐 1-RTT Datagram 组包，并将 Datagram 收发队列分别固定为 256 项；
`qconnection` 补丁同时移除了接收路径上位于该限制之前的无界 Datagram 转交通道，
并把 ACK、CRYPTO、stream 和连接控制帧的分发队列限制为每类 256 项。可靠帧队列
满或消费者停止时，连接以错误关闭而不是静默丢帧或继续积压。gm-quic 的 Datagram
队列满时遵循发送拒绝、接收丢弃新 Datagram 的策略；真实背压和 soak 完成前仍只
具备预发布资格。

## 安全不变量

- TLS 必须验证 CA、SAN 和 `server_name`，ALPN 固定为 `fusen-net/1`。
- Relay 只保存 token 的 SHA-256 摘要，并使用常量时间比较。
- 未 Ready 会话不能发送或接收 overlay 包。
- 包源地址必须等于会话分配地址；目标必须是 overlay 内的在线单播地址。
- 拒绝 IPv6、广播、组播、overlay 外目标、格式错误和超 MTU 包。
- 禁用 0-RTT、TLS key log 和跳过证书验证的运行模式。
- 网络输入路径不允许因畸形输入触发 panic；应用层队列和后端 Datagram 缓冲均有界，
  但后端上限不使用统一的项数单位。
  稳定版仍要求真实 TUN 背压和长时间资源测试确认边界在运行环境中有效。

完整威胁模型见 [`security-model.md`](security-model.md)。

## 平台边界

- Linux：统一层移除/补充 TUN packet-information 头，只向数据面交付 IPv4 包。
- macOS：适配 utun 的 4-byte 地址族头，并只配置 overlay CIDR 路由。
- Windows：通过 Wintun 交换原始 IP 包，路由操作必须可追踪并可回滚。

平台适配层对上只暴露 `PacketDevice`，数据面不得包含平台条件分支。

## 演进方向

稳定 node ID、角色中立运行时以及可替换的地址/路由接口是未来分布式组网的扩展
点，但不构成 mesh 承诺。引入监听与拨号并存、节点发现、路由通告或 NAT 穿透前，
必须新增 ADR 和威胁模型。
