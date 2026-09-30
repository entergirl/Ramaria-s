//! crates/ramaria-service/tests/suites/main.rs - 服务层用例集成测试入口
//!
//! 设计特点:
//! - 覆盖用例层跨模块的端到端行为：装配与状态 / 生成与事件流 / 封存与会话生命周期 /
//!   提示词分层装配 / 知识事实 / 行为规则 / 离线推断链路
//! - 依赖全部为 mock（内存存储 + 脚本化 LLM + 预计算嵌入），不触网、不加载模型、不写仓库外文件
//! - 断言只读取注入依赖的可观测状态，不绕过被测用例
//! - 运行方式：`cargo test -j 2 -p ramaria-service --test suites`

mod ablation_gate;
mod app_orchestration;
mod behavior;
mod bridge;
mod dedup_knowledge;
mod fact_extract;
mod inference;
mod knowledge;
mod prompt_template;
mod session_lifecycle;
mod smoke;
mod support;
