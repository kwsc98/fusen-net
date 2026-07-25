# ADR 0002：QUIC 传输抽象边界

- 状态：Superseded for runtime selection by [ADR 0003](0003-coordinator-p2p-relay-fallback.md)
- 日期：2026-07-20

## 背景

Quinn、s2n-quic 和 gm-quic 的 API、Datagram buffer 和关闭语义不同。核心网络逻辑
需要可测试的抽象边界，但“依赖存在”与“可由当前运行时选择”是不同承诺。

## 当前决策

- 保留对象安全的 endpoint/connection 抽象，以及三个独立 Cargo feature 的编译检查。
- Stellaris v2 Server、Agent、Relay 和 P2P 运行时固定使用 Quinn。
- schema v2 不含 backend 字段，三个 Server UDP listener 表示协议用途而非传输实现。
- s2n-quic 和 gm-quic 的 v2 endpoint 请求显式返回不支持，不能隐式回退或降级。

## 后果

传输抽象仍帮助单元测试和未来评估，但 `all-backends` 只表示编译覆盖，不表示运行时
可切换或稳定支持。若未来增加其他 v2 QUIC 实现，必须有新的 ADR、协议互操作边界和
独立真实网络门禁。
