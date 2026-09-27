# CPR Excel Companion

**v0.4.0：官方 CPR 3.16.0 插件 + 独立桥接服务。数据面经 CPR 原生 Provider 执行，计费与用量统计完整；已在生产环境通过端到端验收（2026-09-27）。**

协议转换移植自 [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)。无需修改 CPR 源码；由插件、桥接服务与上游路由三部分组成。

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
