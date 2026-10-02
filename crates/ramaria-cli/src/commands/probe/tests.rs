//! crates/ramaria-cli/src/commands/probe/tests.rs - probe 命令单元测试入口
//!
//! 设计特点:
//! - 按域拆分子模块：数据集构建与档位样例 / 消融与评分 / 报告统计与知识层 / 报告渲染
//! - 子模块经 `super::super::` 访问 probe 私有子模块，保持内联测试的私有访问等价
//! - 用例名与断言零改写，仅 `use` 路径随文件层级调整

mod dataset;
mod report_render;
mod report_stats;
mod scoring;
