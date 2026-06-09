//! rust/crates/ramaria-memory/src/prompt/mod.rs - System Prompt 构建模块
//!
//! 设计特点:
//! - builder.rs: 5-Block System Prompt 动态装配 (角色定义/Few-shot/记忆/知识边界/上下文)
//! - example_selector.rs: Few-shot 示例筛选 (按话题标签/情绪效价/消息长度)
//! - 数据来源: persona_facts + personality_traits + Persona-Aware RAG 检索结果

// 待实现: pub mod builder; pub mod example_selector;
