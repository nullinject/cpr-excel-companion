# Key × 模型 × 通道

本开发候选直接应用 key_rules：已认证 Client Key ID + 基础模型规则优先，其次 model_channels，最后客户端 -excel 后缀。规则值仅为 native / excel；删除条目表示继承。全局 enabled、原有模型/账户范围和 Excel 并发/队列仍然生效；宿主的认证、模型权限、账户选择与用量控制不变。403 不重试、不换通道。

前端可选择全局或具体 Key、批量设定或恢复继承，并预览原名和后缀请求。缺失目录的 Key 规则保留供显式处理，不会因此使该 Key 获得访问权。模型列表只来自已保存的全局/Key 规则和请求记录，不代表 BPS 实时可用性；模型权限必须通过对应账户实测。

已移除执行路径从未使用的 client_keys 字段及 permits_request。此候选不兼容包含 client_keys 的旧策略 JSON；加载会明确失败而非静默丢弃。发布时须先审核旧规则，显式删除废弃字段，并保留现有 models/accounts、队列设置和版本并发控制。不要把清理后的策略当作授权配置。当前代码尚未部署。

测试：cargo test --workspace --locked --offline；cargo clippy --workspace --all-targets --locked --offline -- -D warnings；前端 pnpm test/typecheck/lint/build。
