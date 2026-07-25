# 架构决策记录

ADR 一经接受不直接重写历史结论；被新决策替代时标为 Superseded。Accepted 或
implemented 不等于发布门禁通过，当前验证状态仍以兼容矩阵和对应 Release 为准。

| ADR | 状态 | 决策 |
| --- | --- | --- |
| [0001](0001-central-relay-for-0.1.md) | Superseded | 中心 Relay 历史基线；由 ADR 0003 替代 |
| [0002](0002-transport-backend-boundary.md) | Superseded for runtime selection | 保留传输抽象；v2 runtime 固定 Quinn |
| [0003](0003-coordinator-p2p-relay-fallback.md) | Accepted, implemented; gates incomplete | 协调服务、可信 Relay、按需 LAN P2P 和单路径回退 |

新增协议、安全边界、信任根、持久状态或跨节点故障语义时，应先增加 ADR，再同步架构、
协议、安全模型、配置、测试门禁和支持矩阵。
