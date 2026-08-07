//! rust/crates/ramaria-memory/src/prompt/mod.rs - System Prompt 构建模块
//!
//! 设计特点:
//! - builder.rs: 四层 System Prompt 动态装配（v3.1 §8.2：角色/说话风格/知识/记忆）
//! - layers.rs: 四层注入结构与预算分配器（脉络独立预算 ≤ 30%，T-V14-6-002）
//! - example_selector.rs: Few-shot 示例筛选 (按话题标签/情绪效价/消息长度)
//! - injection_guard.rs: 线上记忆注入开关 (控制 Block C 是否发送到线上 LLM)
//! - 数据来源: persona_facts + personality_traits + Persona-Aware RAG 检索结果

pub mod builder;
pub mod example_selector;
pub mod injection_guard;
pub mod layers;
