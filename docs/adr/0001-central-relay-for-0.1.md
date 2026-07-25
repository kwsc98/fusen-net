# ADR 0001：中心 Relay 基线

- 状态：Superseded by [ADR 0003](0003-coordinator-p2p-relay-fallback.md)
- 日期：2026-07-20

## 背景

项目最初需要先验证 QUIC Datagram、TUN、静态 IPv4 身份和中心转发。中心 Relay
提供单一在线路由事实，也构成带宽、可用性和明文回退流量的信任边界。

## 历史决策

早期基线把控制与数据转发集中到一个 Relay，端节点只拨号并创建 TUN。该决策曾用于
建立数据包校验、session 所有权和故障测试边界。

## 替代决策

当前架构不再采用“所有流量只经中心 Relay”的运行模型。ADR 0003 以协调服务、可信
Relay 回退和按需 LAN P2P 替代本决策；当前公共接口、协议和实现以 ADR 0003、
[`../architecture.md`](../architecture.md) 和 [`../protocol.md`](../protocol.md) 为准。

中心 Relay 仍作为确定性回退路径和信任边界保留，但不再是唯一数据路径。本文件只
保存决策历史，不定义兼容行为或可选旧运行模式。
