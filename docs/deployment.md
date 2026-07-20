# 部署指南

本指南面向 0.1 的中心 Relay 部署。开始前阅读
[`security-model.md`](security-model.md)；alpha/beta/rc 版本不应直接用于关键生产
流量。

预发布制品只通过托管 CI 和无特权测试，自动发布时会跳过三个真实 TUN 自托管门禁，
发布页会明确标为 prerelease。只有无预发布后缀且通过 Linux、macOS、Windows 真实
TUN job 的版本才具备稳定发布资格；实际平台支持仍以对应 Release Notes 为准。

三个后端都为 Datagram 缓冲设置了上限，但上限的单位不同：s2n 的收发队列分别为
256 项，Quinn 的收发缓冲分别采用 128 KiB 字节预算，后者可容纳的包数取决于包长。
`gm-quic 0.4` 通过仓库内 Apache-2.0 本地 fork 修复 1-RTT Datagram 组包，并将
底层收发队列分别固定为 256 项。gm-quic 发送队列满时拒绝新 Datagram，接收队列
满时丢弃新 Datagram，均不记录包体。该后端仍需通过真实 TUN、持续背压和 soak
门禁；在此之前只用于预发布测试和评估，不应承载关键或长期无人值守的生产流量。
本地 `qconnection` fork 也把 ACK、CRYPTO、stream 和连接控制帧的分发队列限制为
每类 256 项，满时关闭连接。部署层仍必须设置连接、CPU、内存和 UDP 速率限制。

## 拓扑和端口

Relay 至少暴露一个 UDP listener。不同后端使用不同 bind 地址或端口，例如：

```text
7000/udp  quinn
7001/udp  s2n
7002/udp  gm-quic
```

只开放实际配置的 UDP 端口，不需要开放同号 TCP 端口。Relay 不创建 TUN，也不
需要 `NET_ADMIN`。每个 Edge 需要访问对应 listener、读取 CA/token，并有创建
TUN 和 overlay 路由的权限。

## 证书和秘密

- 使用组织 CA 或受信任公开 CA 签发 Relay 服务端证书。
- 证书 SAN 必须包含 Agent 配置的 `server_name`；连接 IP 不替代 SAN 校验。
- `cert_file` 使用叶证书在前、后接中间证书的 PEM 链；`ca_file` 可以包含一个或多个
  受信任 CA 证书。
- 将 Relay 私钥和 Edge token 放在配置目录之外，Unix 权限设为 `0600`。
- Windows 启动时会读取秘密文件 ACL；只允许文件所有者、Administrators 和
  SYSTEM。ACL 无法验证或授权给其他主体时，启动失败。
- Windows Agent 必须让 Release 附带的 `wintun.dll` 与 `fusen-net.exe` 位于同一
  目录；运行时只从该绝对路径加载，并验证 DLL 的发布者签名。
- 每个 Edge 运行一次 `token generate`，只把摘要放到 Relay 注册表。
- 不把示例值、token、私钥或包含这些内容的环境变量写入镜像、Git 或日志。

更换信任根时，证书轮换应先把新 CA 加入 Agent 的 bundle，再替换 Relay 证书链，
确认所有 Agent 完成重连后才移除旧 CA。Token 轮换需要更新注册表摘要和对应 Edge
文件，并允许一次受控重连；0.1 不支持双 token 宽限期。

## 原生运行

安装 release 制品后先校验 checksum，再校验配置：

```bash
sha256sum --check SHA256SUMS
fusen-net config check --config /etc/fusen-net/server.toml
fusen-net server --config /etc/fusen-net/server.toml
```

`config check` 只做结构、路径、基本 PEM 标记和受支持权限检查，不验证证书链、有效期、
SAN 或证书/私钥匹配；Server endpoint 创建和 Agent TLS 握手仍可能因这些问题失败。

Relay 应以专用非登录、非 root 用户运行，并限制为读取配置、节点注册表和 TLS
文件。服务管理器应设置自动重启、文件描述符上限和正常退出超时，但不要把失败配置
变成无限快速重启。

Agent 在启动前校验配置：

```bash
fusen-net config check --config /etc/fusen-net/agent.toml
sudo fusen-net agent --config /etc/fusen-net/agent.toml
```

生产中优先通过 systemd、launchd 或 Windows Service 授予最小权限。Agent 只应
添加 overlay CIDR 路由，不应修改默认路由或 DNS。服务停止超时至少为 5 秒，以便
回滚接口和路由。

## Linux 容器

仓库镜像从 `deploy/docker/` 构建。Relay 容器不需要额外 capability：

```bash
docker run --rm --name fusen-relay \
  -p 7000:7000/udp \
  --cap-drop ALL \
  --security-opt no-new-privileges:true \
  -v "$PWD/server.toml:/etc/fusen-net/server.toml:ro" \
  -v "$PWD/nodes.toml:/etc/fusen-net/nodes.toml:ro" \
  -v "$PWD/secrets:/etc/fusen-net/secrets:ro" \
  -e FUSEN_NODES_FILE=/etc/fusen-net/nodes.toml \
  -e FUSEN_CERT_FILE=/etc/fusen-net/secrets/server.pem \
  -e FUSEN_KEY_FILE=/etc/fusen-net/secrets/server-key.pem \
  ghcr.io/kwsc98/fusen-net/server:v0.1.0-alpha.1 \
  server --config /etc/fusen-net/server.toml
```

Linux Agent 容器只授予 TUN 设备和网络管理 capability：

```bash
docker run --rm --name fusen-edge-a \
  --device /dev/net/tun:/dev/net/tun \
  --cap-drop ALL \
  --cap-add NET_ADMIN \
  --security-opt no-new-privileges:true \
  -v "$PWD/agent.toml:/etc/fusen-net/agent.toml:ro" \
  -v "$PWD/secrets:/etc/fusen-net/secrets:ro" \
  -e FUSEN_SERVER_ADDR=203.0.113.10:7000 \
  -e FUSEN_CA_FILE=/etc/fusen-net/secrets/ca.pem \
  -e FUSEN_TOKEN_FILE=/etc/fusen-net/secrets/edge-a.token \
  ghcr.io/kwsc98/fusen-net/agent:v0.1.0-alpha.1 \
  agent --config /etc/fusen-net/agent.toml
```

不得使用 `--privileged`。上面的镜像标签仅示例版本格式；以 GitHub Release 公布
的实际标签和 digest 为准，不在自动化部署中使用 `latest`。macOS/Windows 容器
不计入原生平台支持。

## 运行验证

1. Relay 日志显示 listener 已绑定，但不包含证书私钥或节点 token。
2. Edge 完成 TLS、注册、TUN 创建和 `Ready`，Relay 路由表显示对应 node ID/IP。
3. 从 Edge A 对 Edge B overlay IP 执行 100 次 ping，稳定版发布门禁要求零丢包。
4. 在 overlay 上分别验证 TCP 和 UDP 应用流量。
5. 重启 Relay，Agent 应在 30 秒内重连；停止 Agent 后 5 秒内不再存在程序创建的
   接口和路由。

## 可观测性

至少采集连接数、鉴权成功/失败、重连次数、活动路由数、Datagram 收发和队列溢出。
当前内建原子计数提供 route queue 与 transport 背压丢包；更细分类需要从脱敏结构化
日志聚合。日志应包含 node ID、session ID、后端和远端地址等诊断字段，但不包含
token、摘要、控制 payload、私钥或用户包内容。

公网 Relay 应在网络层设置速率和连接限制，并对异常鉴权失败、持续队列溢出和重连
风暴告警。0.1 没有管理 API；不要通过暴露调试端口代替正式监控。
