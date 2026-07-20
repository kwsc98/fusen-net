# 架构决策记录

ADR 记录难以从代码本身推导、且会影响协议、安全或长期模块边界的决策。

状态使用 `Proposed`、`Accepted`、`Superseded` 或 `Rejected`。接受后的 ADR 不原地
改写结论；需要改变决策时新增 ADR，并在双方文件中互相链接。

| ADR | 状态 | 决策 |
| --- | --- | --- |
| [0001](0001-central-relay-for-0.1.md) | Accepted | 0.1 采用中心 Relay、单租户 IPv4 overlay |
| [0002](0002-transport-backend-boundary.md) | Accepted | 每条链路同后端，Relay 多 listener 共享数据面 |
