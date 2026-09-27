# CPR Excel Companion

**实验版 v0.1.1：官方 CPR 3.16.0 插件 + 独立 Excel 桥接服务。尚未完成生产验收，请在隔离环境使用。**

协议转换移植自 [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)，不是 SUB2api。此项目无需修改 CPR 源码，但不是仅安装插件即可使用：还必须运行桥接服务并配置上游路由。

## 工作方式

CPR 认证客户端、选择账户后，插件签发上下文并调用官方 `next`；CPR 原生 Provider 将请求交给桥接服务，再由桥接服务转换为 Excel/Basispoints 协议。普通模型名称保持不变，无需 `-excel` 后缀。OAuth 凭据从当前账户请求获取，不写入配置或日志。

## 验证范围

- 0.1.0 原型在**未修改的官方 CPR 3.16.0** 中验证了 HTTP 客户端请求、原生 WebSocket 上游、CPR 用量/计费记录。
- 验证了客户端 WebSocket 和 `previous_response_id` 两轮续接。
- 0.1.1 新增管理页、策略持久化、请求记录、并发/FIFO 队列，以及每条 WebSocket 请求的签名上下文。这些新增能力通过编译和单元测试，**尚未完成官方宿主端到端验收**。
- 图片、文件和工具协议转换已有实现；不代表所有客户端组合均已实测。

## 已知限制

1. **多 Key 隔离未完成。** 当前 `isolationScope` 为手工配置，同一实例不能混用多个租户。官方宿主限制同插件仅一个启用配置，不能靠多个实例解决。管理页的 Key 白名单不可视为已经验证的安全边界。
2. 账户代理使用私有 JSON 映射；自动同步 CPR 代理配置未完成，实际网络出口尚未抓包验收。
3. 管理页、账户/模型/Key 策略与全局开关联动尚未完成端到端验收。
4. 队列拒绝返回 429，可能触发 CPR 重试；严格“直接拒绝”尚未验证。部分提前失败的监控记录可能归类为取消。
5. 目录、配额等非生成路由未完整适配；不要直接替换生产环境全局上游地址。
6. 连接池复用时的逐消息签名、长期 WebSocket 策略更新仍需实际宿主回归。

## 下载与安装

从 [Releases](https://github.com/nullinject/cpr-excel-companion/releases) 下载：

- `nullinject.excel-companion-*.tar.gz`：在 CPR 插件管理中上传；这是 trustedProcess 插件，需要允许清单中的 requests、network、data 权限。
- `cpr-excel-companion-bridge-*.tar.gz`：独立 Linux x86_64 桥接程序，开发构建基于 Debian Bookworm / Rust 1.97.1，需要兼容的 glibc 和系统 CA 证书。
- `SHA256SUMS`：下载校验。

当前提供手工集成，不提供已验收的一键生产安装器。安装插件后还需以下配置。

### 桥接服务

生成至少 32 字节随机密钥，作为私有文件只读挂载到插件进程和桥接服务。不要提交密钥、账户映射或运行时策略数据。

环境变量：

| 变量 | 含义 |
| --- | --- |
| `EXCEL_BRIDGE_SECRET_FILE` | 共享签名密钥文件路径 |
| `EXCEL_BRIDGE_ACCOUNT_MAP` | 私有账户代理映射路径 |
| `EXCEL_BRIDGE_LISTEN` | 默认 `127.0.0.1:8089` |
| `EXCEL_BRIDGE_POLICY_FILE` | 持久化策略文件；默认 `bridge-policy.json`，父目录必须可写 |

账户映射示例（全部为占位符）：

```json
{"accounts":{"CPR_ACCOUNT_ID":{"proxy":"socks5h://proxy.example.invalid:1080","direct":false}}}
```

无代理时显式使用 `{"direct":true}`。缺失账户或矛盾配置会拒绝请求，不会自动直连。更新映射时使用原子文件替换。

桥接端点：`GET /healthz`、`POST/WS /backend-api/codex/responses`、`POST /_control/{operation}`。生成与控制请求都要求签名。控制接口支持 `snapshot` 和 `policy`；策略默认关闭。通过 HTTPS 反代提供控制接口时，路径前缀需要在转发前去掉。

### CPR 插件

插件配置字段：

- `secretFile`：插件进程可读取的共享密钥路径，默认 `/run/secrets/excel-bridge.key`。
- `isolationScope`：独立测试租户的固定作用域，必填。
- `excelEnabled`：默认 false。
- `bridgeControlUrl`：例如 `https://bridge.example.invalid/_control`。
- `showPage`：是否展示管理页，默认 true。

在隔离的 CPR 环境将生成上游路由到桥接服务，保留原生 Provider 的认证、调度和响应解析。Docker 中 `127.0.0.1` 是当前容器；使用共享网络或可达的服务地址。若 CPR 自身经账户代理访问上游，桥接地址也必须经该代理可达。不要为连接桥接而直接移除生产账户代理。

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

Rust 基线 1.97.1，前端 Node >=24 / pnpm 12.6.0。插件打包使用官方 `cpr-plugin package`，清单为 `plugin/plugin.json`，资源映射 `web=web`（相对于清单目录）。

不记录请求正文、文件内容或令牌。拒绝远程图片 URL；内联附件有大小限制。未包含原项目的登录发现功能或 OfficeJS 执行器。

## 许可

主体采用 Unlicense。第三方来源、固定版本及官方 SDK 的 Apache-2.0 许可见 [NOTICE.md](NOTICE.md) 和 `vendor/gateway-plugin-sdk/LICENSE`。
