# Fusen Net 线协议 v1

本文是 Fusen Net 0.1 线协议的规范。关键字“必须”“不得”“应该”和“可以”按
RFC 2119 的约定理解。

## QUIC 参数

- ALPN：`fusen-net/1`。
- TLS：1.3，由客户端验证服务端证书、SAN 和配置的 `server_name`。
- 0-RTT：禁用。控制注册和 Datagram 均不得使用 early data。
- 控制面：客户端建立的第一条双向流。
- 数据面：QUIC Datagram；用户 IPv4 包不得放入可靠流。
- 同一链路两端必须配置相同后端；v1 不承诺跨 QUIC 库直接互操作。

无法协商 ALPN、Datagram 能力或足以容纳配置 MTU 的 Datagram 大小时，连接必须
失败，不能静默改用流或拆分一个 IP 包。

## 控制帧

所有多字节整数使用网络字节序。固定 12-byte 头如下：

```text
0               4       6       7       8              12
+---------------+-------+-------+-------+----------------+
| magic "FNET"  | ver   | type  | flags | payload_length |
+---------------+-------+-------+-------+----------------+
   4 bytes        u16     u8      u8          u32
```

- `magic` 必须为 ASCII `FNET`。
- `ver` 在本规范中必须为 `1`。
- `flags` v1 必须为 `0`；非零值必须拒绝。
- `payload_length` 不包含 12-byte 头，最大为 16384 bytes。
- payload 必须是 UTF-8 JSON 对象。未知字段、重复字段和类型不匹配必须拒绝。
- 实现必须正确处理流分片和多个连续帧，读取声明长度前先执行上限检查。

消息类型：

| 值 | 名称 | 方向 | Payload |
| --- | --- | --- | --- |
| `0x01` | `Register` | Edge -> Relay | 注册身份和 token |
| `0x02` | `RegisterAccepted` | Relay -> Edge | 分配地址及 session |
| `0x03` | `Ready` | Edge -> Relay | TUN 和路由已就绪 |
| `0x04` | `Error` | 双向 | 协议或策略错误 |

### Register

```json
{"node_id":"edge-a","token":"fsn1_<base64url-encoded random value>"}
```

- `node_id` 必须与静态注册表完全匹配，第一个字符是 ASCII 字母或数字，后续可
  使用 ASCII 字母、数字、`.`、`_` 和 `-`，长度 1 至 63。
- `token` 是 token 文件中的 `fsn1_` 前缀值，其随机部分包含 256-bit 熵并使用
  无填充 base64url 编码。
  它只允许出现在经验证的 TLS 控制流中，不能写入日志。

### RegisterAccepted

```json
{
  "overlay_ip": "10.88.0.2",
  "overlay": "10.88.0.0/24",
  "mtu": 1100,
  "session_id": "7d444840-9dc0-4a2b-b7b6-39776a78b3bb"
}
```

`session_id` 是 Relay 为本次连接生成的随机 UUID，不能跨重连复用。
`overlay_ip` 必须是注册表绑定的可用单播 IPv4 地址，且位于 `overlay` 内。

### Ready

```json
{}
```

Edge 只有在成功创建 TUN 并安装 overlay 路由后才发送 `Ready`。Relay 在收到并
验证该消息前不得激活会话路由。

### Error

```json
{"code":"authentication_failed","message":"registration rejected","retryable":false}
```

v1 错误码如下。`message` 用于诊断，不应包含 token、摘要、注册表内容或内部路径。

| code | 含义 |
| --- | --- |
| `authentication_failed` | node ID 或 token 不匹配 |
| `duplicate_node` | node ID 已有在线/保留会话 |
| `invalid_request` | 可解析但字段或请求无效 |
| `protocol_violation` | 帧或状态机违反 v1 规则 |
| `server_busy` | Relay 暂时无能力接受会话 |
| `internal` | 未向对端暴露细节的内部失败 |

`retryable` 明确表示 Agent 是否可以对该错误自动重试；鉴权和永久协议错误必须为
`false`。不可信客户端不能用该字段要求 Relay 重试。

发送致命 `Error` 后应关闭控制流和连接。鉴权失败应返回统一消息，避免区分 node ID
不存在与 token 错误。

## 状态机

```text
Connected -> Register received -> Reserved -> Ready received -> Active
     |              |               |              |
     +--------------+---------------+--------------+--> Error/Closed
```

- 客户端必须先发送且只发送一次 `Register`。
- Relay 验证静态绑定后保留 node ID 并返回 `RegisterAccepted`。
- Edge 配置 TUN/路由，随后发送一次 `Ready`。
- 未进入 `Active` 前收到 Datagram 时 Relay 必须丢弃且不得路由；收到非预期控制
  消息时必须返回协议错误并关闭连接。
- 同一 node ID 已被保留或激活时，新连接得到 `duplicate_node`；旧连接不受影响。
- 连接关闭时，Relay 只清理由该连接 `session_id` 拥有的路由和保留项。

## Datagram

每个 QUIC Datagram 必须恰好包含一个完整 IPv4 包，从 IPv4 版本/IHL 字节开始，
不得包含 Linux PI、macOS utun 地址族头、以太网头或额外 framing。

发送和接收时必须验证：

1. 长度至少覆盖 IPv4 基本头，version 为 4，IHL 和 total length 合法。
2. IPv4 `total_length` 与 Datagram 长度完全一致，且不超过协商 MTU；v1 MTU
   范围为 576..=1100，保证包连同 QUIC 开销可放入最小路径 MTU。
3. Edge 发出的源地址等于其 `RegisterAccepted.overlay_ip`。
4. 目标位于 `overlay`，是非网络地址、非广播、非组播的单播地址。
5. Relay 路由目标为一个处于 `Active` 状态的会话。

不符合条件的 Datagram 直接丢弃，只记录脱敏原因，不返回或记录用户包内容。Relay
为每个目标会话设置容量 256 项的应用层路由队列。传输后端的内部 Datagram 缓冲也
必须有界，但计量方式并不统一：s2n 和 vendored gm-quic 的收发队列分别为 256 项，
Quinn 的收发缓冲分别使用 128 KiB 字节预算，因此能容纳的包数随 Datagram 长度变化。
应用队列或后端发送缓冲没有空间时，按对应契约拒绝或丢弃新 Datagram，并增加可见
的队列或 transport 丢弃计数。部分后端不会向应用暴露接收缓冲淘汰计数，该指标与
更细的校验原因聚合仍是稳定版前的可观测性缺口，不属于线协议。协议不增加序号、
确认、重传或分片，保留普通 IP 网络的丢包和乱序语义。

## 版本兼容

头部版本是完整协议版本，不进行隐式降级。收到非 v1 帧时拒绝并关闭连接；在能够
安全返回结构化错误时可以使用 `protocol_violation`。`0.x` 发布可能以新协议版本引入不兼容变化，
客户端和服务端应使用同一发布系列。详细策略见
[`compatibility.md`](compatibility.md)。
