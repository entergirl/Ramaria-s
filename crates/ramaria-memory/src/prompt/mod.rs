//! crates/ramaria-memory/src/prompt/mod.rs - System Prompt 构建模块
//!
//! 设计特点:
//! - builder.rs: 四层 System Prompt 动态装配（v3.1 §8.2：角色/说话风格/知识/记忆）
//! - layers.rs: 四层注入结构与预算分配器（脉络独立预算 ≤ 30%）
//! - example_selector.rs: Few-shot 示例筛选 (按话题标签/情绪效价/消息长度)
//! - injection_guard.rs: 线上记忆注入开关 (控制 Block C 是否发送到线上 LLM)
//! - 数据来源: persona_facts + personality_traits + Persona-Aware RAG 检索结果

pub mod builder;
pub mod example_selector;
pub mod injection_guard;
pub mod layers;

/// Prompt 模板版本常量（缓存失效键组成部分；决策记录见 docs/dev-1.5/v1.5-decisions.md）。
///
/// 约定:
/// - 随 `prompt/builder.rs` / `prompt/layers.rs` 的模板结构或语义变更时递增，
///   格式 `YYYYMMDD-vX.Y.Z`（日期 + 语义版本）。
/// - 参与 LLM 精确缓存 key 构造（`sha256(model_id + template_version + prompt)`），
///   模板变更后旧缓存自动失效，防止跨版本误命中（回归红线：缓存命中不改变输出语义）。
///
/// 修改提醒:
/// - 修改本常量时同步检查 `ramaria-app` / `ramaria-memory` 各 ChatRequest 构造点
///   是否统一引用本常量（不应散落字面量）。
/// - v1.5 M6 递增记录：行为层槽位填充（`render_behavior_block` 渲染 `## 行为规则`
///   小节，模板结构变更）→ 旧缓存自动失效，防跨版本误命中。
pub const PROMPT_TEMPLATE_VERSION: &str = "20260814-v1.5.1";
