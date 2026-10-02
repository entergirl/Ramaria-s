//! crates/ramaria-core/src/types/utt.rs - Ramaria 原文话语块数据类型模块
//!
//! 设计特点:
//! - 定义 utt 话语块（原文切分后的最小检索单元）
//! - 绑定 session / persona 与消息序号范围
//! - 提供构造与时间辅助方法
//! - 支持 serde，供索引构建与检索层共享
//! - 时间统一使用 Unix 毫秒时间戳

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::now_ms;

// =========================================================
// utt 话语块（v1.4 新增 — 原文注入通道的最小单元）
// =========================================================

/// utt 话语块——原文按连续性切分的最小注入单元（v1.4 A1）。
///
/// 职责:
/// - 承载一次会话中按时间间隙/条数上限切分出的连续原文片段。
/// - 作为对话时【原文片段】注入、跨会话桥接与未来风格统计的原料底座。
///
/// 字段约定:
/// - `persona_uid`: 块归属人格，原文按 persona 严格隔离（隐私约束）。
/// - `block_text`: 块内原文全文，按原文格式拼接（含发言人标记），不写日志。
/// - `embedding`: 块文本向量（f32 小端 BLOB），`None` 表示未生成或 embedding 不可用。
///
/// 安全约束:
/// - 原文是最高敏感层：注入受 `persona_kind_whitelist` 约束，内容不记录日志。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UttBlock {
    /// 内部索引（INTEGER AUTOINCREMENT，0 表示尚未入库）
    pub id: i64,
    /// 块归属人格 UID
    pub persona_uid: String,
    /// 来源会话 ID
    pub session_id: Uuid,
    /// 块内首条消息 ID
    pub start_msg_id: Uuid,
    /// 块内末条消息 ID
    pub end_msg_id: Uuid,
    /// 块内原文全文
    pub block_text: String,
    /// 块内消息条数
    pub msg_count: u32,
    /// 首末消息时间跨度（毫秒）
    pub time_span_ms: i64,
    /// 块文本向量（f32 小端 BLOB），None 表示未生成
    pub embedding: Option<Vec<u8>>,
    /// 创建时间（Unix 毫秒）
    pub created_at: i64,
}

impl UttBlock {
    /// 创建新话语块（id=0，由存储层回填）。
    ///
    /// 参数:
    /// - `persona_uid`: 块归属人格。
    /// - `session_id`: 来源会话。
    /// - `start_msg_id` / `end_msg_id`: 消息区间。
    /// - `block_text`: 原文全文。
    /// - `msg_count`: 消息条数。
    /// - `time_span_ms`: 时间跨度。
    ///
    /// 返回:
    /// - `embedding=None`、`created_at=当前时间` 的 UttBlock。
    pub fn new(
        persona_uid: String,
        session_id: Uuid,
        start_msg_id: Uuid,
        end_msg_id: Uuid,
        block_text: String,
        msg_count: u32,
        time_span_ms: i64,
    ) -> Self {
        Self {
            id: 0,
            persona_uid,
            session_id,
            start_msg_id,
            end_msg_id,
            block_text,
            msg_count,
            time_span_ms,
            embedding: None,
            created_at: now_ms(),
        }
    }
}
