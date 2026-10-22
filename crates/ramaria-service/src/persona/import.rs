//! crates/ramaria-service/src/persona/import.rs - Ramaria 预设导入模块
//!
//! 设计特点:
//! - 两条用例：dry-run 预览（不调用 LLM、不写库）与导入执行（解析 → 可选拆解 → 计划 → 写入）
//! - dry-run 与实写共用同一裁决数据源（`plan_import`），保证「将写入」逐项一致
//! - 导入永不失败：拆解失败 / 隐私未确认 / 条目写入失败均降级，persona 照常建成
//! - 注册列失败上抛（人格记录是根基）；L3 条目逐条独立，写入失败计数不中断其余条目
//! - 拆解受隐私门禁前置：未确认时不调用 LLM，仅保留原文进 `personas.config`
//! - uid 缺省为自动生成（文件名 stem 仅 `persona reload` 目录通道使用）
//! - 隐私：日志与错误消息不含预设原文；日志中的个人标识经 `mask_id` 脱敏

mod decompose;
mod execute;
mod parse;
mod preview;
mod write;

pub(crate) use execute::execute;
pub(crate) use preview::preview;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
