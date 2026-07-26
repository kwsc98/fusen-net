# 兼容性与支持状态

> **文档适用性：Current；适用范围：平台与传输支持；设计评审状态：N/A；ADR 决策
> 状态：N/A；交付状态：Implemented；验证状态：Unverified；发布状态：
> Unreleased。**

<!-- stellaris-release-status:support-status:start -->
本文区分“代码存在”“可以编译”和“已经通过发布门禁”。当前版本是 `0.3.0-alpha.1`
早期预览，没有平台可据此标记为稳定生产支持。
<!-- stellaris-release-status:support-status:end -->

## 破坏式版本规则

- 线协议固定为帧版本 `2` 和四个用途专用 ALPN，不协商其他版本或降级。
- Server、Agent 和静态节点表只接受顶层 `version = 2`。
- 不提供配置迁移器、状态迁移器、双栈 listener、混合集群或兼容 feature。
- 旧配置、旧持久状态、旧 token 格式和旧 Agent 都必须重新生成/部署。
- 同一信任域的 Server、Agent 和配置应使用同一 release；`0.x` 仍可能包含破坏式变化。

## 协议

<!-- stellaris-release-status:protocol-status:start -->
| 链路 | `0.3.0-alpha.1` 实现 | 未完成门禁 |
| --- | --- | --- |
| enrollment | 部署 TLS、token + CSR、幂等持久签发 | 恶意输入、重启和故障注入 E2E |
| control | 节点 mTLS、Welcome、目录、计划、续期、撤销消息 | 长时间重连、到期和撤销 E2E |
| Relay | control lease bind、Ready 后原始 IPv4 Datagram | 双 Agent 真实 TUN、ABA、背压和故障恢复 |
| P2P | Quinn Hybrid endpoint、host candidate、mTLS Ready、路径回退 | 排除 TUN/overlay candidate、证明真实 underlay 直连、多节点 LAN、同时拨号、续期、空闲回收和无重复包 E2E |
<!-- stellaris-release-status:protocol-status:end -->

四条链路的 ALPN 和线格式只由 [`protocol.md`](protocol.md) 规定。NAT 穿透和
server-reflexive candidate 不属于当前协议运行范围。

## QUIC 实现

<!-- stellaris-release-status:transport-status:start -->
| Cargo feature | 构建状态 | v2 运行状态 |
| --- | --- | --- |
| `backend-quinn` | 默认依赖；当前 commit 无归档构建证据 | 唯一可用于 Server/Agent 的实现 |
| `backend-s2n` | 保留依赖与 CI 编译门禁；当前 commit 无归档结果 | 显式返回 v2 不支持，配置不可选择 |
| `backend-gm-quic` | 保留依赖、本地 patch 与 CI 编译门禁；当前 commit 无归档结果 | 显式返回 v2 不支持，配置不可选择 |
| `all-backends` | 配置为抽象层编译门禁；当前 commit 无归档结果 | 不表示运行时可切换后端 |
<!-- stellaris-release-status:transport-status:end -->

三个 Server UDP 地址分别表示 enrollment、control 和 Relay，不是 backend listener。
当前不承诺 Quinn 与其他 QUIC 库的 v2 互操作。

## 平台

<!-- stellaris-release-status:platform-status:start -->
| 平台 | 编译目标 | 原生 Agent/TUN | 当前支持等级 |
| --- | --- | --- | --- |
| Linux x86_64 | CI 目标已配置，当前 commit 无归档结果 | 首轮运行门禁目标 | 尚未完成真实 TUN、namespace、故障和 soak 证据 |
| macOS x86_64 | CI 目标已配置，当前 commit 无归档结果 | 代码保留 | compile-only 目标，运行未验证 |
| Windows x86_64 | CI 目标已配置，当前 commit 无归档结果 | 代码保留 | compile-only 目标，运行未验证 |
<!-- stellaris-release-status:platform-status:end -->

Linux 运行目标需要 `/dev/net/tun`、`iproute2` 提供的 `ip` 命令，以及 root 或
`CAP_NET_ADMIN`。macOS/Windows 的接口、路由、权限和完整 P2P 行为在真实环境证据
完成前不能列为支持。

## 网络边界

- Overlay：IPv4 单播；静态单地址；默认 MTU 1100。
- Server underlay：Agent 必须能双向访问三个 IPv4 UDP listener。
- LAN P2P：两端 `p2p.bind` 对应的真实 underlay host candidate 必须直接可达；支持升级
  必须有排除 TUN/overlay 地址的路由证据，P2P 指标不能单独证明已经绕过 Relay。
- NAT：Relay 可作为不可直连时的路径；当前不做打洞、映射观察或外部 STUN。
- Proxy：不支持 TCP、HTTP 或 SOCKS 代理作为 QUIC underlay。
- IPv6 underlay/overlay：当前配置和候选校验不支持。

确切验证结果只能来自对应 commit 的 CI 和 Release Notes。仓库中存在测试脚本不等于
该测试已经成功执行。
