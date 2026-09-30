# CPR Excel Companion

**v0.9.0 插件 + v0.8.4 独立桥接：适配 CPR >=3.18.2、<3.19.0。数据面经 CPR 原生 Provider 执行，保留计费与用量统计链路；验收范围和已知限制见下文。**

协议转换移植自 [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)。无需修改 CPR 源码；由插件、桥接服务与上游路由三部分组成。

## 宿主兼容范围

当前插件固定采用 CPR v3.18.2（`e30aad475560b94db2d999e2251d180e45d52671`）的 SDK：清单 v2、进程协议 v2、中间件 v3、统一 `observer` 完成事件。旧的 `permissions`、`request_lifecycle` 和 `usage` 声明不再使用。完成事件中的 `usage.failure` 投影到桥接现有的错误字段，终态、用量与耗时仍来自宿主。

安装 0.9.0 时必须同时迁移绑定：保留原 `attempt` 范围，将原完成／用量观察合并为 `nullinject.excel-companion.observer`、`stage=observation`、`failurePolicy=observe`、`event=request_completed`。不能仅修改旧包的兼容版本号。旧的 3.16 / 3.17 宿主继续使用对应旧版插件。

桥接使用独立 Docker Compose 网络，不能再使用 `network_mode: service:codex-proxy-rs`。插件控制地址为 `http://excel-companion-bridge:8089/_control`；桥接端口仅发布到宿主回环（例如 `127.0.0.1:8093:8089`），由反代路由访问。CPR 的 `openai.api.base_url` 保持已配置的桥接地址。这样宿主内部更新、容器重启不会让桥接留在旧网络命名空间，导致模型列表和请求入口返回 502。

CPR 3.18.2 的 `upstream_adapter` 可提供受管 HTTP/SSE、账号代理、认证、结算与续接基础，已具备去掉外置转发链路的接口条件。当前版本仍保留桥接的附件上传、工具转换、流式解析及续接缓存；这些路径完整迁移并验证前，不停用桥接或把 Base URL 切回原生地址。

### v0.8.9：可选 Policy 错误分类

在「Excel 网关 → 执行限制」开启 `policy_errors_as_server_error`，可将 Excel 通道的 `cyber_policy`、`bio_policy`、`misalignment_policy_violation`、`content_filter` 转为普通 `server_error`：非 2xx HTTP 错误返回 HTTP 500；流式错误输出 `response.failed`，`response.incomplete/content_filter` 也转换为失败终态。默认关闭，缺少此字段的旧配置保持原行为；原生通道、认证错误、限流、`invalid_prompt` 和其他未完成原因不变。

这只是错误分类转换，客户端/宿主是否重试及重试上限取决于其既有逻辑。桥接不增加请求、不改写提示词、不切换账户，已有输出和 usage 不丢弃；重试可能重复计费或重复执行已输出的工具操作，开启前应确认调用方的重试行为。转换前原始错误仍由现有私有脱敏日志保存。部署须同时更新桥接和插件前端；旧桥接会拒绝未知设置，不会静默启用。不要通过直接写数据库或替换运行时临时插件目录安装，使用宿主正式安装/版本切换接口。

插件不再把宿主锁在 3.16.x：清单接受 `>=3.16.0, <4.0.0` 的稳定版本，避免同主版本升级时因过窄声明直接停用。此范围是兼容策略，不代表每个未来 3.x 版本都已实测；协议版本、能力声明、权限与平台校验仍由宿主执行。4.x 和预发布宿主需单独验证。此次仅调整插件安装包与兼容性测试，不更改桥接数据面，也不申请新增预算管理或账号权限。

## v0.6.2 修复：HTTP 请求压缩

CPR 的 Codex HTTP 上游会发送 `Content-Encoding: zstd`。此前桥接直接将压缩正文按 JSON 解析，导致 `400 invalid JSON request`，客户端可能随之重连；这类错误不能作为模型能力下降的证据。

桥接现在先解压再解析，同时从重新序列化的上游请求中去掉旧 `Content-Encoding` / `Content-Length`。普通与 Excel 通道均适用。压缩前、解压后正文上限均为 32 MiB，zstd 窗口上限为 32 MiB；损坏压缩包返回 400，超限返回 413，不支持或叠加的编码返回 415。

同时修复未签名透传请求缺省 `stream` 字段时发生 panic、导致连接中断的问题。

本修复需要更新并重启**桥接服务二进制**；仅更新 CPR 侧插件不会修复旧桥接。回归测试包含真实 HTTP 监听器下未签名、签名原生、签名 Excel 三条路径，不会请求真实上游。

## 流式终态修复（本地已验证，待部署）

原提示 “Excel upstream did not complete the response” 由桥接在收到上游 error、response.failed 或 response.incomplete 时统一生成，并不等同于一次 TCP 断线。旧逻辑丢弃了错误分类、未完成原因和终态 usage，无法据此判断线上是限流、上下文超限还是服务错误。

- 对已知错误码保留分类，并生成固定脱敏说明；不转发原始错误正文、提示词或凭据。不认识的代码仍标为未分类，不能据此推断根因。
- 2026-09-28 热修复：默认保留 cyber_policy、bio_policy、misalignment_policy_violation、invalid_prompt、content_filter、server_overloaded、usage_not_included。策略错误继续返回失败、不伪造成功，桥接不自动重放；默认关闭的新分类选项见上文。cyber_policy 原分类路径已用真实 Codex 0.157.1 配合同输入 SSE 探针验证：直接显示策略原因，不再因分类丢失误报断流并重连。其余新增映射由回归测试覆盖；历史未分类事件不能追溯判定为某一种策略错误。
- 保留 response.incomplete、max_output_tokens / content_filter 原因、已有文本和 usage；不伪造完成，也不把未完成工具参数发送成可执行调用。
- 保留已创建的 response ID；有效终态后不再追加第二个失败；无终态 EOF 和截断 SSE 明确失败。原生链路的顶层 error 也作为终态处理。
- 监控记录保留安全错误码及未完成原因，宿主没有新错误码时不清空已记录的原因；补充仅含事件类型、已知分类、耗时的日志，不记录请求正文或原始错误。
- 未新增自动重放或换账号，避免部分输出或工具调用后的重复执行。认证、账户路由和并发限制不变。

以上为普通回归测试验证，不是对线上错误率的保证。此修复涉及独立桥接二进制，仅升级 CPR 插件不能替换正在运行的旧 Bridge。

## v0.8.4：Codex exec 内部工具路由

- 修复已在 `functions.exec` 描述中显式声明的内部对象参数 API（例如 `mcp__codex_app__list_threads`）被上游误放进外层 references 后触发 `upstream requested an undeclared tool` 的问题。
- 顶层工具匹配仍优先。仅当本轮声明了 custom `functions.exec`，且其描述包含该 API 的精确 TypeScript 对象参数声明时，转换成同一个已声明 `functions.exec` 的 custom 调用；桥接不执行工具，不新增顶层权限。
- 参数通过 JSON 字符串 + `JSON.parse` 传递，避免把引号、换行或 `__proto__` 当成 JavaScript 代码/原型设置。未知名称、仅在描述正文提及的名称、非法标识符、非对象参数和 `tool_choice=none` 仍拒绝。
- 历史尾部补充顶层/内部工具的路由提示；工具名诊断只记录名称和有界目录样本，不记录参数、代码、请求正文或凭据。
- 流式转换只输出一次 custom 调用，保留 call_id，并以原生 run_officejs 身份重放后续工具结果。没有加入自动重试，也不会执行重复调用。

## v0.8.3：工具中继与账户出口修复

- 续接缓存过期、未命中或累计超过 2 MiB 时，HTTP / WebSocket 返回标准 `previous_response_not_found`（`param=previous_response_id`），由支持该协议的客户端移除旧 ID 并补发完整历史；桥接不会跨作用域取缓存、静默删历史或无限重试。
- 2 MiB 仍是单条进程内缓存上限（最多 32 条、30 分钟），不是模型上下文上限。超过缓存预算的完整历史允许无状态预热/重放，正文仍受 32 MiB 请求上限约束；不扩大缓存，也不自动做摘要。
- 新工具调用按 CPA 协议使用 references: [完整工具名]，code 仅承载 function 参数对象或 custom 原文；仍校验工具已声明。兼容已有嵌套封装，包括旧版本的空 references。
- 未命中插件绑定的原生请求不再借用第一个账户的代理。通过 chatgpt-account-id 精确匹配映射中的 upstream_account_id；缺失、未知或重复映射返回 503，绝不自动切换其他账户或直连。
- 升级次序：先更新桥接二进制，再更新并运行 deploy/sync-account-map.py。同步只读取启用的 OpenAI 账户的 ID、上游账户 ID 与出口代理，不读取 OAuth 凭据。
- 不修改官方 CPR 本体或账户传输设置；按 Key/账户组/模型筛选仍使用 CPR 原生插件绑定。代理不自动配置浏览器时区，本桥接没有浏览器运行时，不宣称仅凭代理就完成时区一致性验收。

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
客户端 ──▶ CPR（官方 3.18.2）
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

## Client Key 绑定与模型通道

无需修改 CPR。**Key 范围由 CPR 原生插件绑定决定，范围内共用模型通道规则**；这两个功能不能等同为任意的 Key × 模型独立矩阵。

- CPR 插件管理 → 当前配置 → 绑定：通过 clientKeyIds、账号组和模型范围筛选哪些请求进入插件。未匹配的请求走原生透传。
- 网关设置 → 模型与通道：按基础模型名选择“跟随客户端 / Excel / 原生”，查看原模型名与 -excel 后缀名各自的模型级规则；支持批量设置。
- 网关设置 → 并发与排队、账户范围：管理桥接执行容量和 Excel 账户范围。
- 安装、升级、授权和宿主绑定仍在 CPR 插件管理处理。插件页面没有修改自身绑定的宿主 API，不读取管理员凭据绕过页面隔离。

### 管理界面

- 请求监控提供实时 Excel 执行 / 排队数量、最近记录统计、完整 Key / 请求 ID 搜索、状态与通道筛选、分页和可展开的错误详情。统计仅覆盖内存中最近的记录，取消与失败分开显示。
- 网关设置按模型通道、并发排队、账户范围分区，固定保存栏提示未保存改动。刷新或切换页面不覆盖草稿；服务器版本发生冲突时明确提示重新载入，不静默覆盖。
- 桥接断连时保留已有内容并标记为缓存；恢复连接后仍保留草稿。账户与 Key 目录分别分页读取，单个目录失败不会清空另一个目录。
- 插件管理页的连接字段保留在原生配置中，并提供中文说明：签名密钥文件、插件隔离标识、桥接控制地址、侧栏开关。隔离标识不是 Client Key ID；侧栏开关不等于通道开关。
- 页面继承 CPR 的明暗主题，窄屏表格在自身区域横向滚动。认证、模型授权、配置版本检查和运行中禁止修改的约束保持不变。

### 早期版本的 Key 选择器

v0.1.1 确实有 Key 列表和 client_keys 配置，但当时 Control::enter 传给 permits_request 的是手工配置的 ctx.scope（isolationScope），并非宿主认证的请求 Key ID。同版本 README 已注明多 Key 隔离未完成。后续 key_rules 只是持久化字段，当前执行路径没有读取它；“规则引擎已内置、以后自动生效”的旧说明不准确。

现保留历史策略数据，不增加伪造的 Key 身份，不从完成后的用量观察猜测当前请求。已移除未注册的 request 阶段 Authorization 摘要提取；attempt 签名、账户校验和 CPR 授权保持不变。**本次设置页整理不宣称完成逐 Key 独立模型路由。**

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

1. **Key 准入由 CPR 授权与原生绑定负责。** 桥接只对已签名请求应用全局模型/账户范围及并发队列。历史 client_keys、key_rules 字段不参与当前路由，也不构成隔离边界；静态 isolationScope 不代表请求 Key。
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

桥接端点：`GET /healthz`、`POST/WS /backend-api/codex/responses`、`POST /_control/{snapshot|policy|observe}`，控制端点要求逐请求签名；healthz 为公开存活检查，生成端点可允许未签名原生透传（不允许 Excel 转换）。

### CPR 插件配置

| 字段 | 含义 |
| --- | --- |
| `secretFile` | 插件进程可读的共享密钥路径，默认 `/run/secrets/excel-bridge.key` |
| `isolationScope` | 实例作用域，必填 |
| `bridgeControlUrl` | 桥接控制地址；独立 Compose 网络使用 `http://excel-companion-bridge:8089/_control` |
| `showPage` | 管理页开关，默认 true |

上游路由：`openai.api.base_url` 指向桥接。宿主无回环豁免，base_url 必须经账户代理可达；桥接使用独立 Compose 网络，控制面通过服务名直连，数据面仍经既有反代入口。桥接挂载用目录。不要使用共享 netns，CPR 重启后该引用可能失效。

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

Rust 1.97.1，Node >=24 / pnpm 12.6.0。打包：官方 `cpr-plugin package --manifest plugin/plugin.json --binary target/release/cpr-excel-companion-plugin --target x86_64-unknown-linux-gnu --resource-map web=frontend/dist --output-dir plugin/dist`（vendored SDK 保持 v3.16.0 提交 0534dd8；0.8.9 使用官方 CLI d310286 打包，并通过 CPR 3.16.0 宿主安装包校验）。

默认不记录请求正文、文件内容或令牌；上游的可选短时工具失败诊断会将有限工具事件写入私有文件，仅在操作员显式配置后生效。拒绝远程图片 URL；内联附件有大小限制。未包含原项目的登录发现功能或 OfficeJS 执行器。

## 致谢

感谢以下开源项目及其作者：

- [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)：本项目 Excel 协议转换与工具调用适配的移植来源，感谢作者的原始实现与开源分享。
- [zyycn/codex-proxy-rs](https://github.com/zyycn/codex-proxy-rs)：提供 CPR 宿主、插件 SDK，以及账户调度、代理、计费和用量统计等基础能力。
- [zyycn/codex-proxy-plugins](https://github.com/zyycn/codex-proxy-plugins)：感谢作者公开官方插件模板与示例，为插件开发提供参考。

第三方代码的来源、版本与许可证详见 [NOTICE.md](NOTICE.md)。

## 许可

主体采用 Unlicense。第三方来源、固定版本及官方 SDK 的 Apache-2.0 许可见 [NOTICE.md](NOTICE.md) 和 `vendor/gateway-plugin-sdk/LICENSE`。


### 完整上游错误记录

Bridge 在协议转换/错误白名单映射之前，记录 error、response.failed、response.incomplete 和非 2xx HTTP 错误。文件位置为账户映射文件同级目录下的 error-logs/upstream-errors.jsonl，可按 request_id 精确关联。保留未知错误码、类型、完整多行消息、参数及嵌套错误字段；不再只记录字段形状，也不按 512 字符截断服务器记录。非 2xx 响应同时保存解析后的错误及完整文本。

仅服务器保存该诊断日志：目录 0700、文件 0600；单文件达到 32 MiB 时按完整记录轮转，保留当前文件与 5 个历史文件（单条超大错误不会被截断）。凭据字段、Bearer/Basic、常见 API key/JWT/私钥以及本次请求的真实认证值替换为 [REDACTED]。非错误的完整请求、请求头、工具目录和模型输出不额外归档；省略字段列于 omitted_non_error_fields。普通错误内容保持原样，脱敏后的记录不等同于未经处理的原始字节。

客户端错误分类、拒绝语义和重试策略保持不变，不向客户端或普通 Docker 日志公开完整错误。写入失败会输出不含错误载荷的 bridge error_record_failed，启动时检查日志文件可写。历史上已经丢弃的错误子码无法补回；此功能覆盖部署后的新请求。
