//! crates/ramaria-service/src/export.rs - 会话导出数据装配用例
//!
//! 设计特点:
//! - 只做数据装配：按会话收集全量消息与人格关联的未吸收 L1 摘要；
//!   JSON / Markdown 文本生成与文件写出属入口能力（两侧渲染结构不同，不在本层统一）
//! - 人格过滤：仅保留含目标 persona_uid 消息的会话（消息集合保持全量，不按消息二次过滤）
//! - 分页：作用于过滤后的会话集合（offset 缺省 0；limit 缺省全量，`Some(0)` 按下界 1 处理）
//! - 计数口径：`total_sessions` 为过滤前全部会话数，`sessions` 为过滤并分页后的装配结果
//! - 隐私：日志只记计数，消息正文与摘要不进日志

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{MemoryL1, Message, Session};

use crate::engine::Engine;

// =========================================================
// 请求与结果类型
// =========================================================

/// 会话导出数据装配请求。
///
/// 字段约定:
/// - `persona`: 人格过滤（`None` = 不过滤）；仅保留含目标 persona_uid 消息的会话。
/// - `limit`: 会话条数上限（`None` = 全部；`Some(0)` 按下界 1 处理）。
/// - `offset`: 分页偏移（缺省 0）。
#[derive(Debug, Clone, Default)]
pub struct ExportDataRequest {
    pub persona: Option<String>,
    pub limit: Option<u32>,
    pub offset: Option<u32>,
}

/// 单个会话的导出数据。
///
/// 字段约定:
/// - `session`: 会话核心行（起止时间 / 人格归属 / 通道等信息）。
/// - `messages`: 会话全量消息（时间正序）。
#[derive(Debug, Clone)]
pub struct ExportSessionData {
    pub session: Session,
    pub messages: Vec<Message>,
}

/// 导出数据装配结果。
///
/// 字段约定:
/// - `total_sessions`: 过滤前的全部会话数（分页无关）。
/// - `sessions`: 过滤并分页后的会话数据（含各自全量消息）。
/// - `l1_memories`: 指定人格时的未吸收 L1 摘要段（`None` = 未指定人格；
///   空列表 = 该人格无未吸收摘要）。
#[derive(Debug, Clone)]
pub struct ExportData {
    pub total_sessions: usize,
    pub sessions: Vec<ExportSessionData>,
    pub l1_memories: Option<Vec<MemoryL1>>,
}

// =========================================================
// 装配用例
// =========================================================

/// 装配会话导出数据（会话集合 + 消息 + 人格 L1 摘要段）。
///
/// 流程:
/// 1. 读取全部会话并逐会话读取全量消息；
/// 2. 人格过滤：仅保留含目标 persona_uid 消息的会话；
/// 3. 分页：对过滤后的会话集合应用 offset / limit；
/// 4. 指定人格时读取该人格的未吸收 L1 摘要（空列表为正常空态）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 装配请求（人格过滤 / 分页）。
///
/// 返回:
/// - `ExportData`：过滤前总数、过滤并分页后的会话数据与可选 L1 摘要段。
pub(crate) async fn collect(engine: &Engine, req: ExportDataRequest) -> RamariaResult<ExportData> {
    let storage = engine.storage_ref();
    let all_sessions = storage.list_sessions().await?;
    let total_sessions = all_sessions.len();

    // ---- 逐会话装配（过滤需要先读消息判定归属；消息集合保持全量） ----
    let mut filtered: Vec<ExportSessionData> = Vec::with_capacity(all_sessions.len());
    for session in all_sessions {
        let messages = storage.list_messages(session.id).await?;
        if let Some(persona) = req.persona.as_deref() {
            let matched = messages
                .iter()
                .any(|message| message.persona_uid.as_deref() == Some(persona));
            if !matched {
                continue;
            }
        }
        filtered.push(ExportSessionData { session, messages });
    }

    // ---- 分页（作用于过滤后的会话集合） ----
    let offset = req.offset.unwrap_or(0) as usize;
    let limit = req
        .limit
        .map(|limit| limit.max(1) as usize)
        .unwrap_or(usize::MAX);
    let sessions: Vec<ExportSessionData> = filtered.into_iter().skip(offset).take(limit).collect();
    let message_count: usize = sessions.iter().map(|session| session.messages.len()).sum();

    // ---- 人格 L1 摘要段（仅指定人格时装配） ----
    let l1_memories = match req.persona.as_deref() {
        Some(persona) => Some(storage.list_unabsorbed_l1(persona).await?),
        None => None,
    };

    tracing::debug!(
        total_sessions,
        exported_sessions = sessions.len(),
        message_count,
        persona_filter = req.persona.is_some(),
        "会话导出数据装配完成"
    );

    Ok(ExportData {
        total_sessions,
        sessions,
        l1_memories,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{engine_with_db, seed_l1, seed_persona, seed_session_with_messages};

    /// 空库：装配返回空集合（非错误），无人格过滤时不带 L1 段。
    #[tokio::test]
    async fn collect_empty_db_returns_empty_result() {
        let (engine, _storage, dir) = engine_with_db("export-empty").await;

        let data = engine
            .export_sessions(ExportDataRequest::default())
            .await
            .expect("空库装配应成功");
        assert_eq!(data.total_sessions, 0);
        assert!(data.sessions.is_empty());
        assert!(data.l1_memories.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 无过滤：全部会话与全量消息逐会话装配（总数与消息数口径）。
    #[tokio::test]
    async fn collect_without_filter_exports_all_sessions() {
        let (engine, storage, dir) = engine_with_db("export-all").await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;
        seed_session_with_messages(&storage, "char-0002", 2, 2_000).await;

        let data = engine
            .export_sessions(ExportDataRequest::default())
            .await
            .expect("装配应成功");
        assert_eq!(data.total_sessions, 2);
        assert_eq!(data.sessions.len(), 2);
        assert!(
            data.sessions.iter().all(|s| !s.messages.is_empty()),
            "每个会话应装配全量消息"
        );
        let message_total: usize = data.sessions.iter().map(|s| s.messages.len()).sum();
        assert_eq!(message_total, 5, "消息数应为两会话之和");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 人格过滤：仅保留含目标 persona_uid 消息的会话；指定人格时装配 L1 段。
    #[tokio::test]
    async fn collect_filters_by_persona_and_includes_l1() {
        let (engine, storage, dir) = engine_with_db("export-filter").await;
        seed_persona(&storage, "char-0001").await;
        seed_persona(&storage, "char-0002").await;
        seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;
        seed_session_with_messages(&storage, "char-0002", 2, 2_000).await;
        // seed_l1 会为承载摘要自动建一个无消息会话（外键依赖）：总数计 3 个会话
        seed_l1(
            &storage,
            "char-0001",
            "工作压力摘要",
            Some("工作压力"),
            3_000,
        )
        .await;

        let data = engine
            .export_sessions(ExportDataRequest {
                persona: Some("char-0001".to_string()),
                limit: None,
                offset: None,
            })
            .await
            .expect("装配应成功");
        assert_eq!(data.total_sessions, 3, "总数口径为过滤前全部会话");
        assert_eq!(
            data.sessions.len(),
            1,
            "仅保留含匹配人格消息的会话（无消息会话不计入）"
        );
        assert!(
            data.sessions[0]
                .messages
                .iter()
                .all(|m| m.persona_uid.as_deref() == Some("char-0001")),
            "过滤后的会话应只含目标人格相关数据"
        );
        let l1 = data.l1_memories.expect("指定人格时应装配 L1 段");
        assert_eq!(l1.len(), 1);
        assert_eq!(l1[0].summary, "工作压力摘要");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 分页：offset / limit 作用于过滤后的会话集合；总数仍为过滤前口径。
    #[tokio::test]
    async fn collect_pages_filtered_sessions() {
        let (engine, storage, dir) = engine_with_db("export-page").await;
        seed_persona(&storage, "char-0001").await;
        seed_session_with_messages(&storage, "char-0001", 1, 1_000).await;
        seed_session_with_messages(&storage, "char-0001", 1, 2_000).await;
        seed_session_with_messages(&storage, "char-0001", 1, 3_000).await;

        let data = engine
            .export_sessions(ExportDataRequest {
                persona: None,
                limit: Some(1),
                offset: Some(1),
            })
            .await
            .expect("装配应成功");
        assert_eq!(data.total_sessions, 3, "总数不随分页变化");
        assert_eq!(data.sessions.len(), 1, "offset 1 + limit 1 应只余一条");

        // 无人格指定：不带 L1 段
        assert!(data.l1_memories.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
