# ADR 0002：QUIC 后端边界

- 状态：Accepted
- 日期：2026-07-20

## 背景

项目需要比较和支持 Quinn、s2n-quic、gm-quic。虽然它们实现 QUIC，具体 API、
Datagram 能力、关闭语义和扩展行为不同。把跨库直接互操作当成默认承诺会显著扩大
测试矩阵并掩盖实现差异。

## 决策

每条 Agent-Relay 链路必须在配置中选择相同后端。Relay 可以同时创建多个不同后端
listener，连接建立后都适配到对象安全的 `Connection` 接口，并共享同一个控制面、
路由表和数据面。

Quinn 是参考实现；核心库默认启用 `backend-quinn`。s2n-quic 和 gm-quic 分别由
独立 feature 启用，正式 CLI 使用 `all-backends`。三个后端必须通过相同的 TLS、
ALPN、控制流、Datagram、关闭、超时和错误契约测试。

## 后果

- Relay 可以在源、目标使用不同后端时转发包，而不要求两个 Edge 直接互操作。
- 每个后端故障被限制在 transport 适配层，协议和路由测试可以使用 fake transport。
- 稳定版仍被三个后端共同阻塞；任一契约测试失败都不能将该后端标为支持。
- 未来若承诺跨库直连兼容，需要新的 ADR 和明确的互操作测试矩阵。
