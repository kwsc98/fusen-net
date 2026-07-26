# 部署指南

> **文档适用性：Current；适用范围：v2；设计评审状态：N/A；ADR 决策状态：N/A；
> 交付状态：Implemented；验证状态：Unverified；发布状态：Unreleased。**

<!-- stellaris-release-status:deployment-status:start -->
本指南面向 Stellaris `0.3.0-alpha.1` 的全新 v2 部署。当前版本未通过完整 Linux 真实
TUN、故障注入和资源 soak 门禁，只适合隔离测试环境。
<!-- stellaris-release-status:deployment-status:end -->

不要尝试复用任何旧配置、状态、token 或节点 identity。

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

Server 不创建 TUN，也不需要 `NET_ADMIN`。Linux Agent 需要 `/dev/net/tun`、`iproute2`
提供的外部 `ip` 命令，以及 root 或 `CAP_NET_ADMIN`。缺少 `ip` 时 TUN 可能创建成功，
但 overlay 路由安装会失败。macOS/Windows 当前仅为编译目标，不应按本指南宣称运行支持。

## 准备信任材料

部署者先提供：

- 由组织/公开部署 CA 签发的 service certificate，SAN 匹配配置 `server_name`；
- 对应 service private key；
- Agent 可读取的部署 CA PEM bundle；
- 每节点独立的一次性 enrollment token 及其静态注册表摘要。

service certificate 用于 enrollment、control 和 Relay 三个 listener。节点 CA 由
`server init` 单独创建，不能用部署 CA 私钥替代，也不要把节点 CA 私钥分发给 Agent。

隔离开发环境可从仓库根目录执行：

```bash
bash scripts/generate-dev-tls.sh ./configs/certs stellaris.test
openssl verify \
  -CAfile ./configs/certs/deployment-ca.pem \
  ./configs/certs/deployment-server.pem
```

helper 会拒绝覆盖现有文件，生成 30 天开发 CA 和 7 天 service certificate，并在返回前
验证签发链。它不替代生产 PKI；生产部署还必须由 PKI 工具验证 SAN、有效期、leaf-first
chain 和 cert/key 匹配，且不得把 `deployment-ca-key.pem` 放进 Stellaris Server。

先为 Server 和每个 Agent 创建独立的非登录运行账户。以下名称只是示例；账户创建方式
由操作系统决定：

```bash
sudo install -d -o stellaris-server -g stellaris-server -m 0700 \
  /etc/stellaris/server-secrets /var/lib/stellaris/server
sudo install -d -o stellaris-agent -g stellaris-agent -m 0700 \
  /etc/stellaris/agent-secrets /var/lib/stellaris/agent
sudo -u stellaris-agent stellaris token generate \
  --node-id edge-a --output /etc/stellaris/agent-secrets/edge-a.token
```

将打印的摘要写入 Server 的 `nodes.toml`，不要复制 token 明文到 Server。通过安全带外
通道把 token 文件交给对应 Agent 运行账户。Unix 上所有私钥、token、协调状态和
identity 目录必须由实际读取它们的 effective UID 所有且 group/other 不可访问。
service key 应以 `0600` 安装给 `stellaris-server`；配置和公开证书可以只读共享。

## 初始化 Server

准备好 `server.toml`、节点表和部署 TLS 文件后：

```bash
sudo -u stellaris-server stellaris config check --config /etc/stellaris/server.toml
sudo -u stellaris-server stellaris server init --config /etc/stellaris/server.toml
```

初始化创建节点 CA certificate/key 和空协调状态，且绝不覆盖或轮换已有状态。若进程
在完整、严格有效的 CA pair 已持久化后中断，而协调状态尚未创建，同一命令可安全
重试并沿用该 CA；单独 key/certificate、无效 CA pair 或已有协调状态均拒绝。初始化
后备份节点 CA、协调状态和配置；节点 CA 私钥与协调状态必须作为同一安全恢复集管理。
不要在普通启动失败时删除状态并重新初始化，这会改变信任根并使现有 Agent identity
无效。

启动：

```bash
sudo -u stellaris-server stellaris server run --config /etc/stellaris/server.toml
```

`config check`、`server init` 和 `server run` 必须使用同一个 Server effective UID；
否则初始化生成的节点 CA/状态会因 owner 不匹配而在运行时被拒绝。Server 以专用
非登录、非 root 用户运行，并只授予配置、注册表、service TLS 和状态目录所需权限。
服务管理器应限制文件、内存、文件描述符和 UDP 速率；持续配置错误不要形成无延迟
重启循环。

节点表仅在启动时加载。修改 enabled 或 token 摘要后安排 Server 重启。地址只允许在
首次 enrollment 前修改；已有持久 node/IP binding 时原地改地址会使启动 fail-closed，
v2 没有地址迁移流程。在线 session 不在快照中，Server 重启会要求节点重新认证。

## 启动 Agent

每个 Agent 使用不同的 node ID、token、identity 目录、overlay 地址注册项和 P2P
UDP bind：

先用实际 Agent 用户校验：

```bash
sudo -u stellaris-agent stellaris config check --config /etc/stellaris/agent.toml
```

普通 `sudo -u stellaris-agent` 不会自动获得 TUN 所需 capability。Linux 上建议由
systemd 显式授予进程及其路由子进程 `CAP_NET_ADMIN`，最小 service 片段如下：

```ini
[Service]
User=stellaris-agent
Group=stellaris-agent
ExecStart=/usr/local/bin/stellaris agent run --config /etc/stellaris/agent.toml
AmbientCapabilities=CAP_NET_ADMIN
CapabilityBoundingSet=CAP_NET_ADMIN
NoNewPrivileges=true
Restart=on-failure
```

同时确认该用户可以打开 `/dev/net/tun`。也可以在隔离测试机上让 token 生成、配置校验
和 Agent 启动全部以 root 运行，但不能中途切换 effective UID；token 和 identity owner
会不匹配。不要简单给可被其他用户执行的共享二进制设置宽泛 file capability。

首次运行会在 `identity.directory` 创建 P-256 私钥和持久 enrollment 请求。成功后
安装节点证书及节点 CA；不要删除该目录，也不要在主机间复制。token 是一次性的，
identity 丢失后不能依靠同一 token 重建节点。

同一个 identity 目录不得同时挂载给两个 Agent 进程或两台主机。v2 没有跨进程
identity lock；重复运行同一私钥/证书会造成 control incarnation 和路径反复替换。

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

## 备份与恢复

当前 v2 没有在线一致快照、快照 generation 或旧备份回滚检测。以下流程是最低操作
要求，尚未通过完整灾难恢复门禁：

1. 停止 Server，并确认没有进程仍在写协调状态。
2. 把 `coordinator_state_file`、节点 CA certificate/key、`server.toml`、`nodes.toml`
   和部署 service TLS 材料作为同一个带时间戳、不可修改的恢复集备份。
3. 分别停止每个 Agent，再备份其完整 `identity.directory`；不要只复制私钥或
   `credentials.json`，也不要从一个 Agent 目录拼接另一个目录的文件。
4. 记录备份对应的 commit、二进制版本、文件哈希、owner/mode 和节点表摘要。备份本身
   必须加密并限制访问。
5. 恢复时写入空目录，恢复原 owner/mode，使用同一 release；不得合并不同时点的节点
   CA 和协调状态，也不得在两台主机同时启动同一 Agent identity。
6. 先在隔离维护网络中以实际 Server 用户启动并验证状态/注册表一致，再逐个启动 Agent
   并验证旧 token 没有再次消费、node/IP/SPKI 与备份一致。

若恢复的 Agent leaf 已过期，它无法通过旧 control 会话续期：为该 node 生成新 token，
更新静态摘要并重启 Server，再把 token 配置给该 Agent 进行 enrollment。该流程复用
备份中的 operational key；若 Server 当前授权 SPKI 已与备份不同，新 enrollment 会再次
替换授权 SPKI，必须在维护窗口验证旧凭据拒绝。无法确认 Server/Agent 备份属于同一
时间点时，不得拼接恢复。

恢复一份结构正确但较旧的协调状态可能恢复旧 token/SPKI 授权；v2 无法自动识别这种
回滚。只有能够证明是最新的一致备份才能恢复。无法证明时应隔离旧部署并重建整个
信任域，不能把 `server init` 当修复命令。

## 证书轮换

- **同一部署 CA 下更新 service certificate：**并排写入新 cert/key，严格设置 owner/
  mode，更新配置路径并重启 Server。当前没有证书热加载。
- **更换部署 CA：**先让所有 Agent 的 `deployment_ca_file` 同时信任新旧 CA 并重启
  Agent，再切换 Server service cert/key；全部节点恢复连接后才能移除旧 CA。
- **节点 leaf：**由 Agent 在有效 control 会话中自动续期，不需要管理员复制证书。
- **节点 CA：**v2 不支持原地轮换或双节点 CA 信任。节点 CA 到期、泄露或必须替换时，
  停止部署，重新初始化全新状态并用新 token enrollment 所有 Agent。

service key 或未消费 enrollment token 泄露后应立即隔离受影响 listener/节点并轮换。
节点 CA 或协调状态疑似泄露时，当前安全边界不提供继续运行保证。

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
