//! crates/ramaria-importer/src/qq/parser/mod.rs - QQ 聊天记录解析核心
//!
//! 设计特点:
//! - 仅支持 shuakami/qq-chat-exporter v6.x JSON 格式（语义化 type 名称）
//! - 完整覆盖 qce v6.x 全部 10 种语义化消息类型（text/reply/audio/json/file/video/forward + type_10/type_19 + system）
//! - 消息指纹: SHA-256 前 16 位 hex，用于跨导入批次的重复检测
//! - 编码兼容: 支持 UTF-8/UTF-8-BOM/UTF-16-LE/GBK/Latin-1 多编码自动检测
//! - 角色映射: 双前缀模式——导出者也加 [{self_name}] 前缀，消除"用户 vs 助手"误导
//! - Session 切割: 按 gap_minutes 时间间隔将消息流切割为独立会话
//!
//! 模块分层:
//! - detect: 格式检测与多编码解码；time: Unix 毫秒时间戳日期换算
//! - elements: 元素助手与指纹；message: 单条消息解析与角色映射
//! - sessions: 会话切割；stream: 流式解析驱动与对外解析入口

mod detect;
mod elements;
mod message;
mod sessions;
mod stream;
mod time;

pub use detect::detect_qq_format;
pub use stream::parse_qq_export;

// 单元测试跨子模块引用的私有项（测试构建下对 `qq::parser::tests` 可见）
#[cfg(test)]
use elements::{
    extract_reply_body, fallback_image_placeholder, image_element_infos, json_element_description,
    make_fingerprint, normalize_source_ref, render_image_placeholders,
};
#[cfg(test)]
use message::parse_json_message;
#[cfg(test)]
use sessions::split_into_sessions;
#[cfg(test)]
use stream::aggregate_members;
#[cfg(test)]
use time::ts_ms_to_date;

#[cfg(test)]
mod tests;
