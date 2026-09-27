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
