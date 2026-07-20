# 更新日志

本项目遵循 [Keep a Changelog](https://keepachangelog.com/zh-CN/1.1.0/) 和
[语义化版本](https://semver.org/lang/zh-CN/)。`0.x` 期间仍可能发生不兼容变更，
具体兼容承诺见 [`docs/compatibility.md`](docs/compatibility.md)。

## [Unreleased]

## [0.1.0-alpha.1] - 2026-07-20

### Added

- 统一的 `fusen-net server|agent|config|token` 命令入口。
- 版本化配置、静态节点注册表及 QUIC/TUN v1 协议边界。
- 开源治理、安全、架构、部署和发布文档。
- 三后端 3 x 3 Relay 路由、Relay 重启恢复和路由安装清理竞态测试。
- 三平台真实 TUN 生命周期 harness，以及 Linux 双 namespace 的故障注入、三后端
  30 分钟 soak 和进程资源增长门禁。

### Changed

- 项目主线从旧 TCP 端口代理切换为中心 Relay 转发的 IPv4 overlay。
- 源码重组为核心库和 CLI 应用，应用构建提交 `Cargo.lock`。
- 许可证改为 `Apache-2.0 OR MIT`。
- 通过 Apache-2.0 本地 fork 补齐 gm-quic 0.4 的 1-RTT Datagram 组包，并将完整
  Datagram 路径的收发队列固定为 256；队列满时拒绝或丢弃新 Datagram。
- Rust 基线更新为 1.97.0；v1 overlay MTU 固定为 576..=1100。
- `NodeRuntime` 改为可组合的监听、拨号、转发和 TUN 能力模型，并增加独立的
  `control` 与 `data_plane` 模块边界。
- Edge 注册握手统一使用 10 秒超时；路由安装不再因退出取消而丢失清理凭证。
- Windows 配置检查会拒绝授权给所有者、Administrators 和 SYSTEM 之外主体的
  token 或私钥 ACL。
- Windows token 生成器会在写入秘密前建立受保护 ACL；Agent 从可执行文件目录
  加载并验签 Release 附带的 `wintun.dll`。
- TUN 适配按 Linux、macOS、Windows 拆分；发布工作流固定第三方 Action 和已验证
  tag commit，并校验版本化 changelog。

### Removed

- 旧 TCP 端口映射 CLI、配置和线协议兼容性。
- 仓库内硬编码的公网地址、示例证书和私钥。

## 历史快照

0.1 之前的代码是未发布、未打标签的实验快照，不构成版本兼容基线。旧 TCP 主线
将在正式迁移时保留为 `legacy-tcp-v4` 标签。
