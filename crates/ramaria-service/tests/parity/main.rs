//! crates/ramaria-service/tests/parity/main.rs - 平行对照测试目标入口（四条关键路径）
//!
//! 设计特点:
//! - 目的：为封存 / 召回 / chat / 索引重建四条关键路径提供"同输入 → 独立执行 → 输出等价"
//!   的对照测试基建
//! - 基线：四条路径各自的规范化快照冻结为 golden 基线（`tests/parity/golden/`），
//!   产出变更以"与基线逐字段一致"回归
//! - 对比口径：同一 fixture 下产出的同形状快照，两侧可直接送入 `assert_parity` 比对
//! - 确定性纪律：真实 SQLite 临时库 + 脚本化 mock LLM + 预计算向量 mock 嵌入；
//!   不依赖网络、真实模型与系统时钟绝对值（fixture 时间取"当前时间 - 固定偏移"）
//! - 失败可诊断：对照不等价时输出逐路径差异报告（路径 / 左值 / 右值），
//!   并提示 golden 基线的更新方式
//! - 运行方式：`cargo test -j 2 -p ramaria-service --test parity`
//! - 日志开关：设置环境变量 `PARITY_LOG=1`（或 `debug` / `trace`）后运行，
//!   服务层用例的 tracing 日志输出到 stderr（默认静默，避免成功路径噪声）

mod chat;
mod index;
mod recall;
mod seal;
mod support;
