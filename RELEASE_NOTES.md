## 1.0.0-rc.10 · 2026-10-23

- 还原 SIWC 的 `resource` 为 `https://api.openai.com/v1`：`https://bps.openai.com` 被授权端判定为 invalid_authorize_request。
- 会话新增 `source` 字段；`excel-addin` 来源（从 Excel 加载项缓存导入）无需 direct 权限即可用于 Excel 通道。
- SIWC 的 `resource` 改为 `https://bps.openai.com`：BPS 的访问执行按 token 受众匹配，
  之前用 `https://api.openai.com/v1` 签发，BPS 直接返回 `no_matching_rule`。
- **改用 Excel 加载项同款授权（SIWC）**：授权端点改为 `auth.openai.com/api/accounts/authorize`，
  动态客户端 + PKCE，scope 含 `chatgpt.tokens.use.direct`，resource 为 `https://api.openai.com/v1`。
  上一版误用 Codex OAuth（`app_EMoam…`），签发的 token 不被 BPS 接受。
- 旧版本保存的 Codex 会话缺少 direct 权限，升级后自动视为未授权，需要在开关上重新授权一次。
- 授权链接改为可见可复制：插件页在 sandbox iframe 中运行，`window.open` 会被拦截，
  现在直接显示授权地址并提供「复制」按钮，复制后在浏览器新标签页打开。
- 授权面板拆成两步提示（复制链接 → 粘贴回调地址），不再声称已自动打开页面。
- **账户级 BPS 授权开关**：账户信息列表每行新增一个开关，开启后给出 ChatGPT 授权链接（Codex 官方 OAuth，PKCE），
  把回调地址粘回页面即完成绑定；关闭即解除。授权按 CPR 账户 ID 隔离，保存到策略文件同目录的 `account-auth.json`（`0600`）。
- 账户信息改为显示**邮箱**（缺失时退回名称，最后才是截断的账户 ID），不再直接暴露 `acct_…` 主键。
- 已授权账户的 Excel 请求改用该会话的 `Authorization` 与账号身份头覆盖宿主注入值；未授权账户行为不变。
- 新增管理接口：`GET api/auth/status`、`POST api/auth/start`、`POST api/auth/complete`、`POST api/auth/logout`。
- 授权失败、state 过期与文件不可写均返回明确错误，不静默回退到宿主凭据。
- 撤销 rc.4 的按钮式交互（改用开关）；rc.4、rc.5 均已从 Releases 撤回（rc.5 误打包了旧前端产物）。

同步到 CPR 插件 SDK v3.29.7（提交 `78bc57c5cf24b01a38e985cdd739bb4ae89ce11f`）。此版本是发布候选，不是稳定 1.0。

- 清单升级：`upstream_adapter` v1 → v4，`management` v1 → v2，`observer` v1 → v2；进程协议 v5，宿主范围收紧为 `>=3.29.7, <4.0.0`。
- 适配 `UpstreamAdapterRequest.fast_mode` 三态，替换旧的 `disable_fast` 布尔语义；宿主冻结的 Fast 策略仍由宿主在 attempt 前应用，插件不二次改写。
- 请求与响应头改用 `(String, Vec<u8>)` 字节形式，保留重复头与非 UTF-8 值；附件上传与受管 HTTP/WebSocket 出站同步调整。
- `service_tier` 从事件顶层移入 `ResponseObservation`，同时上报请求 ID 与上游回显模型；原始观测与标准事实分离传递。
- WebSocket 握手结果携带 `http_version` 与拒绝时的 trailers，匹配 v4 升级枚举。
- 工具链固定 Rust 1.99.1，`Atomic::fetch_update` 改用 `try_update`；SDK 内置测试随上游同步。
- 兼容性测试改为覆盖 3.29.7 起的宿主序列，并断言三项能力版本。

升级说明：宿主须为 v3.29.7 或更高；v3.18.x 宿主请继续使用 1.0.0-rc.1。早先按 3.18.2 SDK 构建、版本号同为 1.0.0-rc.2 的制品与本版内容不同，宿主禁止同版本替换，因此本版改用 1.0.0-rc.10。切换插件代次会使已有续接材料失效，客户端需重发完整历史。

## 1.0.0-rc.1 · 2026-09-30

此版本是发布候选，不是稳定 1.0；未宣称解决宿主的长流截止时间问题。

- Key × 基础模型 × 通道：已认证 Key 覆盖 → 全局模型覆盖 → 客户端后缀，不改变宿主认证、账号和模型授权；403 不自动换通道。
- 删除旧 client_keys 策略字段，未知字段显式失败；迁移非空限制前不得删字段或启用默认策略。
- 修复 Vue 字典更新导致路由预览/保存状态不刷新的问题，防止特殊 Key/模型名污染原型。
- 修复原生工具历史误封装与缓存未命中丢失；保留合法的客户端工具输入和权限错误分类。
- Excel 推理强度明确校验，支持 none/low/medium/high/xhigh，未知值不再静默降为 medium。
- 非对象请求在修改续接字段前拒绝，避免畸形输入触发 panic。
- 统一包版本、Rust 格式及 Clippy 检查，移除重复的旧前端构建产物。
- 已观察到 CPR 3.18.2 受管 HTTP 链路在持续输出后约 120 秒超时，同服务器直接 BPS 输出可持续到测试端的 210 秒限制。宿主修复及健康检查恢复前不发布稳定 1.0。

## 0.8.9 · 2026-09-29

- 撤销 `0bd540f` 引入的提示词改写和桥接策略重试循环。
- 新增默认关闭的 `policy_errors_as_server_error`：仅将 Excel 通道的明确 policy 错误映射为普通 `server_error`，交由调用方现有机制决定重试；原始诊断日志、usage 与部分输出保留，桥接不额外发请求。
- 升级须同时更新桥接与插件，并通过宿主正式安装/版本切换接口部署，不直接写插件数据库记录；配置默认关闭，不自动更改账户、模型或现有开关。保留现有更新来源绑定，GitHub 来源使用同一仓库的正式 Release。

## Bridge hotfix — 2026-09-28

- Fix streamed failure classification: an unknown provider code no longer hides a recognized error type; support flat error code/type fields.
- Add a request ID to Excel failed responses and log safe error-field shapes. Keep raw error text and credentials out of responses and diagnostics.
- Keep failed streams failed; do not replay partially delivered output or change native routing, authentication, or plugin configuration.

## 0.8.7 · 2026-09-28

- 修复请求监控在 Token 用量为 null 时整页渲染失败；未知用量显示为「—」，真实零用量仍显示为 0。
- 请求表新增「首字 / 完成」与 tok/s，保留宿主上报的真实计时；缺失数据不补造，输出速率按输出 Tokens ÷（完成耗时 − 首字耗时）计算。
- 不改变请求路由、续接、账户权限或宿主用量结算。

## 原生续接修复 · 2026-09-28

- 原生通道的 WebSocket 请求保持与上游同一连接，原样传递 previous_response_id，不再改用 HTTP 造成续接拒绝。
- 补齐原生模型目录转发，沿用账户认证与出站代理；不新增模型或修改 Key 权限。
- HTTP 收到无法续接的 ID 时明确要求完整历史，绝不静默删除续接参数。

## 0.8.6 · 2026-09-28

- 管理页改为紧凑请求表与四类设置，移除概览大卡片、重复标题和说明侧栏。
- 未修改时不显示保存栏；保留草稿、冲突检查、请求繁忙保护和账户白名单语义。
- 包含前一提交的管理目录分页和保存状态修复；不改 CPR 宿主或认证与模型授权。

# v0.8.2 真实 Codex 兼容修复

- Excel 通道的 `generate=false` 预热在本地完成，返回空输出和零用量，并保留有界续接历史。修复原生 HTTP 透传返回 `Unsupported parameter: generate`。
- 官方 CPR 3.16.0 仅绑定 attempt 中间件，避开 request 阶段 WebSocket 帧类型不匹配。升级旧实例时移除 request 绑定，保留 attempt 与两类 observation 绑定。
- 移植原项目的工具封装解码：对象、代码围栏、嵌套传输及无效反斜杠；仅解析数据，仍拒绝未声明工具，不执行外层 JavaScript。
- SSE 解码及工具转换失败写入监控失败原因，并发送明确的失败终态，避免表现为无原因断流；保留 v0.8.1 的 SSE 保活与 v0.8.0 的协议转换。
- 部署要求 `openai.ws_pool.enabled: true`。不要向账户凭据 JSON 添加未定义的 `websockets` 字段，会导致整个凭据解析失败。

验证：Rust 工作区 106 项测试通过，Clippy 零警告，Linux release 构建成功。实际 Codex CLI 0.158.0-alpha.2.1 使用真实 Key 验证 gpt-6-sol / gpt-6-astra 的写文件、执行测试、恢复会话和上下文记忆。账户容量和上游服务错误仍可能出现，不能保证永不重试。

旧版本升级需同时更新桥接程序和插件绑定；只上传插件无法替换桥接二进制。操作前保留账户、策略与旧二进制备份。

---

# v0.4.0 混合架构

官方 CPR 3.16.0 + attempt 插件 + 独立桥接。数据面经 CPR 原生 Provider 执行：认证、调度、重试、用量结算完整保留。控制面（管理页、观察归并）由插件进程内直连桥接。

## 相对 0.1.1 的变更

- `-excel` 后缀模型自动识别并签名。宿主按请求元数据覆写正文 model 字段，插件只判定不改写，后缀由桥接还原。
- 声明 request_lifecycle 与 usage 观察：宿主终态（真实 client_key 身份、上游模型、错误码）转发到桥接，归并进请求记录。
- 桥接新增 `/_control/observe`；连接失败、上游拒绝等失败路径的原因写入记录并返回给宿主。
- 插件控制通道改为进程内直连（trustedProcess 原生 TCP）。宿主受管 HTTP 禁止回环与私网地址，共享 netns 部署只能直连。
- 桥接补齐原项目的 x-stainless 指纹头；UA 经 `EXCEL_BRIDGE_UPSTREAM_UA` 覆盖。
- 账户映射容忍同步脚本的 version 元数据字段。

## 部署形态（生产实测 2026-09-27）

- CPR 官方镜像 ghcr.io/zyycn/codex-proxy-rs:3.16.0，`openai.api.base_url` 指向反代公网路由。
- 桥接以 `network_mode: service:codex-proxy-rs` 共享网络命名空间，端口由 CPR 服务发布；CPR 容器重建后需重建桥接容器。
- 账户映射由 systemd timer 每 5 分钟从 CPR 数据库同步；挂载目录而非单文件。
- 第一代 local.excel-bps 插件实例已停用：官方宿主的 envelope 校验拒绝短路响应。

## 验证

生产实测通过：原生透传、Excel 非流式（sol/terra）、Excel 流式（astra，WebSocket 上游）、model_requests 用量与计费、侧边栏观察归并。`cargo test --workspace --locked` 与 `cargo clippy -D warnings` 全绿；前端 pnpm build / lint 通过。
## 0.8.8 · 2026-09-28

- 原提示词改写与桥接内策略重试实现已于 2026-09-29 撤销，请勿依据此旧条目启用或重新安装该实验版本。
