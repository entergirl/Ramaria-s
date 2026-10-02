//! crates/ramaria-memory/src/chat/mod.rs - 在线对话装配编排模块（记忆层）
//!
//! 设计特点:
//! - 装配素材加载：persona 结构化画像 / 事实 / 性格 / 示例 / 自动风格规则 / 知识层去重
//! - 普通装配路径：结构化素材走 5-Block 装配器，纯文本降级（无 persona / 冷启动）原样返回
//! - 冷启动兜底：persona 无结构化画像时经 persona.toml 组装基础 prompt（DB 配置优先，文件系统回退）
//! - 示例预选：评分轮换 + 记忆未命中兜底；关闭时回退静态 selected 注入
//! - 与传输无关：storage / 配置经参数注入，供 app 与 service / MCP 入口同源复用
//! - 安全约束：不记录完整 prompt 或用户消息；日志只记数量与计数
//! - 素材加载/示例预选/脉络/冷启动兜底按职责拆入子模块，本文件仅保留声明与逐项 re-export

mod examples;
mod material;
mod narrative;
mod persona_fallback;
mod time;

#[cfg(test)]
mod tests;

pub use examples::load_examples_for_input;
pub use material::{
    LoadedPromptMaterial, PromptMaterialInputs, build_system_prompt, load_prompt_material,
};
pub use narrative::{NarrativeMaterial, format_l1_as_context_line, load_narrative_material};
pub use persona_fallback::load_persona_toml_prompt;
pub use time::now_timestamp_str;
