# CPR Excel Companion

**v0.8.2：官方 CPR 3.16.0 插件 + 独立桥接服务。数据面经 CPR 原生 Provider 执行，计费与用量统计完整；已在生产环境通过端到端验收（2026-09-27）。**

协议转换移植自 [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)。无需修改 CPR 源码；由插件、桥接服务与上游路由三部分组成。

## v0.6.2 修复：HTTP 请求压缩

CPR 的 Codex HTTP 上游会发送 `Content-Encoding: zstd`。此前桥接直接将压缩正文按 JSON 解析，导致 `400 invalid JSON request`，客户端可能随之重连；这类错误不能作为模型能力下降的证据。

桥接现在先解压再解析，同时从重新序列化的上游请求中去掉旧 `Content-Encoding` / `Content-Length`。普通与 Excel 通道均适用。压缩前、解压后正文上限均为 32 MiB，zstd 窗口上限为 32 MiB；损坏压缩包返回 400，超限返回 413，不支持或叠加的编码返回 415。

同时修复未签名透传请求缺省 `stream` 字段时发生 panic、导致连接中断的问题。

本修复需要更新并重启**桥接服务二进制**；仅更新 CPR 侧插件不会修复旧桥接。回归测试包含真实 HTTP 监听器下未签名、签名原生、签名 Excel 三条路径，不会请求真实上游。

## v0.8.2 桥接：真实 Codex 客户端兼容

- `generate=false` 预热在桥接本地建立有界续接历史，返回空输出、零用量；不向 Excel 发起生成请求。
- 补齐原项目工具封装的兼容解码，保留已声明工具校验；流解析或工具转换失败时发送失败终态并保留原因，避免无原因断流。
- 官方 CPR 3.16.0 部署必须保持 `openai.ws_pool.enabled: true`。关闭连接池会让 Codex 的首次 WebSocket 请求就收到 `previous_response_not_found` / `pool_unavailable`，随后反复重试和回退；普通 HTTP 探活无法发现此问题。
- 插件仅绑定 `attempt` 和观察事件。官方 3.16.0 的 `request` 中间件 WebSocket 帧投影会导致 `request middleware returned an invalid response`；旧安装需移除该 request 绑定。该阶段也未提供客户端 Key，移除不会损失可用的身份信息。
- 账户并发占满时应配置 CPR 原生 `maxWaitingPerAccount` / `concurrencyWaitTimeoutSeconds`。桥接排队发生在宿主账户选择之后，无法兜底宿主提前返回的 503；本次线上使用等待上限 8、超时 120 秒，账户并发上限仍为 3。按自己的负载调整。
- 不要在账户凭据 JSON 中添加 `websockets` 字段：3.16.0 拒绝未知字段，导致凭据解析失败和 503。通过官方账户设置管理传输方式。
- 原生 HTTP 上游不接受 `generate=false`，因此 Excel 预热在本地完成，不能简单改走原生 HTTP。

### 真实 Codex 验收（2026-09-27）

Codex Desktop CLI 0.158.0-alpha.2.1 使用真实 Key，各模型 3 轮：创建文件、执行测试、`exec resume` 续接、修改代码、再次测试和回忆前文标记。Sol 三轮全部完成且零重连；Astra 三轮完成，但第一轮发生 TLS 握手 EOF 重试，用时约 293 秒，后两轮正常。因此本版本不能承诺没有传输错误。此前 180 秒超时及账户并发 3/3 引起的 503 也保留为失败证据，不以最终成功抹除。

## 工作方式

```text
客户端 ──▶ CPR（官方 3.16.0）
              │ attempt 插件：签发 HMAC 上下文，识别 -excel 后缀模型
              │ openai.api.base_url 指向桥接
              ▼
          桥接服务 ──Excel 请求──▶ bps.openai.com（按账户映射代理回源，出口 IP 与原生一致）
              │──其余请求──▶ chatgpt.com（原样透传）
```

- **请求走 CPR 原生 Provider**：认证、账号调度、重试、用量结算全部保留。`-excel` 后缀模型与普通模型混用，无需 separate key 或单独入口；`model_requests` 表同时记录两类请求的完整 usage。
- **插件只做两件事**：为每条 attempt 消息签发 HMAC 上下文（账户、作用域、excel 位、有效期 15 分钟）；把宿主最终用量/失败观察转发给桥接，归并出真实 client_key 身份与错误码。
- **桥接做协议转换**：Responses → Basispoints，含流式/非流式、图片与文件上传、function/custom 工具与命名空间、加密 reasoning 历史重放。上游拒绝原因透传并写入请求记录。
- **出口一致**：桥接按账户映射选择 socks5h 代理回源 BPS；映射由定时任务从 CPR 数据库同步（`deploy/sync-account-map.py`）。
- **管理页**：CPR 插件侧边栏展示请求监控（状态、排队、耗时、tokens、上游模型、错误码），数据来自桥接快照与观察归并。
- OAuth 凭据由宿主保管，经 `host.auth.get` 随请求取用，不写入配置或日志。

## 三条宿主约束（设计依据）

这些行为来自官方 3.16.0 源码，决定了本项目的形态：

1. attempt 中间件的响应必须携带 Provider envelope（`gateway-core/engine/provider.rs`），短路响应被协议校验拒绝。因此插件不能进程内直连 BPS，必须让原生 Provider 执行——这也正是计费完整的原因。
2. Provider 把账户出口代理无条件套用到 base_url 连接（`Proxy::all`，无回环豁免）。桥接必须经账户代理可达：部署时通过反代公网路由（如 Caddy `/excel-companion/*`）回源，桥接再按映射代理回 BPS。
3. 宿主按请求元数据覆写正文 `model` 字段，插件层改写无效。`-excel` 后缀由桥接的 `prepare()` 还原为上游模型名。

## 按 Key × 模型切换 Excel / 原生

三个层级，全部可用：

1. **按 Key（哪些 Key 能用 Excel）**：CPR 插件实例的「绑定」`clientKeyIds`——宿主持有 key 身份并按绑定过滤，绑定的 key 触发签名，未绑定的 key 由桥接原生透传（自动还原 `-excel` 后缀名）。在 CPR 插件实例编辑器中修改，保存即生效。
2. **按模型（全局默认通道）**：侧栏「模型通道」按基础模型名三态切换（跟随后缀 / Excel / 原生）。"Excel" = 无后缀也强制走 Excel；"原生" = 带 `-excel` 后缀也压回原生。预制四款模型（gpt-5.6-sol/terra/luna、gpt-6-astra），请求中出现的新模型自动补充。
3. **按请求（客户端选择）**：`-excel` 后缀 = Excel，原模型名 = 原生，同一个 key 下自由混用。

按 Key × 模型的精细矩阵 = 1 + 2/3 组合：绑定决定 key 是否进入 Excel 通道，模型通道/后缀决定具体走向。

> **为什么侧栏不能直接按 Key 切换**：官方 3.16.0 宿主对插件隐藏客户端身份——request 阶段头投影实测只含 `user-agent` 和 `content-type`（无 authorization），attempt 阶段头为空。key 身份只在宿主准入层（原生 key 模型规则/预算）与绑定匹配器中存在。桥接已内置按 Key 规则引擎（`key_rules`，key 摘要经签名上下文传递 + 观察归并学习映射），未来宿主一旦投影身份即可用，无需再改协议。

## 模型通道（按基础模型名的三态开关）

网关设置页对**基础模型名**（不带后缀）设置通道，三态语义：

| 状态 | 无后缀请求 | 带 `-excel` 后缀请求 |
| --- | --- | --- |
| 跟随后缀（默认） | 原生 | Excel |
| Excel | **强制 Excel** | Excel |
| 原生 | 原生 | **强制原生** |

"Excel" 态意味着客户端无需改模型名即可走 Excel；"原生" 态优先级高于后缀，客户端带后缀也会被压回。存储在桥接策略的 `model_channels`，侧栏保存即时生效。

## 验证记录（2026-09-27，生产实测）

原生透传（gpt-5.6-sol，13 input tokens）、Excel 非流式（gpt-5.6-sol/terra-excel，22356 input = BPS 固定前缀）、Excel 流式（gpt-6-astra-excel，SSE + [DONE]）、WebSocket 上游传输、CPR `model_requests` 用量与计费、侧边栏观察归并（真实 Key ID、上游模型、错误码）全部通过。

## 已知边界

1. **多 Key 隔离靠策略字段。** 桥接准入按模型/账户/Key 白黑名单与并发队列控制；同实例多租户混用未经隔离验收，Key 白名单不构成安全边界。
2. 队列拒绝返回 429，会触发 CPR 重试；提前失败的记录可能归类为取消。
3. 目录、配额等非生成路由未适配。
4. 反代/CDN 注入头（`cf-*`、`x-forwarded-*`、`cdn-loop`）由桥接剥离；若上游风控策略变化，透传请求可能需要更新剥离清单。

## 下载与安装

从 [Releases](https://github.com/nullinject/cpr-excel-companion/releases) 下载：

- `nullinject.excel-companion-*.tar.gz`：CPR 插件管理上传；trustedProcess 插件，权限 requests、network、data。
- `cpr-excel-companion-bridge-*.tar.gz`：Linux x86_64 桥接程序，构建基线 Debian Bookworm / Rust 1.97.1。
- `SHA256SUMS`：下载校验。

### 桥接服务

生成至少 32 字节随机密钥，只读挂载给插件进程与桥接。

| 变量 | 含义 |
| --- | --- |
| `EXCEL_BRIDGE_SECRET_FILE` | 共享签名密钥文件路径 |
| `EXCEL_BRIDGE_ACCOUNT_MAP` | 账户代理映射路径 |
| `EXCEL_BRIDGE_LISTEN` | 默认 `127.0.0.1:8089` |
| `EXCEL_BRIDGE_POLICY_FILE` | 持久化策略文件，默认 `bridge-policy.json` |
| `EXCEL_BRIDGE_UPSTREAM_UA` | 回源 User-Agent，默认 `Mozilla/5.0` |
| `EXCEL_BRIDGE_MODEL_SUFFIX` | Excel 模型后缀，默认 `-excel` |

账户映射示例（占位符）：

```json
{"accounts":{"CPR_ACCOUNT_ID":{"proxy":"socks5h://proxy.example.invalid:1080","direct":false}}}
```

无代理账户显式写 `{"direct":true}`；缺失账户或矛盾配置拒绝请求，不自动直连。更新映射用原子替换，且**挂载目录而非单文件**（rename 替换会使单文件 bind mount 失效）。

桥接端点：`GET /healthz`、`POST/WS /backend-api/codex/responses`、`POST /_control/{snapshot|policy|observe}`，均要求签名。

### CPR 插件配置

| 字段 | 含义 |
| --- | --- |
| `secretFile` | 插件进程可读的共享密钥路径，默认 `/run/secrets/excel-bridge.key` |
| `isolationScope` | 实例作用域，必填 |
| `excelEnabled` | 总开关，默认 true |
| `excelModelSuffix` | Excel 模型后缀，默认 `-excel` |
| `bridgeControlUrl` | 桥接控制地址，共享 netns 部署用 `http://127.0.0.1:8089/_control` |
| `showPage` | 管理页开关，默认 true |

上游路由：`openai.api.base_url` 指向桥接。宿主无回环豁免，base_url 必须经账户代理可达；共享 netns 直连仅适用于账户无代理的部署。桥接挂载用目录。CPR 容器重建后需重建桥接容器（`network_mode: service:` 的 netns 引用会失效）。

## 开发验证

```sh
cargo test --workspace --locked
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --workspace --release --locked
cd plugin/frontend
pnpm install --frozen-lockfile
pnpm build
pnpm lint
```

Rust 1.97.1，Node >=24 / pnpm 12.6.0。打包：官方 `cpr-plugin package --manifest plugin/plugin.json --binary target/release/cpr-excel-companion-plugin --target x86_64-unknown-linux-gnu --resource-map web=frontend/dist --output-dir plugin/dist`（CLI 与 vendored SDK 同为 v3.16.0 提交 0534dd8）。

不记录请求正文、文件内容或令牌；拒绝远程图片 URL；内联附件有大小限制。未包含原项目的登录发现功能或 OfficeJS 执行器。

## 许可

主体采用 Unlicense。第三方来源、固定版本及官方 SDK 的 Apache-2.0 许可见 [NOTICE.md](NOTICE.md) 和 `vendor/gateway-plugin-sdk/LICENSE`。
