# selector4nix：Fastly 端点优选交接

## 1. 交接结论

目标不是把 Fastly 优选做成 mosdns 插件，也不是把 `cache.nixos.org` CNAME 到第三方优选域名，而是把它集成到 `selector4nix`：

> 对 `https://cache.nixos.org/` 这个逻辑 substituter 维护多个 Fastly 网络端点；请求 URL、HTTP Host 和 TLS SNI 始终保持 `cache.nixos.org`，仅覆盖 TCP 连接目标 IP；在证书和 HTTP 校验通过的端点中，根据元数据请求延迟和真实 NAR 传输表现选择端点，并在失败时切换。

该能力属于 substituter 内部的传输路由选择，不应被建模成多个独立 substituter。多个端点共享相同的缓存内容、签名密钥、凭据、优先级和 NAR URL 语义，区别仅在网络路径。

## 2. 当前状态

- 本交接基于 `p3psi-boo/selector4nix` 提交 [`a0e8b0db006cdc2f275a740f7c60f760a1346607`](https://github.com/p3psi-boo/selector4nix/tree/a0e8b0db006cdc2f275a740f7c60f760a1346607)。
- mosdns 中曾提交 CNAME 方案：`79e7ace add fastly cname preference plugin`。
- CNAME 方案经真实 TLS 测试证明不适合 `cache.nixos.org`，不应作为 selector4nix 实现基础；selector4nix 版本落地后，应另行决定是否从 mosdns 撤回该实验插件。

## 3. 已验证事实

### 3.1 Fastly IP 不能跨服务任意替换

DNS CNAME 不会把客户端的 TLS SNI 从原域名改成 CNAME 目标。即使返回：

```text
cache.nixos.org. CNAME fastly.182682.xyz.
```

客户端连接最终 IP 时，TLS SNI 仍是 `cache.nixos.org`。Fastly 官方要求域名使用与其 TLS configuration 对应的 DNS 记录；DNS、证书和 TLS activation 是关联的：

- [Fastly DNS Records](https://www.fastly.com/documentation/reference/api/tls/custom-certs/dns-records/)
- [Fastly TLS quick start](https://www.fastly.com/documentation/guides/getting-started/domains/securing-domains/tls-quick-start/)
- [Routing traffic to Fastly](https://www.fastly.com/documentation/guides/concepts/routing-traffic-to-fastly/)

2026-08-22 实测 `fastly.182682.xyz` 返回的下列地址，全部不能为 `cache.nixos.org` 提供匹配证书：

```text
151.101.67.52
151.101.131.52
151.101.191.52
151.101.207.52
199.232.179.52
```

失败信息：

```text
SSL: no alternative certificate subject name matches target host name 'cache.nixos.org'
```

因此不得使用以下方法：

- CNAME 到第三方 Fastly 优选域名；
- 将第三方域名解析出的 IP 直接用于 `cache.nixos.org`；
- 使用 `danger_accept_invalid_certs`、`-k` 或其他方式绕过证书校验；
- 把 ICMP ping 成功视为端点可用。

### 3.2 Discussion #511 的地址规律只能用于发现候选

[mosdns Discussion #511](https://github.com/IrineSistiana/mosdns/discussions/511) 总结了 Fastly IPv4 第三段、服务类型、区域段以及 IPv6 后缀之间的经验规律。该规律可帮助生成候选，但不是 Fastly 公布的稳定协议。讨论中的 mosdns 作者也指出其可靠性未知，Fastly 可随时改变部署方式。

所以该规律的正确用途是：

```text
经验规律 -> 生成候选 -> TLS/SNI 校验 -> HTTP 校验 -> 传输测试 -> 进入可用端点池
```

而不是：

```text
经验规律 -> 无验证地改写生产 DNS
```

### 3.3 当前出口的实测结果

测试环境的公网出口由 Cloudflare 标识为 `LAX`。以下结果只证明该出口当时的路径，不代表中国大陆家庭宽带、其他运营商或其他时间。

所有候选均使用原始 URL 和 SNI：

```bash
curl --noproxy '*' \
  --resolve "cache.nixos.org:443:${IP}" \
  https://cache.nixos.org/nix-cache-info
```

只有证书有效且返回 HTTP 200 的候选进入吞吐测试。吞吐测试使用缓存中真实 NAR 的同一段 Range：

```text
/nar/06kdfhjdwfyw2ycvc57h8zfqilqgkc5x69n1kzfnlxs931n60d5r.nar.zst
Range: bytes=0-1048575
```

每个端点使用三次独立连接并比较中位数。三次是本次实验中能够计算中位数并容忍一次瞬时抖动的最小奇数样本；1 MiB 是根据该链路实测传输阶段明显长于建连和 TTFB 后选取的实验范围，不应直接成为产品默认值。

| 排名 | IP | TTFB 中位数 | 1 MiB 总耗时中位数 | 吞吐中位数 |
|---:|---|---:|---:|---:|
| 1 | `199.232.193.91` | 301.0 ms | 2.562 s | 0.390 MiB/s |
| 2 | `151.101.1.91` | 321.3 ms | 2.576 s | 0.388 MiB/s |
| 3 | `151.101.65.91` | 339.5 ms | 2.618 s | 0.382 MiB/s |
| 4 | `151.101.41.91` | 481.5 ms | 2.692 s | 0.371 MiB/s |
| 5 | `151.101.129.91` | 357.6 ms | 2.876 s | 0.348 MiB/s |

本轮结果不能支持“永久固定一个全球最快 IP”。前三名差距很小，路由和负载也会变化。它支持的是：端点必须在实际 Nix 客户端所在网络持续测量，并保留故障切换能力。

## 4. selector4nix 当前可复用的能力

selector4nix 已经具备本功能需要的大部分机制：

- `SubstituterProbingProvider` 使用 `/nix-cache-info` 判断 substituter 健康状态；
- `NarInfoProvider` 已记录 `.narinfo` 请求延迟；
- `NarStreamProvider` 已并发打开候选 substituter 的 NAR 流；
- `selector4nix-streaming` 已支持 Range、分块下载、滑动窗口和 per-host throttling；
- `NarTransferMetric` 已采集实际 NAR 传输信息；
- actor、repository 和 domain event 已提供状态隔离和失败传播边界。

缺少的是“一个 substituter 内部有多个可选择连接端点”的领域模型，以及让 reqwest 在保留 URL host/SNI 的情况下连接指定 IP 的基础设施适配。

## 5. 建议领域模型

### 5.1 新概念：SubstituterEndpoint

建议在 `selector4nix-core` 的 substituter 领域下引入端点实体或值对象，至少表达：

```text
SubstituterEndpoint
  endpoint_ip            实际 TCP 连接地址
  availability           是否通过最近一次 TLS/HTTP 验证
  metadata_latency       /nix-cache-info 或 .narinfo 的观测延迟
  nar_throughput         实际 NAR 分块传输的观测吞吐
  last_success           最近成功时间
  last_failure           最近失败及失败类别
```

逻辑 host 不需要在每个 endpoint 中重复保存，应来自所属 substituter URL，例如 `cache.nixos.org`。

### 5.2 不要复制 Substituter

以下建模不推荐：

```toml
[[substituters]]
url = "https://151.101.1.91/"

[[substituters]]
url = "https://199.232.193.91/"
```

它会导致：

- URL host 变成 IP，破坏 Host 和 TLS SNI；
- 同一缓存被错误视为不同 substituter；
- 健康状态、优先级、凭据和 NAR location 缓存重复；
- 现有 substituter 选择策略承担不属于它的端点路由职责。

正确关系应是：

```text
Substituter(cache.nixos.org)
  ├── Endpoint(151.101.1.91)
  ├── Endpoint(151.101.65.91)
  └── Endpoint(199.232.193.91)
```

## 6. 建议配置边界

在 `SubstituterRawConfiguration` 和 `SubstituterConfiguration` 下增加可选的 endpoint selection 配置。字段名称可在实现时按项目术语调整，但语义建议保持如下：

```toml
[[substituters]]
url = "https://cache.nixos.org/"

[substituters.endpoint_selection]
mode = "fastly"
candidates = [
  "151.101.1.91",
  "151.101.65.91",
  "151.101.129.91",
  "151.101.193.91",
  "199.232.193.91",
]
```

配置规则：

- `candidates` 是静态种子，不代表永久可用；
- 当前 DNS A/AAAA 结果应自动加入候选池，保证 Fastly 官方路径始终参与选择；
- Discussion #511 的区域地址推导应是显式实验模式，不应隐式启用；
- 探测频率、样本窗口、退避和超时不得由本交接凭直觉指定默认值，应沿用项目现有网络策略，或由维护者根据实际测量确定；
- 未配置 endpoint selection 时，所有现有 substituter 行为必须保持不变。

## 7. HTTP 客户端实现要求

### 7.1 URL 不变，只覆盖 DNS 解析

对候选 `199.232.193.91` 发起请求时，语义必须等价于：

```bash
curl --resolve cache.nixos.org:443:199.232.193.91 \
  https://cache.nixos.org/nix-cache-info
```

即：

```text
URL host = cache.nixos.org
HTTP Host = cache.nixos.org
TLS SNI = cache.nixos.org
TCP peer = 199.232.193.91
```

reqwest 可通过 endpoint-scoped client 的 DNS override，或自定义 connector，实现上述语义。不要把请求 URL 改成 IP，也不要手工伪造 Host 后关闭 TLS 校验。

### 7.2 建议增加 EndpointHttpClientPool

当前 `bootstrap.rs` 构建一个通用 reqwest `Client` 和一个 `StreamingClient`。建议增加基础设施层客户端池：

```text
EndpointHttpClientPool
  key = (logical_host, endpoint_ip)
  value = reqwest::Client / StreamingClient transport
```

每个 endpoint client：

- URL 仍使用 substituter 原始域名；
- DNS override 固定到候选 IP；
- 使用正常 CA 和 hostname verification；
- 复用连接，但 endpoint 状态变化后允许淘汰对应 client；
- 继承现有 timeout、User-Agent、凭据和网络设置。

### 7.3 分块请求必须保持端点一致

`selector4nix-streaming/src/client.rs` 的首次 Range 请求会把 client、URL 和 configure closure 交给 `HttpChunkConnector`，后续 chunk 再使用同一 client。

选定一个 endpoint 后：

- 首个 Range 请求必须通过 endpoint-bound client；
- `HttpChunkConnector` 的后续所有 Range 请求必须继续使用该 endpoint-bound client；
- 不得让首块走优选 IP、后续块重新走系统 DNS；
- 传输中端点失败时，由 NAR stream 层终止或切换，不能静默混用无法验证的地址。

现有 throttler 以 URL host 为键。首版建议继续以逻辑 host 共享并发额度，避免多个 Fastly endpoint 绕过同一 substituter 的并发约束。若未来要改成 per-endpoint 配额，应作为独立需求论证。

## 8. 探测与选择策略

### 8.1 准入门槛

一个 endpoint 只有同时满足以下条件才能进入可用池：

1. TCP/TLS 请求连接到候选 IP；
2. TLS SNI 为 substituter 原始 host；
3. 系统信任链和 hostname verification 成功；
4. `GET /nix-cache-info` 返回 HTTP 200；
5. 响应体可完整读取。

任何证书失败都是 endpoint 不兼容，不是可忽略的临时错误。

### 8.2 两类目标分别评分

metadata 与 NAR 的目标不同，不应只维护一个“ping 值”：

- `.narinfo` 和 `/nix-cache-info`：关注完成延迟或 TTFB；
- NAR 文件：关注真实有效字节吞吐和失败率。

建议冷启动时使用通过准入检查且 metadata 延迟较好的 endpoint；产生真实 NAR 流量后，使用生产传输指标更新 NAR endpoint 选择。不要周期性下载固定大文件作为唯一依据，这会制造额外流量，并可能只测到某个对象的 POP 缓存状态。

### 8.3 候选来源

按可信度排序：

1. substituter 当前 DNS A/AAAA 回答；
2. 用户显式配置的候选；
3. 历史上通过完整 TLS/HTTP 校验的候选；
4. Discussion #511 规律推导出的实验候选。

所有来源最终都必须经过同一准入门槛。来源可信度不能替代验证结果。

### 8.4 失败语义

- 单个 endpoint 失败：更新 endpoint 状态，尝试该 substituter 的其他可用 endpoint；
- 所有 endpoint 均失败：才将失败上升为 substituter 失败；
- endpoint 返回 404：资源不存在与 endpoint 离线必须保持现有语义区分；
- TLS hostname mismatch：标记 endpoint 不兼容，不使用退避后无限重试掩盖配置错误；
- endpoint 切换不应改变 `NarFileLocation` 的逻辑 source URL，避免当前 NAR location 缓存被绑定到短期 IP。

## 9. 与当前代码的建议映射

| 现有位置 | 建议改动 |
|---|---|
| `infrastructure/config/general_raw.rs` | 解析 substituter endpoint selection 原始配置 |
| `infrastructure/config/general_parsed.rs` | 校验候选 IP、模式和配置组合 |
| `domain/substituter/model/` | 增加 endpoint 模型及健康/测量状态 |
| `domain/substituter/port/` | 增加 endpoint 探测与端点选择所需 port |
| `infrastructure/provider/substituter_probing_provider.rs` | 使用 endpoint-bound client 探测 `/nix-cache-info` |
| `infrastructure/provider/nar_info_provider.rs` | 为 metadata 请求选择 endpoint 并回报延迟 |
| `infrastructure/provider/nar_stream_provider.rs` | 为 NAR 请求选择 endpoint，并上报打开/传输失败 |
| `selector4nix-streaming/src/client.rs` | 接收或持有 endpoint-bound client |
| `selector4nix-streaming/src/stream/http_backend.rs` | 保证所有后续 Range 请求沿用选定 endpoint |
| `selector4nix/src/bootstrap.rs` | 组装 endpoint repository、selector、client pool 和 providers |
| dashboard use cases/frontend | 展示 logical substituter 下各 endpoint 的状态和观测值 |

实现时应遵守项目现有分层：领域层定义 endpoint 语义和选择输入，基础设施层负责 reqwest DNS override，application/actor 层协调状态变化；不要让 reqwest 类型进入领域模型。

## 10. 最小实施顺序

### 阶段 A：正确连接

- 支持为单个 substituter 配置静态 endpoint IP；
- 建立 endpoint-bound reqwest client；
- 使用原 host/SNI 完成 `/nix-cache-info` 和 NAR Range 请求；
- 一个 endpoint 失败时切换另一个 endpoint；
- 未配置该功能时行为不变。

阶段 A 证明“端点覆盖不会破坏 TLS、Host、凭据、Range 和 fallback”。

### 阶段 B：动态选择

- 记录 endpoint metadata 延迟；
- 从真实 NAR 分块传输记录吞吐；
- 按请求类型选择 endpoint；
- 将单 endpoint 状态与 substituter 总体状态分离；
- 暴露日志和 dashboard 可观测性。

阶段 B 证明“选择来自真实网络观测，而非静态顺序”。

### 阶段 C：候选发现

- 自动纳入系统 DNS 当前回答；
- 可选地实现 Fastly 经验规律候选生成；
- 对新候选执行完整准入检查；
- 删除长期不兼容候选的运行时状态。

阶段 C 不应阻塞阶段 A、B，也不能弱化 TLS 校验。

## 11. 验收标准

### 11.1 配置与兼容性

- endpoint selection 未配置时，现有配置、请求选择、缓存和 streaming 测试全部保持通过；
- 非 Fastly substituter 不受影响；
- 非法 IP、重复或冲突配置在配置解析阶段得到明确错误；
- 凭据仍按逻辑 URL 匹配，而不是按 endpoint IP 匹配。

### 11.2 TLS 与路由

- 测试能够证明 TCP peer 是指定 endpoint IP；
- URL host、HTTP Host 和 TLS SNI 保持 substituter 原始域名；
- hostname mismatch 的候选不可进入可用池；
- 禁止通过关闭证书校验让测试通过。

### 11.3 NAR streaming

- 首块和后续 Range chunk 使用同一选定 endpoint；
- endpoint 中途失败会产生可观察失败并触发既定 fallback，而非返回截断成功流；
- NAR 内容长度、Range 边界和现有 chunked streaming 行为不变；
- `NarFileLocation` 缓存逻辑 URL，不缓存易变 endpoint IP。

### 11.4 状态与选择

- 单 endpoint 失败不会立即把整个 substituter 标记离线；
- 所有 endpoint 不可用时，substituter 才按现有状态机进入失败流程；
- metadata 延迟和 NAR 吞吐是不同观测量；
- 选择结果可在结构化日志或 dashboard 中看到 endpoint IP 和选择依据。

### 11.5 真实集成验证

在实际部署 selector4nix 的主机上：

1. 配置 `cache.nixos.org` 当前 DNS IP 和至少一个经讨论规律生成的候选；
2. 确认无效候选因证书失败被排除；
3. 通过 selector4nix 执行真实 `nix build`；
4. 确认 `.narinfo`、NAR 首块和后续 Range 请求保持正确 SNI；
5. 确认选定 endpoint 失败时，同一 substituter 的其他 endpoint 接管；
6. 对比启用前后的真实 NAR 有效吞吐，并记录测试出口和时间。

验收不规定固定“提速百分比”。是否提速由部署网络决定；功能正确性的硬门槛是 TLS、HTTP、数据完整性、端点选择和故障切换均成立。

## 12. 建议测试矩阵

### 单元测试

- raw/parsed 配置转换；
- endpoint 候选去重和输入校验；
- endpoint 可用、临时失败、不兼容状态转换；
- metadata 与 NAR 评分输入不混用；
- 所有 endpoint 失败时才上升 substituter failure。

### provider 测试

- 本地 TLS 测试服务分别模拟正确证书和错误证书；
- endpoint-bound client 连接指定地址但发送原 Host/SNI；
- `/nix-cache-info` 的 200、非 200、超时和响应体错误；
- NAR 206 Range、200 full response、404、连接中断。

### streaming 测试

- `HttpChunkConnector` 后续 chunk 保持同一 endpoint client；
- 多 chunk 返回内容拼接完整；
- endpoint 切换策略不会产生重复、缺口或乱序字节；
- logical-host throttling 仍然生效。

### 集成测试

- 一个 substituter、多个 endpoint；
- 多个 substituter，其中只有 `cache.nixos.org` 启用 endpoint selection；
- endpoint 故障后回退同 substituter，再回退其他 substituter；
- 持久化 NAR location 跨重启后不携带过期 endpoint IP。

## 13. 待维护者决定的问题

以下问题没有足够证据在交接阶段设置默认值，应由 selector4nix 维护者结合现有策略决定：

- endpoint 选择是每次 NAR 固定一次，还是允许文件间动态调整；
- metadata 延迟与 NAR 吞吐分别采用何种平滑或衰减方法；
- 主动探测频率、样本窗口、失败退避和状态过期时间；
- 是否允许自动生成 Discussion #511 的区域候选；
- IPv4 和 IPv6 是否使用独立候选池及 Happy Eyeballs；
- dashboard 是否允许手动禁用、固定或重新探测 endpoint。

无论如何选择，都应保持以下不变量：原始 host/SNI、严格证书校验、真实 NAR 指标、单 endpoint 故障隔离，以及未启用功能时完全兼容。

## 14. 一句话交接

将 Fastly 优选实现为 `selector4nix` 中 `Substituter` 下的 endpoint-aware transport：候选 IP 只改变连接地址，不改变 URL/Host/SNI；先以 TLS 和 `/nix-cache-info` 准入，再用真实 NAR 传输指标选择，并让所有 Range chunk 沿用同一端点。

