# 故障排查

先执行配置校验，并记录二进制版本、平台、后端和非秘密错误信息：

```bash
fusen-net --version
fusen-net config check --config /etc/fusen-net/agent.toml
RUST_LOG=fusen_net=debug fusen-net agent --config /etc/fusen-net/agent.toml
```

提交 Issue 前删除 token、token 摘要、私钥、证书序列号、内网主机名和用户包内容。

## Relay 无法监听

- 确认 `bind` 是有效本地地址，多个 `[[listeners]]` 没有重复 SocketAddr。
- 确认选择的 backend 已编译进当前二进制。
- 检查端口是否被其他进程占用，并确认开放的是 UDP 而不是 TCP。
- 非 root 运行 Relay 时避免需要特权的低端口，或由系统能力精确授权。

Linux 可用 `ss -lunp` 查看 UDP listener；macOS 可用 `lsof -nP -iUDP`；Windows
可用 `Get-NetUDPEndpoint`。这些命令可能需要管理员权限。

## TLS 或 ALPN 失败

- `server_name` 必须匹配服务端证书 SAN，不等于证书文件名或 Relay node ID。
- `ca_file` 必须包含签发服务端证书的 CA，而不是服务端私钥。
- 检查系统时间、证书有效期和证书链顺序。
- Agent backend 必须与目标 UDP listener 的 backend 相同。
- 确认中间防火墙/NAT 允许双向 UDP，并且没有把端口转发为 TCP。

Fusen Net 不提供跳过证书校验的排障选项。不要用永久关闭验证来定位证书问题。

## 注册被拒绝

- node ID 必须与 `nodes_file` 完全一致，区分大小写。
- Agent token 文件是生成器创建的明文 token；Server 注册表保存的是其 SHA-256
  摘要，两者不能互换。
- 每个 node ID 同时只允许一个会话。`duplicate_node` 时先确认旧 Agent 是否仍在
  运行或处于重连状态。
- 修改注册表后按当前版本要求安全重载或重启 Relay，不能假设文件会自动热加载。

鉴权失败对外使用统一错误，Relay 不会说明 node ID 还是 token 错误。不要把 token
粘贴到 Issue 或日志中求助，应在本地重新生成并轮换。

## Agent 无法创建 TUN

- Linux：确认 `/dev/net/tun` 存在，并授予 root 或 `CAP_NET_ADMIN`。容器需同时
  使用 `--device /dev/net/tun` 和 `--cap-add NET_ADMIN`。
- macOS：确认进程有创建 utun 和路由的权限，检查请求的固定名称是否被平台忽略。
- Windows：以管理员身份运行，确认 Wintun 驱动可用且体系结构匹配。
- 检查 `tun_name` 是否与现有接口冲突；省略该字段让平台分配名称可帮助定位问题。

不要使用 Docker `--privileged` 作为长期修复。

## 已连接但 overlay 不通

1. 确认两端日志均已进入 `Ready/Active`，Relay 存在两个活动路由。
2. 检查分配 IP 唯一、属于同一 `overlay_cidr`，目标不是网络/广播地址。
3. 检查宿主机存在指向 TUN 的 overlay CIDR 路由，且没有更具体的冲突路由。
4. 查看源地址伪造、overlay 外目标、包格式和超 MTU 的脱敏日志，以及队列/transport
   溢出计数。
5. 将应用包控制在配置 MTU 内；路径 MTU 黑洞时先用较小 ping payload 验证。
6. 检查 Edge 本机防火墙是否允许 TUN 接口上的 ICMP/TCP/UDP。

Fusen Net 不修改默认路由或 DNS，所以普通 Internet 流量不经过 overlay 是预期
行为。QUIC Datagram 允许丢包和乱序，少量 underlay 丢包不会由 Fusen Net 重传。

## 退出后残留接口或路由

记录退出原因、接口名和路由后，可使用平台原生命令确认资源的创建者。只删除可明确
归属于 Fusen Net 的 overlay 路由或 TUN；不要批量清空系统路由。复现时检查正常
SIGINT、TLS 失败、Relay 重启和进程崩溃四种路径。

正常退出或可控失败后资源应在 5 秒内回滚。若未回滚，请提交包含平台、版本、配置
的非秘密部分和相关日志的 Issue。
