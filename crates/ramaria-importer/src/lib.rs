//! rust/crates/ramaria-importer/src/lib.rs - Ramaria 聊天记录导入器
//!
//! 设计特点:
//! - 通过 `ImportSource` trait 抽象导入源，支持 QQ/微信/Telegram 等格式扩展
//! - 当前版本仅支持 qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - 完整覆盖 qce v6.x 全部 10 种消息类型（text/reply/audio/json/file/video/forward/type_10/type_19/system）
//! - 快速导入模式：仅写入 L0 messages 表，适合快速查看历史对话
//! - 深度导入模式：逐 session 执行 L0→L1→L2→L3 全管线，生成完整记忆和性格画像
//! - Persona 归属：导入时自动查找或创建 source="qq" 的 persona
//! - SHA-256 指纹去重：防止同一文件重复导入

pub mod error;
pub mod qq;
pub mod traits;

pub use traits::{ImportMode, ImportReport, ImportSource, ImportedSession, ParsedMessage};
