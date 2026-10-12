//! crates/ramaria-service/src/recall/mod.rs - 召回用例（memory_recall 的服务层实现）
//!
//! 设计特点:
//! - 召回同源：记忆层检索走 `ramaria_memory::recall::assemble_recall`（与在线管线同一份
//!   实现），服务层只负责"分层装配 + 预算裁剪 + 结构化输出"
//! - 分层装配按注入优先级排列：行为 > 知识 > 表达（风格） > 脉络 > 记忆（L1/L2/L3） > 原文
//! - 两种模式：`Search`（有检索输入）与 `Overview`（无 query 且无 messages，按时间线概览）
//! - 隐私策略在服务层执行：人格白名单（`allowed_personas`）与原文开关（`allow_raw_text`）
//!   不满足时直接拒绝或不出原文，保证「策略执行」不依赖协议壳
//! - 预算纪律：`max_items` 上限 20（超出截断）、`max_chars` 按字符边界截断，均计入 stats
//! - 静默降级：知识 / 风格 / 画像 / 行为读取失败均记 warn 并按空处理，不阻塞召回
//!
//! 未接线说明:
//! - `RecallRequest.conversation_id`（"当前外部对话库内历史参与检索去重"）当前仅作数据属性
//!   透传，尚未参与检索去重；接线点为 `list_messages_by_channel_ref`。
//!
//! 模块划分:
//! - `policy`：召回隐私与边界策略（配置映射 / 链式覆盖 / 白名单判定）；
//! - `entry`：用例入口（策略校验、请求归一化、检索 / 概览分流）；
//! - `search`：检索模式分层装配与预算裁剪（记忆层共用召回）；
//! - `overview`：概览模式时间线装配与渲染；
//! - `layers`：各辅助分层读取与条目映射（行为 / 知识 / 风格 / 脉络 / 画像）；
//! - 模块级辅助：人格类型显式解析（按 persona 行 `kind` 读取，供召回装配使用）。

mod entry;
mod layers;
mod overview;
mod policy;
mod search;

use ramaria_core::traits::StorageBackend;
use ramaria_core::types::PersonaKind;

// 策略 re-export：`crate::recall::RecallPolicy` 为引擎与入口层既有调用路径
pub use policy::RecallPolicy;

// 用例入口 re-export：`crate::recall::run` 为引擎既有调用路径
pub(crate) use entry::run;

// =========================================================
// 辅助
// =========================================================

/// 解析 persona 行的显式类型（未知 / 读取失败 → `None`，保守降级）。
///
/// 职责:
/// - 按 persona 行 `kind` 解析目标人格类型（显式载体），供召回装配的
///   Persona-Aware 阈值与 utt 原文白名单使用。
///
/// 返回:
/// - 行存在 → `Some(kind)`；
/// - 行缺失 / 读取失败 → 记 warn 并返回 `None`（不阻塞召回）。
pub(crate) async fn resolve_persona_kind(
    storage: &dyn StorageBackend,
    uid: &str,
) -> Option<PersonaKind> {
    match storage.get_persona_by_uid(uid).await {
        Ok(Some(persona)) => Some(persona.kind),
        Ok(None) => {
            tracing::warn!(persona = %uid, "persona 行不存在，类型按未知处理");
            None
        }
        Err(e) => {
            tracing::warn!(persona = %uid, error = %e, "persona 类型读取失败，按未知处理");
            None
        }
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
