# 现代分布式组网方案调研

> **调研快照：2026-07-26。文档适用性：Current；适用范围：非规范设计参考；设计评审
> 状态：N/A；ADR 决策状态：N/A；交付状态：N/A；验证状态：N/A；发布状态：N/A。**
> 本文比较可迁移到 Stellaris 的现代方案，不定义当前协议、配置或已支持能力。外部项目
> 会继续演进；进入 `V3-P0` 或新的架构 ADR 前必须重新核对原始资料。

## 结论

现代分布式网络没有一种可以单独称为“最先进证明方式”的机制。生产系统通常把六类
问题分开解决：

```text
长期 principal        谁是这个节点
短期 credential       它现在用哪把密钥证明身份
authorization state   它现在是否获准、拥有什么地址和权限
locator/path          现在从哪里可以到达它
secure transport      这条连接是否机密、完整且持有对应私钥
attestation           宿主硬件/软件是否处于某个可接受状态（可选）
```

对 Stellaris 当前规模，推荐组合仍是“中心准入与短期签发 + 持久 NodeUID/地址状态 +
节点间 QUIC mTLS + 协调发现 + Relay 回退”。这与完全中心转发不同：信任和授权可以
集中裁决，能够直连的数据面仍可分布在节点之间。

## 主流模式比较

| 模式 | 代表规范或系统 | 擅长解决 | 不能单独解决 |
| --- | --- | --- | --- |
| CA 签发短期工作负载身份 | SPIFFE X.509-SVID | 可轮换身份、明确 trust domain、标准 X.509 验证 | 地址分配、业务授权、恶意 CA |
| 自认证 peer ID | libp2p Peer ID、WireGuard public key | 无需中心查询即可把连接绑定到公钥 | 安全换钥时保持长期身份、组织准入、地址冲突 |
| 协调控制面 + P2P 数据面 + Relay | Tailscale 等现代 overlay | 节点发现、NAT/路径协调、直连优先和可靠回退 | 控制器/Relay 信任、跨组织策略本身 |
| 分域 trust bundle / federation | SPIFFE Federation、SCION ISD/TRC | 多管理域独立根信任与显式互信 | 全局名称、地址、ACL 和撤销冲突的自动裁决 |
| 远程证明 | IETF RATS、TPM/TEE attestation | 证明设备或软件状态的可验证 claims | 节点唯一性、网络地址所有权、普通 mTLS 授权 |
| 透明日志、阈值签发 | Certificate Transparency 类日志、threshold CA | 检测签发分歧、降低单在线签发密钥风险 | 离线节点即时撤销、数据路径加密 |

### 短期 CA 身份

SPIFFE 的稳定 X.509-SVID 规范使用恰好一个 URI SAN 表达工作负载身份，并要求标准
X.509 path validation、受约束的 KeyUsage/EKU 和 trust bundle。可迁移的重点不是直接
宣称 Stellaris 与 SPIFFE 兼容，而是：

- principal 使用规范化 URI/命名空间，不从网络消息自报身份；
- leaf 短期有效，签发 CA 与 leaf 用途严格分开；
- 验证链之后仍执行应用层授权，证书公开本身不等于“当前获准”；
- trust bundle 的添加、重叠和移除必须有明确顺序。

Stellaris v3 候选使用自己的 URI 和 claims profile，因此除非未来 ADR 决定完整采用
SPIFFE Workload API、bundle 和验证规则，否则不得标为 X.509-SVID 兼容。

### 自认证身份

libp2p Peer ID 是 protobuf 编码公钥的 multihash；较短公钥可由 identity multihash
直接内嵌，其他情况使用密码学摘要。WireGuard 的 cryptokey routing 则直接把公钥与
允许的网络地址关联。这类模型让“我持有对应私钥”非常清楚，适合开放式 P2P 发现，
但长期身份等于 operational key 时会产生一个重要代价：安全换钥会改变身份，或必须
再增加一层签名委托。

因此 Stellaris 不建议把当前 SPKI 哈希直接作为永久 NodeUID。推荐由信任域签发不可变
NodeUID，再用 `key_epoch` 和当前授权 SPKI 处理换钥。这样保留自认证握手的 PoP，同时
不把长期审计身份绑死在可能泄露的密钥上。

### 控制面与数据面分离

现代 mesh overlay 普遍把成员、策略和 endpoint discovery 放在控制面，把端到端加密
包放在 peer 数据面；直连不可用时再走中继。这里最重要的不变量是“路径不是身份”：
host candidate、NAT 映射和 Relay locator 只能说明如何到达节点，不能授予 node ID、
overlay IP 或权限。

Stellaris 当前的 Relay-first、后台探测、Ready 后按目标原子切换和失败包不补发符合该
方向。下一步应先完成身份/地址状态和真实 underlay 证据，再增加 NAT 穿透，避免把复杂
连接问题与尚未稳定的授权模型叠加。

### 多信任域

SPIFFE Federation 的关键约束是每个 trust domain 保持独立 bundle，不能把多个域的
key 合并成一个会让任一域冒充其他域的共同根。SCION 也通过 Isolation Domain 与 Trust
Root Configuration 把信任和路由放在明确边界内。对 Stellaris，这意味着未来
federation 至少需要：

- `TrustDomainId + NodeUID` 形式的全局 principal；
- 每域独立 trust bundle、地址前缀、授权策略和撤销信息；
- 明确的双边互信与 capability，而不是加载第二张 CA 后默认全互通；
- 防 bundle/manifest 回滚、签发分歧和地址/名称冲突的规则。

因此“中心化准入、短期签发和权威授权状态”足以支撑当前单信任域；只有一个 CA signer
并不足以裁决节点准入、当前 SPKI、地址所有权和撤销。即使完整的单域控制面也不自动
构成真正的多信任域架构。多域 federation 必须单独建 ADR 和威胁模型。

### 远程证明与更强控制器安全

IETF RATS 把 Evidence、Verifier、Attestation Result 和 Relying Party 分开。它适合未来
要求“只有满足指定固件/启动状态的节点才能 enrollment”的环境，但 attestation 是
身份和授权的附加输入，不能替代 NodeUID、证书、地址租约或撤销状态。

Raft 解决协调服务的 crash fault 和一致状态复制；BFT、阈值签名、透明日志主要用于
控制器可能作恶或签发 equivocation 的威胁模型。当前单组织、最多 256 节点不应先承担
这些复杂度。只有 HA 或恶意控制器成为明确需求时再分别决策。

## Stellaris 采用顺序

| 顺序 | 采用内容 | 当前判断 |
| --- | --- | --- |
| 1 | QUIC/TLS 1.3、独立 ALPN、PoP、严格应用授权 | v2 已实现，发布门禁未完成 |
| 2 | 永久 NodeUID、名称/密钥/地址分离、短期 leaf | Proposed v3 |
| 3 | 离线 Root、在线 Intermediate、带 epoch 的 issuer manifest | Proposed v3 |
| 4 | 真实 host candidate 过滤和 underlay 路径证据 | v2 可复用基线门禁 |
| 5 | server-reflexive candidate 与双向打洞 | Conditional NAT 阶段 |
| 6 | 多协调 HA、跨域 federation、Relay E2E、attestation | 独立 ADR，未排期 |

不推荐在首轮加入 DID、区块链、PoW/PoS、全局 BFT 或零知识成员证明。它们不能替代
组织准入、地址唯一性、当前授权和恢复语义，且没有对应的当前威胁模型。

## 官方参考

以下链接在调研快照日期核对；它们是设计依据，不是 Stellaris 的依赖或兼容声明。

- [RFC 9000: QUIC](https://www.rfc-editor.org/rfc/rfc9000)
- [RFC 8446: TLS 1.3](https://www.rfc-editor.org/rfc/rfc8446)
- [RFC 7301: ALPN](https://www.rfc-editor.org/rfc/rfc7301)
- [RFC 5280: X.509 PKI certificate and CRL profile](https://www.rfc-editor.org/rfc/rfc5280)
- [SPIFFE X.509-SVID specification](https://github.com/spiffe/spiffe/blob/main/standards/X509-SVID.md)
- [SPIFFE Federation specification](https://github.com/spiffe/spiffe/blob/main/standards/SPIFFE_Federation.md)
- [libp2p Peer IDs and Keys](https://github.com/libp2p/specs/blob/master/peer-ids/peer-ids.md)
- [WireGuard protocol and cryptokey routing](https://www.wireguard.com/protocol/)
- [Tailscale control/data-plane overview](https://tailscale.com/docs/concepts/what-is-tailscale)
- [RFC 9334: RATS Architecture](https://www.rfc-editor.org/rfc/rfc9334)
- [RFC 9162: Certificate Transparency Version 2.0](https://www.rfc-editor.org/rfc/rfc9162)
- [Raft consensus paper and resources](https://raft.github.io/)
- [Practical Byzantine Fault Tolerance](https://pmg.csail.mit.edu/papers/osdi99.pdf)
- [NIST Threshold Cryptography project](https://csrc.nist.gov/projects/threshold-cryptography)
- [SCION cryptography overview](https://docs.scion.org/en/latest/cryptography/index.html)

项目规范选择以 [`design-overview.md`](../design-overview.md)、
[`node-identity-trust-addressing-plan.md`](../node-identity-trust-addressing-plan.md) 和相关 ADR
为准；外部资料与项目权威文档冲突时，先通过新 ADR 更新项目决策，不能直接按外部实现
修改协议。
