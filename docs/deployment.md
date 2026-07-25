# 部署指南

本指南面向 Stellaris `0.3.0-alpha.1` 的全新 v2 部署。当前版本未通过完整 Linux
真实 TUN、故障注入和资源 soak 门禁，只适合隔离测试环境。不要尝试复用任何旧配置、
状态、token 或节点 identity。

## 拓扑与端口

单实例 Server 同时提供协调服务和可信 Relay：

```text
7000/udp  enrollment  deployment TLS + token/CSR
7001/udp  control     deployment TLS + node client certificate
7002/udp  relay       deployment TLS + node client certificate
7100/udp  Agent A P2P node-to-node mTLS (example)
7200/udp  Agent B P2P node-to-node mTLS (example)
```

实际端口来自配置。Server 三个 bind 必须互不冲突，均使用 Quinn；不要开放同号 TCP。
P2P 只使用可直接到达的 IPv4 host candidate，不包含 NAT 打洞。无法直连时流量继续走
可信 Relay。

Server 不创建 TUN，也不需要 `NET_ADMIN`。Linux Agent 需要 `/dev/net/tun` 和 root
或 `CAP_NET_ADMIN`。macOS/Windows 当前仅为编译目标，不应按本指南宣称运行支持。

## 准备信任材料

部署者先提供：

- 由组织/公开部署 CA 签发的 service certificate，SAN 匹配配置 `server_name`；
- 对应 service private key；
- Agent 可读取的部署 CA PEM bundle；
- 每节点独立的一次性 enrollment token 及其静态注册表摘要。

service certificate 用于 enrollment、control 和 Relay 三个 listener。节点 CA 由
`server init` 单独创建，不能用部署 CA 私钥替代，也不要把节点 CA 私钥分发给 Agent。

```bash
install -d -m 0700 /etc/stellaris/secrets /var/lib/stellaris
stellaris token generate \
  --node-id edge-a --output /etc/stellaris/secrets/edge-a.token
```

将打印的摘要写入 `nodes.toml`，不要写入 token 明文。Unix 上所有私钥、token 和
协调状态必须由运行用户所有且 group/other 不可访问。

## 初始化 Server

准备好 `server.toml`、节点表和部署 TLS 文件后：

```bash
stellaris config check --config /etc/stellaris/server.toml
stellaris server init --config /etc/stellaris/server.toml
```

初始化创建节点 CA certificate/key 和空协调状态，且绝不覆盖或轮换已有状态。若进程
在完整、严格有效的 CA pair 已持久化后中断，而协调状态尚未创建，同一命令可安全
重试并沿用该 CA；单独 key/certificate、无效 CA pair 或已有协调状态均拒绝。初始化
后备份节点 CA、协调状态和配置；节点 CA 私钥与协调状态必须作为同一安全恢复集管理。
不要在普通启动失败时删除状态并重新初始化，这会改变信任根并使现有 Agent identity
无效。

启动：

```bash
stellaris server run --config /etc/stellaris/server.toml
```

Server 以专用非登录、非 root 用户运行，并只授予配置、注册表、service TLS 和状态
目录所需权限。服务管理器应限制文件、内存、文件描述符和 UDP 速率；持续配置错误
不要形成无延迟重启循环。

节点表仅在启动时加载。修改 enabled、地址或 token 摘要后安排 Server 重启。在线
session 不在快照中，Server 重启会要求节点重新认证。

## 启动 Agent

每个 Agent 使用不同的 node ID、token、identity 目录、overlay 地址注册项和 P2P
UDP bind：

```bash
stellaris config check --config /etc/stellaris/agent.toml
sudo stellaris agent run --config /etc/stellaris/agent.toml
```

首次运行会在 `identity.directory` 创建 P-256 私钥和持久 enrollment 请求。成功后
安装节点证书及节点 CA；不要删除该目录，也不要在主机间复制。token 是一次性的，
identity 丢失后不能依靠同一 token 重建节点。

Agent 从 `ControlWelcome` 取得权威 overlay CIDR 和 MTU，创建 TUN 并安装 overlay
路由。它不修改默认路由或 DNS。节点 P2P UDP 端口只向需要直连的受控 LAN 开放；
Relay UDP 端口必须保持可达，以便无 P2P 或断线后的后续包回退。

## 凭据生命周期

节点证书有效期为 24 小时。Agent 在约半个有效期带抖动续期，且 CSR 必须沿用当前
公钥。新证书落盘后建立替代 control/Relay 会话；P2P endpoint 更新 TLS 身份而不
重新 bind。监控续期失败，避免证书到期后 control/Relay/P2P 全部关闭。

若需要轮换节点私钥：

1. 生成新 token，并替换节点表中的摘要。
2. 重启 Server，使新静态表生效。
3. 安全归档旧 Agent identity，使用新 token 在新的私有 identity 目录 enrollment。
4. 新 enrollment 提交后，持久授权 SPKI 被替换；验证旧证书无法再连接。

该操作没有宽限期或在线管理 API，应在隔离测试中先演练。禁用节点同样需要修改节点
表并重启 Server；正式撤销操作接口尚未提供。

## 容器

容器示例见 [`../deploy/docker/README.md`](../deploy/docker/README.md)。Server 容器
不需要额外 capability；Linux Agent 仅授予 `/dev/net/tun` 和 `NET_ADMIN`，不得使用
`--privileged`。

Agent identity 必须使用持久、每节点独立且权限受限的 volume。把 identity 放在
临时文件系统会在容器重建后丢失私钥和证书，而一次性 token 无法再次 enrollment。
固定具体镜像版本或 digest，不使用 `latest`；当前 alpha 不应被当作已发布稳定镜像。

## 验证清单

以下是部署验收动作，不是仓库已经通过的结果：

1. 三个 Server UDP listener 都已绑定，日志不含 token、私钥或完整控制 payload。
2. Agent 完成 enrollment，随后建立 control 和 Ready Relay；重启 Agent 后不再次
   消费 token。
3. 两节点经 Relay 验证 ping、TCP 和 UDP，再确认局域网可达时路径计数转为 P2P。
4. 阻断 P2P 后验证当前失败包不被补发、后续包回退 Relay。
5. 重启 Server/Agent，验证重新认证、TUN/路由行为和证书续期。
6. 在 Linux namespace 门禁中运行故障注入及至少 30 分钟资源 soak。

指标核心记录会话、Relay/P2P 包、丢包原因、续期、建链、路径切换和队列高水位，
配置 `observability.metrics_bind` 后通过 HTTP `/metrics` 导出。该 exporter 尚未通过
发布门禁，只能绑定可信管理接口；同时应结合脱敏日志与进程/OS 监控，且绝不采集
token、私钥、完整证书材料或用户包体。
