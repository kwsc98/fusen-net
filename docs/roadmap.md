# 路线图

路线图描述目标，不是发布日期承诺。是否完成以对应 Release、CI 和兼容矩阵为准。

## 0.1.0-alpha.1：开源基线

当前阶段目标：

- 标准化 workspace：`crates/fusen-net` 核心库和 `apps/fusen-net-cli` 应用。
- 固定 Rust 1.97.0、Edition 2024、lockfile、统一 lint 和基础 CI。
- 建立中英文入口、架构/协议/安全/部署文档和双许可证。
- 提供严格 TOML、配置校验、token 生成器及不包含秘密的示例。
- 移除旧 TCP 文档、硬编码公网地址以及仓库内测试私钥。

退出条件：普通 build/test/fmt/clippy/rustdoc 和许可证/漏洞检查通过，CLI 配置与
token 子命令可用；未接通的数据面必须明确标为不可用。

## 0.1.0-alpha.2：可运行应用

- 接通 `server` 和 `agent` CLI 到核心运行时，支持多 listener Relay。
- 完成安全配置优先级、秘密文件权限检查和相对路径语义。
- Agent 只管理 overlay CIDR 路由，支持正常退出清理和退避重连。
- 用运行时生成的测试 CA/证书替代任何嵌入私钥。

退出条件：Fake TUN 下一个 Relay、两个 Edge 双向传包、重连、背压和优雅退出
集成测试通过。

## 0.1.0-beta.1：协议和安全

- 完成 `FNET` v1 控制帧、注册状态机和严格边界解析。
- 完成静态地址分配、常量时间 token 验证、session 所有权路由和源地址校验。
- 清除网络输入路径的 panic/todo/吞错，所有运行队列有界。
- 增加随机输入、异常包、重复节点和路由竞态测试。

退出条件：协议/安全单元测试、Fake TUN 故障测试、依赖安全门禁全部通过，威胁
模型与实现复核一致。

## 0.1.0-rc.1：后端和平台

- Quinn、s2n-quic、gm-quic 通过统一 transport 契约测试。
- Linux、macOS 和 Windows 平台适配统一输出无平台头 IPv4 包。
- 三个平台的真实 TUN runner 完成 ping/TCP/UDP、重连和路由回滚。
- 发布二进制、Linux 镜像、checksum、SBOM 和 provenance 的流水线演练。

退出条件：[`releasing.md`](releasing.md) 中全部稳定版门禁通过，无未处理的高危
安全问题。

## 0.1.0：首个稳定版本

稳定版范围仍是中心 Relay、单租户和 IPv4-only。稳定表示文档中的支持矩阵和恢复
标准已经验证，不表示具备 ACL、IPv6、默认路由、DNS、NAT 穿透或 mesh。

## 后续候选方向

以下内容需独立 ADR、威胁模型和版本计划，尚未承诺进入具体版本：

- 多租户和显式 ACL；
- 动态地址租约及持久化；
- IPv6 overlay；
- 角色为 `Hybrid` 的监听/拨号节点；
- 节点发现、路由通告、NAT 穿透和分布式组网。

在完整 mesh 之前，优先保证中心模型的正确性、安全性、恢复能力和资源稳定性；0.1
不设置吞吐量 SLA。
