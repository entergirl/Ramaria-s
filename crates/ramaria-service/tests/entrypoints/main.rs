//! crates/ramaria-service/tests/entrypoints/main.rs - 多入口装配集成测试入口
//!
//! 设计特点:
//! - 覆盖三种入口装配形态（桌面完整钩子链 + 生命周期 / CLI 默认装配 / MCP 轻量钩子链 + 空闲循环）
//!   在同一数据库上的组合行为：并发封存抢占、写入与召回一致、跨引擎回流可见
//! - 覆盖入口侧收敛路径：缺索引版本的启动自愈与对话前自愈、封存许可与原文闸门
//! - 依赖全部离线确定：真实 SQLite 临时库 + 脚本化 LLM + 预计算嵌入，不触网、不加载模型
//! - 运行方式：`cargo test -j 2 -p ramaria-service --test entrypoints`
//! - 日志开关：设置环境变量 `PARITY_LOG=1` 后运行，用例 tracing 日志输出到 stderr

mod concurrent_engines;
mod cross_entry_visibility;
mod gates;
mod missing_index;
mod support;
