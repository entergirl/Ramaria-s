//! rust/crates/ramaria-importer/src/lib.rs - Ramaria 聊天记录导入器（v1.1 新增）
//!
//! 设计特点:
//! - 通过 `ImportSource` trait 抽象导入源，支持 QQ/微信/Telegram 等格式扩展
//! - 快速导入模式：仅写入 L0 messages 表，适合快速查看历史对话
//! - 深度导入模式：逐 session 执行 L0→L1→L2→L3 全管线，生成完整记忆和性格画像
//! - Persona 归属：导入时用户手动指定，支持 (source, ref_id) 索引去重
//! - SHA-256 指纹去重：防止同一文件重复导入
//! - 编译期 feature gate：`importer` feature 控制是否编译此 crate

pub mod error;
pub mod qq;
pub mod traits;

pub use traits::{ImportMode, ImportReport, ImportSource, ImportedSession, ParsedMessage};
