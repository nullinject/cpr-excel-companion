# CPR Excel Companion

v1.0.0-rc.4 将 Excel 协议转换、附件上传、工具转换、续接缓存、路由设置和监控集成到一个 CPR 插件进程。无需独立桥接服务、监听端口、共享签名密钥或账号代理同步任务。

兼容 CPR `>=3.29.7, <4.0.0`。SDK 固定为官方 v3.29.7、提交 `78bc57c5cf24b01a38e985cdd739bb4ae89ce11f`。使用清单 v2、进程协议 v5、`upstream_adapter` v4、`management` v2、`observer` v2 完成适配器、管理页面与完成事件。

> 当前为候选版本，不是稳定 1.0。宿主 v3.29.x 已放宽插件处理预算与受管 HTTP 读取计时，长响应断流的宿主侧截止时间问题在 v3.29.7 上待复测。

## 工作方式

```text
客户端 → CPR（鉴权、模型授权、选号、账号租约）
            → Excel Companion（策略、协议转换）
            → CPR 受管 HTTP / WebSocket（账号认证、账号代理）
            → Excel / 原生上游
          ← 标准事实、原始响应、用量 → CPR 结算
```

插件默认不读取、不保存账号凭据，也不维护自己的账号代理客户端：原生与 Excel 两条通道都由宿主在出站时注入账号认证。只有在管理页对某个账户点「授权」后，插件才会为该账户保存一份独立授权会话（`account-auth.json`，`0600`），并仅用它覆盖该账户 Excel 请求的 `Authorization` 与账号身份头。附件上传声明为 auxiliary 出站，参与 CPR 的副作用记录。宿主负责重试；插件不自行重放失败请求或换号。管理页直接调用进程内策略与监控对象，账号和 Key 目录通过宿主 API 获取。

适配器覆盖绑定范围内的 OpenAI OAuth 请求，以保持管理页即时切换模型通道。原生客户端 WebSocket 使用宿主管理的上游 WebSocket，Excel 使用 HTTP/SSE。适配器声明 WebSocket 能力以允许二者；宿主的静态传输标签表示适配器声明，不代表每次出站都使用 WebSocket。

## 安装与迁移

1. 安装插件包。创建插件进程可读写的数据目录，迁移时将旧 `bridge-policy.json` 复制为该目录内的 `plugin-policy.json`，保留文件内容、模型通道与版本号。升级前清理已经停用的 client_keys 字段；未知配置字段会显式拒绝，不会静默丢弃。文件权限建议 `0600`，所有者为 CPR 运行用户。
2. 配置 `policyFile` 为容器内的绝对路径，例如 `/app/.runtime/data/excel-companion/plugin-policy.json`；`showPage` 控制侧栏显示。目录必须存在。无需 `secretFile`、`isolationScope`、`bridgeControlUrl`。
3. 将旧 attempt 绑定替换为 `nullinject.excel-companion.upstream-adapter`，`stage=upstream`、`failurePolicy=reject`，保留原 Key、账号组及模型范围，Provider 限定为 `openai`。保留 `nullinject.excel-companion.observer`、`stage=observation`、`event=request_completed`、`failurePolicy=observe`。
4. 将 CPR `openai.api.base_url` 恢复为 `https://chatgpt.com/backend-api`。模型目录等非生成接口由 CPR 原生处理；未命中插件绑定的生成请求也走 CPR 原生链路。
5. 验证两类通道、账号模型刷新、工具、附件及续接后，停用独立 bridge 容器、账户映射同步 timer，移除 bridge 服务配置及 `/excel-companion/*` 反代。保留旧配置和策略备份用于回滚。

切换插件代次会使已有续接材料失效，客户端应重新发送完整历史。不要将失效的 `previous_response_id` 迁入新实例。

## 路由与隔离

Key、账号组、Provider 和模型范围由 CPR 插件绑定控制。范围内支持 Key × 基础模型 × 通道矩阵：已认证 Key 的模型覆盖优先，其次为全局模型覆盖，最后跟随客户端后缀。路由选择不会授予额外账号或模型权限，403 不自动切换通道。

| 模型通道 | 无后缀 | 带 `-excel` 后缀 |
| --- | --- | --- |
| 跟随客户端 | 原生 | Excel |
| Excel | Excel | Excel |
| 原生 | 原生 | 原生 |

Excel 的账户范围、模型范围及并发队列仍由管理页配置。宿主提供请求的真实 Key ID，续接缓存按插件实例、代次、进程、Key、账号和凭据版本隔离。Excel 历史缓存最多 32 条、每条 2 MiB、30 分钟；超限或失效须发送完整历史。原生 WebSocket 续接使用宿主保留的同一连接；原生 HTTP 不支持跨请求的连接续接。

## 功能与边界

- Responses 流式/非流式、function/custom/namespace 工具转换及加密 reasoning 历史重放沿用原转换器。插件不执行工具。
- 内联图片与文件通过同一账号的受管出站上传；每个附件不超过 20 MiB，不接受远程图片 URL。
- 管理页展示最近请求、用量、排队与错误；记录在内存中，插件重启后清空。策略持久化在 `policyFile`，有版本冲突检查。
- Excel 本地预热返回零用量；无终态 EOF 不伪造成功。
- `policy_errors_as_server_error` 默认关闭，沿用旧策略；开启只转换错误分类，不能保证宿主/客户端不会重试或产生重复费用。
- Excel 推理强度支持 none/low/medium/high/xhigh；省略时为 medium。ultra 或未知值明确报错，不再静默降级。
- 旧桥接程序源码和历史部署脚本仍保留供回滚参考，插件运行不调用它们。旧说明见 [legacy-bridge.md](docs/legacy-bridge.md)。

## 开发验证

Rust 1.99.1（`rust-toolchain.toml` 固定），Node >=24，pnpm 12.6.0。

```sh
cargo test --workspace --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo build --release --locked -p cpr-excel-companion-plugin
cd plugin/frontend
pnpm install --frozen-lockfile
pnpm test
pnpm lint
pnpm build
```

前端以 `plugin/frontend` 为唯一来源，不提交重复的构建产物。用官方打包器映射构建目录：

```sh
cpr-plugin package --manifest plugin/plugin.json \
  --binary target/x86_64-unknown-linux-gnu/release/cpr-excel-companion-plugin \
  --target x86_64-unknown-linux-gnu --resource-map web=frontend/dist \
  --output-dir dist
```

安装前通过目标宿主的上传校验接口验证安装包。升级前若旧策略存在 `client_keys`，只在其 allow/deny 均为空时删除该废弃字段；非空时必须先将限制显式迁移到 CPR 原生绑定，不能丢弃限制。

协议转换移植自 [Kaixxrua/excel-codex-bridge](https://github.com/Kaixxrua/excel-codex-bridge)。
