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

- Added opt-in safe prompt translation for the latest user message and bounded retry for recognized pre-output policy rejections.
