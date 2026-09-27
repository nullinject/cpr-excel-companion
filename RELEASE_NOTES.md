# v0.1.1 实验性预发布

面向未修改的官方 CPR 3.16.0 的 Excel 中间件插件和独立桥接服务，协议转换移植自 Kaixxrua/excel-codex-bridge。需要同时安装两部分，保留普通模型名。

此前 0.1.0 原型已在官方宿主验证 HTTP 请求、原生 WebSocket 上游、计费记录、客户端 WebSocket 和续接。0.1.1 增加管理页、策略与队列模块、逐消息签名；新增功能尚未完成端到端验收。

**不适合直接替换多用户生产服务。** 多 Key 隔离、代理自动同步、严格拒绝语义和完整管理联动尚未验收，详见 README。当前发布不意味着已切换线上服务。

验证：cargo test --workspace --locked（90 项含 SDK 和文档测试）；cargo clippy --workspace --all-targets --locked -- -D warnings；前端 pnpm build / pnpm lint；pnpm audit --prod 未发现已知漏洞。

资产提供 Linux x86_64 插件包、独立桥接程序和 SHA256SUMS。安装需要配置签名密钥、私有账户代理映射、上游路由和可写策略目录；没有一键生产安装器。
