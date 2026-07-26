# 故障排查

> **文档适用性：Current；适用范围：v2；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**

先记录二进制版本、平台和非秘密错误，再校验配置：

```bash
stellaris --version
stellaris config check --config /etc/stellaris/agent.toml
RUST_LOG=stellaris=debug stellaris agent run --config /etc/stellaris/agent.toml
```

Issue 中删除 token/摘要、私钥、证书序列号、CSR、内部地址、完整控制 payload 和用户
包内容。当前 macOS/Windows 仅为编译目标；运行问题不应先假设已有平台门禁覆盖。

## Server 状态未初始化

- 全新部署必须先运行 `stellaris server init --config ...`，再运行 `server run`。
- `server init` 不覆盖已有状态。仅当节点 CA certificate/key 同时存在且严格校验有效、
  协调状态缺失时，命令会沿用该 CA 完成中断的初始化；其他不完整或无效组合均拒绝。
- 普通启动不会自动生成缺失状态。检查配置中三个状态路径是否不同、父目录权限是否
  安全、运行用户是否一致。
- `config check`、`server init` 和 `server run` 必须使用拥有 service key、节点 CA 和
  协调状态的同一 effective UID；不要由 root 初始化后切换到其他 Server 用户运行。
- 单独 CA key/certificate、损坏或不匹配 CA、已有协调状态但 CA 不完整时，不要通过
  删除剩余文件反复初始化。先保留现场并从一致备份恢复，或在确认没有任何需要保留的
  Agent identity 后重新建设整个信任域。
- 若错误表示持久 node/address binding 与静态注册表冲突，恢复已 enrollment 节点原来的
  `id`/`ipv4`。v2 不支持靠修改 `nodes.toml` 原地改绑地址；需要新地址时只能规划全新
  v2 信任域并重新 enrollment 受影响部署，不能手工删除单条协调状态。

## UDP listener 无法绑定

- 检查 `[listeners]` 的 `enrollment`、`control`、`relay` 都是非零 IPv4 UDP 地址。
- 三者不能相同；同端口的通配 bind 也会和具体本地地址冲突。
- 用 Linux `ss -lunp`、macOS `lsof -nP -iUDP` 或 Windows
  `Get-NetUDPEndpoint` 检查占用。
- 这些端口分别是协议用途，不是 backend 选择；v2 运行时只使用 Quinn。

## TLS 或 ALPN 失败

- Agent `server_name` 必须匹配 service certificate SAN；连接 IP 或文件名不能替代。
- `deployment_ca_file` 必须信任 service certificate 的签发 CA，不是节点 CA。
- enrollment 只认证 Server；control/Relay 还要求 Agent 节点证书。
- P2P 双方信任 enrollment 下发的节点 CA，并校验 descriptor 指纹和签名 node/IP。
- 检查系统时间、证书有效期、PEM 链顺序和 UDP 双向可达性。
- 确认连接到了正确用途的端口；四条链路的 ALPN 不可互换。

Stellaris 没有跳过证书验证、ALPN 降级或 insecure 模式。

## Enrollment 被拒绝

- `node_id` 必须与节点表完全一致且 `enabled = true`。
- Agent 文件保存 `stl2_` 明文 token，节点表保存命令打印的 SHA-256 摘要；二者不能
  互换。
- 节点表仅在 Server 启动时加载，修改摘要后必须重启。
- token 一次成功后即消费。相同 enrollment ID/token/CSR 的持久重试可以得到同一
  结果；删除 Agent pending identity 后用新 ID 重放 token 会被拒绝。
- 运行 `config check` 和 `agent run` 的 effective UID 必须与 token/identity owner 一致；
  先以普通用户校验再使用 `sudo` 启动通常会被权限策略拒绝。
- identity 目录已有其他 node ID、损坏证书或权限过宽时会 fail-closed。

鉴权响应不会区分 node 不存在和 token 错误。不要在日志或 Issue 中粘贴 token；需要
更换私钥时按部署指南执行完整 token/SPKI 轮换。

## Control 或 Relay 反复重连

- 节点证书必须未过期，签名 node ID/IP 必须与 enabled 注册项相同，SPKI 必须仍是
  协调状态中授权值。
- Relay 必须绑定当前 control session 和 incarnation；control 被替换会主动关闭旧
  Relay。
- `ControlWelcome` 中 overlay、MTU、IP 和证书期限必须与本地身份一致。
- 检查 control/Relay UDP 端口分别可达，不能把两个地址指向同一用途 listener。
- 续期失败会按退避重试；持续失败直到 `NotAfter` 会关闭现有会话。

## P2P 未建立但 Relay 可用

这是可接受的回退状态。依次检查：

1. 两个 Agent 的 `p2p.bind` 在 LAN 防火墙中双向可达；只有共享同一宿主 IP 时才必须
   使用不同端口，不同主机可以使用相同 UDP 端口。
2. 日志/指标中两端均已发布 IPv4 host candidate，并收到同一 connection plan。
3. candidate 不能是 wildcard、loopback、multicast、broadcast 或 IPv6。
4. 两端节点证书、descriptor 指纹、session/incarnation 和 plan 期限仍有效。
5. 没有 NAT 或端口映射阻断直连；当前版本不做打洞、地址观察或 STUN。

还要确认已发布 candidate 是宿主机 underlay 地址，而不是 Stellaris TUN/overlay 地址。
当前枚举尚未自动证明这一点；P2P 路径计数只表示节点间 QUIC Ready，不足以证明流量
没有通过 Relay overlay 承载。

P2P send 失败时当前包按设计丢弃，不会补发 Relay；后续包才回退。偶发单包丢失不能
直接判断为路径切换故障。

## TUN 或 overlay 不通

- Linux 检查 `/dev/net/tun`、`iproute2` 的 `ip` 命令，并授予 root 或
  `CAP_NET_ADMIN`；容器还要映射设备。
- `tun_name` 不能和已有接口冲突；省略可让平台分配名称。
- overlay IP/CIDR/MTU 来自 Server，不在 Agent 配置中。检查节点表地址唯一且可用。
- 查看宿主机是否存在指向 TUN 的 overlay 路由，以及是否有更具体的冲突路由。
- 检查源地址、目标地址、IPv4 长度、MTU、queue full 和 unavailable path 丢包计数。
- Stellaris 不修改默认路由或 DNS，普通 Internet 流量不经过 overlay 是预期行为。

不要用 Docker `--privileged` 或关闭证书校验作为长期排障手段。清理残留资源时只删除
能够明确归属于本次 Stellaris 实例的 TUN 和 overlay 路由，不要批量清空系统路由。
