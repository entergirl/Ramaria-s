//! crates/ramaria-core/src/types/mod.rs - Ramaria 核心业务数据类型模块入口
//!
//! 设计特点:
//! - 覆盖核心领域对象: Session、Message、MemoryL1、MemoryEvent、Persona、PersonalityTrait 等
//! - 完整 Persona 体系: 枚举与结构体覆盖人格注册、事件提取、性格推断全链路
//! - ID 双轨制: TEXT 主键表使用 UUID v4，INTEGER AUTOINCREMENT 表使用 i64
//! - 统一时间规范: 所有时间使用 Unix 毫秒时间戳，存储层以 INTEGER 形式持久化
//! - 按领域拆分子模块并逐项 re-export，保持原有深层导入路径稳定

use uuid::Uuid;

/// 创建一个新的 UUID v4。
///
/// 用法:
/// - sessions / messages / memory_l1 等 TEXT 主键表创建 ID 时优先使用。
///
/// 返回:
/// - 新的 UUID v4。
#[inline]
pub fn new_id() -> Uuid {
    Uuid::new_v4()
}

/// 将 UUID 格式化为 SQLite TEXT 兼容的字符串。
///
/// 返回:
/// - UUID 的小写 hex 字符串，如 `"550e8400-e29b-41d4-a716-446655440000"`。
#[inline]
pub fn uuid_to_db(u: Uuid) -> String {
    u.to_string()
}

/// 从 SQLite TEXT 解析 UUID。
///
/// 返回:
/// - 成功时返回 Ok(UUID)。
/// - 解析失败时返回 `Err(RamariaError::Validation)`，携带 trace_id 和原始值。
///
/// 说明:
/// - 存储层可能读到历史遗留的非法数据，此处返回明确的错误而非静默降级。
/// - 调用方应记录 WARNING 日志并传播错误，以便上层统一处理数据一致性问题。
#[inline]
pub fn uuid_from_db(s: &str) -> crate::error::RamariaResult<Uuid> {
    Uuid::parse_str(s).map_err(|_| {
        crate::error::RamariaError::validation(format!(
            "UUID 解析失败: 数据库中存储了非法 UUID 值 '{s}'，可能由历史数据损坏或 bug 引起"
        ))
    })
}

/// 获取当前 Unix 毫秒时间戳。
///
/// 返回当前 Unix 毫秒时间戳。
///
/// 用法:
/// - 所有业务实体创建、更新、访问时间优先使用此函数。
///
/// 返回:
/// - 当前 Unix 毫秒时间戳。
///
/// 说明:
/// - 核心层不依赖 tokio、网络或数据库，因此使用标准库 `SystemTime`。
/// - 若系统时钟在 UNIX_EPOCH 之前（极度异常），返回 0。
///   上层应在发现时间戳为 0 时记录 ERROR 日志。
#[inline]
pub fn now_ms() -> i64 {
    use std::time::SystemTime;
    match SystemTime::now().duration_since(SystemTime::UNIX_EPOCH) {
        Ok(d) => d.as_millis() as i64,
        Err(_) => {
            // 系统时钟异常——返回 0 作为哨兵值，上层需检测并告警
            0
        }
    }
}

mod attachments;
mod backend;
mod inbound;
mod memory;
mod message;
mod persona_enum;
mod persona_struct;
mod session;
mod state;
mod style;
mod utt;

pub use attachments::{
    AttachmentStatus, MessageAttachment, build_render_map, image_placeholder_hash,
    is_local_relative_ref, replace_image_placeholders,
};
pub use backend::{BackendConfig, LlmProvider, ModelCapability, PrivacyConsent};
pub use inbound::{InboundAttachmentKind, InboundAttachmentRef, InboundMessage, InboundSender};
pub use memory::{
    ClusterSnapshot, EventBatchWrite, EventRelation, EventRelationKind, EventSource, EvidenceNote,
    MemoryEvent, MemoryL1, PersonaEventAggregate,
};
pub use message::{Message, MessageKey, MessageRole, MessageSource};
pub use persona_enum::{
    EvidenceDirection, FactSource, FactStatus, FactTier, PersonaKind, Presentation, ProfileField,
    TraitLayer, TraitSource, TraitStatus,
};
pub use persona_struct::{Persona, PersonaFact, PersonalityTrait, TraitEvidence};
pub use session::{CHANNEL_LOCAL, CHANNEL_QQ, MemberRole, Session, SessionMember};
pub use state::AppState;
pub use style::{PersonaExample, PersonaStyleStats, StyleRuleSource, StyleStatsStatus};
pub use utt::UttBlock;

#[cfg(test)]
mod tests;
