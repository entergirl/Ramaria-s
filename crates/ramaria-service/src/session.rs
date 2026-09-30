//! crates/ramaria-service/src/session.rs - 会话读取与管理用例（chat_history / 创建 / 删除）
//!
//! 设计特点:
//! - 读取两条定位路径：显式 `session_id` 优先；否则取该人格最近一条消息所属会话
//! - 分页语义：存储层按 `created_at DESC` 取页（最新优先），返回前翻正为页内时间正序
//! - `total` 为该会话（或该人格消息）的总条数，供调用方判断是否还有更多历史
//! - 发送目标解析：交互式发送前的会话预检（已关闭 / 不存在时自动新建并绑定人格），
//!   查询失败保守沿用原会话，交由生成路径做最终校验
//! - 管理动作直通存储层：创建返回核心 `Session`（宿主自行映射响应结构），
//!   删除区分"仅会话行"与"级联清理关联数据"两种口径，错误语义与存储层一致
//! - 纯读取路径不修改任何状态；无数据时返回结构完整的空结果（不报错）

use chrono::{DateTime, Utc};
use ramaria_core::error::RamariaResult;
use ramaria_core::types::Session;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{HistoryMessageView, HistoryRequest, HistoryResult};

/// 读取会话历史（分页）。
///
/// 流程:
/// 1. `session_id` 提供 → 直接按会话分页读取；
/// 2. 否则按 `persona` 定位该人格最近一条消息所属会话（无消息 → 空结果）；
/// 3. 两条路径都缺失 → 空结果；
/// 4. 页内消息翻正为时间正序（便于调用方直接展示为对话流）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 历史请求（session_id 与 persona 二选一；limit / offset 可缺省）。
///
/// 返回:
/// - `session_id`（实际读取的会话，无数据时为 None）、`messages`、`total`。
pub(crate) async fn history(engine: &Engine, req: HistoryRequest) -> RamariaResult<HistoryResult> {
    let storage = engine.storage_ref();
    let limit = req.effective_limit() as i64;
    let offset = req.effective_offset() as i64;

    // ---- 1. 定位目标会话 ----
    let target: Option<Uuid> = match req.session_id {
        Some(sid) => Some(sid),
        None => match normalize_persona(req.persona.as_deref()) {
            Some(uid) => match storage.list_messages_by_persona_paginated(&uid, 1, 0).await {
                Ok(list) => list.first().map(|m| m.session_id),
                Err(e) => {
                    tracing::warn!(persona = %uid, error = %e, "定位近期会话失败，返回空历史");
                    None
                }
            },
            None => None,
        },
    };

    let Some(session_id) = target else {
        tracing::debug!("历史请求无可用会话定位，返回空结果");
        return Ok(HistoryResult {
            session_id: None,
            messages: Vec::new(),
            total: 0,
        });
    };

    // ---- 2. 分页读取（DESC 取页）+ 总条数 ----
    let mut page = storage
        .list_messages_paginated(session_id, limit, offset)
        .await?;
    let total = storage.count_messages(session_id).await? as usize;

    // 页内翻正为时间正序（存储层 DESC → 展示与消费按对话流顺序）
    page.reverse();

    let messages: Vec<HistoryMessageView> = page
        .iter()
        .map(|m| HistoryMessageView {
            role: m.role,
            content: m.content.clone(),
            time: DateTime::from_timestamp_millis(m.created_at)
                .unwrap_or(DateTime::<Utc>::UNIX_EPOCH),
            persona_uid: m.persona_uid.clone(),
        })
        .collect();

    tracing::debug!(
        %session_id,
        limit,
        offset,
        returned = messages.len(),
        total,
        "会话历史读取完成"
    );

    Ok(HistoryResult {
        session_id: Some(session_id),
        messages,
        total,
    })
}

// =========================================================
// 发送目标解析（交互式发送前置）
// =========================================================

/// 解析发送目标会话（会话预检与自动重建）。
///
/// 语义:
/// - 指定会话存在且未关闭 → 原样返回；
/// - 指定会话不存在或已关闭 → 新建会话（绑定调用方人格）并返回其 id；
/// - 存储查询失败 → 保守返回原会话，交由生成路径做最终校验；
/// - 未指定会话（`None`）→ 新建会话（绑定人格）并返回。
///
/// 用途:
/// - 交互入口（桌面 / CLI）在进入生成前调用：避免前端竞态窗口把已关闭或
///   已删除的会话 id 传入后收到"会话已关闭"错误（自动重建以继续对话）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 新建会话时绑定的人格（`None` 表示暂不绑定）。
/// - `session_id`: 调用方指定的会话（`None` = 无指定，直接新建）。
///
/// 返回:
/// - 可用的会话 id（原会话或新建会话）。
pub(crate) async fn resolve_send_session(
    engine: &Engine,
    persona_uid: Option<&str>,
    session_id: Option<Uuid>,
) -> RamariaResult<Uuid> {
    let Some(sid) = session_id else {
        let session = create(engine, persona_uid).await?;
        tracing::info!(session_id = %session.id, "发送目标未指定会话，已新建");
        return Ok(session.id);
    };

    match engine.storage_ref().get_session(sid).await {
        Ok(Some(session)) if session.ended_at.is_some() => {
            let new_session = create(engine, persona_uid).await?;
            tracing::info!(
                old_session_id = %sid,
                new_session_id = %new_session.id,
                "发送目标会话已关闭，自动创建新会话"
            );
            Ok(new_session.id)
        }
        Ok(Some(_)) => Ok(sid),
        Ok(None) => {
            let new_session = create(engine, persona_uid).await?;
            tracing::info!(
                old_session_id = %sid,
                new_session_id = %new_session.id,
                "发送目标会话不存在，自动创建新会话"
            );
            Ok(new_session.id)
        }
        Err(e) => {
            tracing::warn!(
                %sid,
                error = %e,
                "发送目标会话状态查询失败，保守沿用原会话（由生成路径判定）"
            );
            Ok(sid)
        }
    }
}

// =========================================================
// 会话创建与删除用例
// =========================================================

/// 创建会话用例。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 绑定的人格 uid（None 表示暂不绑定，发送消息时回写）。
///
/// 返回:
/// - 新会话核心记录（新 UUID、当前开始时间、本地通道、未关闭）；
///   宿主自行映射为各自的响应结构。
pub(crate) async fn create(engine: &Engine, persona_uid: Option<&str>) -> RamariaResult<Session> {
    engine.storage_ref().create_session(persona_uid).await
}

/// 删除会话用例（仅删除会话行本身）。
///
/// 说明:
/// - 关联数据按外键规则处理：有级联规则的子表（消息 / L1）随行清理，
///   存在无级联引用的子表（utt 块 / 对话示例）时删除可能失败；
///   需要保证全部关联数据整体移除时使用 [`delete_cascade`]。
/// - 会话不存在时幂等成功（与存储层删除语义一致，不报错）。
pub(crate) async fn delete(engine: &Engine, session_id: Uuid) -> RamariaResult<()> {
    engine.storage_ref().delete_session(session_id).await
}

/// 级联删除会话用例（事务内按依赖顺序清理全部关联数据）。
///
/// 流程:
/// 1. 删除会话的 utt 块 / 消息 / L1 摘要 / 对话示例 / 反馈审计残留；
/// 2. 删除会话行本身；整体成功或整体回滚。
///
/// 说明:
/// - 供一次性合成会话（如探针）用完即删的场景使用：不触发封存 / 学习管线；
/// - 宿主若持有生命周期容器（活跃指针与活跃时间缓存），删除后需自行清理
///   （见 `Lifecycle` 的指针与缓存接口），本用例只负责存储侧数据。
/// - 会话不存在时幂等成功（删除 0 行不报错）。
pub(crate) async fn delete_cascade(engine: &Engine, session_id: Uuid) -> RamariaResult<()> {
    engine
        .storage_ref()
        .delete_session_cascade(session_id)
        .await
}

/// 归一化人格 uid（空串视为未提供）。
fn normalize_persona(persona: Option<&str>) -> Option<String> {
    persona
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .map(str::to_string)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        engine_with_db, seed_closed_session_with_messages, seed_messages, seed_persona,
        seed_session_with_messages, seed_utt_block,
    };
    use crate::types::ChatRole;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::{MemoryL1, MessageRole};
    use ramaria_storage::SqliteStorage;
    use uuid::Uuid;

    /// 造会话 + N 条消息（created_at 自 base 起逐条 +1）。
    async fn seed_session(
        storage: &SqliteStorage,
        persona: &str,
        count: usize,
        base_ts: i64,
    ) -> Uuid {
        let session = storage
            .create_session(Some(persona))
            .await
            .expect("创建会话");
        seed_messages(storage, session.id, persona, count, base_ts).await;
        session.id
    }

    /// 按 session_id 分页：页内时间正序、total 正确、offset 越界为空。
    #[tokio::test]
    async fn history_pages_within_session() {
        let (engine, storage, dir) = engine_with_db("history").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session(&storage, "char-0001", 7, 1_000).await;

        // 第一页：最新 3 条（时间正序 = 1004..1006）
        let page1 = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: None,
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert_eq!(page1.session_id, Some(session_id));
        assert_eq!(page1.total, 7);
        assert_eq!(page1.messages.len(), 3);
        assert_eq!(page1.messages[0].content, "消息内容 4", "页内时间正序");
        assert_eq!(page1.messages[2].content, "消息内容 6");

        // 第二页：offset 3 → 1001..1003
        let page2 = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: Some(3),
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert_eq!(page2.messages[0].content, "消息内容 1");

        // 越界 → 空页但 total 保留
        let beyond = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: Some(3),
                offset: Some(99),
                persona: None,
            })
            .await
            .expect("历史读取成功");
        assert!(beyond.messages.is_empty());
        assert_eq!(beyond.total, 7);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 按 persona 定位：取该人格最近消息所属会话。
    #[tokio::test]
    async fn history_resolves_latest_session_by_persona() {
        let (engine, storage, dir) = engine_with_db("history-persona").await;
        seed_persona(&storage, "char-0001").await;
        let first = seed_session(&storage, "char-0001", 2, 1_000).await;
        let latest = seed_session(&storage, "char-0001", 2, 2_000).await;

        let result = engine
            .history(HistoryRequest {
                session_id: None,
                persona: Some("char-0001".to_string()),
                limit: None,
                offset: None,
            })
            .await
            .expect("历史读取成功");

        assert_eq!(result.session_id, Some(latest), "应定位最近会话");
        assert_ne!(result.session_id, Some(first));
        assert_eq!(result.total, 2);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 空请求 / 无数据人格 → 空结构（不报错）。
    #[tokio::test]
    async fn history_empty_inputs_return_empty() {
        let (engine, storage, dir) = engine_with_db("history-empty").await;
        seed_persona(&storage, "char-0001").await;

        let no_input = engine
            .history(HistoryRequest::default())
            .await
            .expect("历史读取成功");
        assert!(no_input.session_id.is_none());
        assert!(no_input.messages.is_empty());
        assert_eq!(no_input.total, 0);

        let no_messages = engine
            .history(HistoryRequest {
                session_id: None,
                persona: Some("char-0001".to_string()),
                limit: None,
                offset: None,
            })
            .await
            .expect("历史读取成功");
        assert!(no_messages.session_id.is_none());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 消息视图字段完整（角色 / 内容 / 时间 / 发言人）。
    #[tokio::test]
    async fn history_message_view_fields() {
        let (engine, storage, dir) = engine_with_db("history-fields").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session(&storage, "char-0001", 2, 5_000).await;

        let result = engine
            .history(HistoryRequest {
                session_id: Some(session_id),
                limit: None,
                offset: None,
                persona: None,
            })
            .await
            .expect("历史读取成功");

        assert_eq!(result.messages[0].role, MessageRole::User);
        assert_eq!(result.messages[0].persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(
            result.messages[0].time.timestamp_millis(),
            5_000,
            "时间应与存储一致"
        );
        // ChatRole 与内核角色的映射保持可用（供上层判断"最后一条用户消息"）
        assert_eq!(ChatRole::User.as_str(), "user");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话创建：绑定人格 / 不绑定两种口径，字段符合核心会话约定。
    #[tokio::test]
    async fn create_session_binds_persona_and_local_channel() {
        let (engine, _storage, dir) = engine_with_db("session-create").await;

        let bound = engine
            .create_session(Some("char-0001"))
            .await
            .expect("创建会话应成功");
        assert_eq!(bound.persona_uid.as_deref(), Some("char-0001"));
        assert_eq!(bound.channel, "local", "本地创建走 local 通道");
        assert!(bound.ended_at.is_none(), "新会话应处于进行中");
        assert!(bound.started_at > 0);

        let unbound = engine.create_session(None).await.expect("创建会话应成功");
        assert!(unbound.persona_uid.is_none());
        assert_ne!(bound.id, unbound.id, "两次创建应得到不同会话");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 会话删除：删除后不可查；不存在的会话幂等成功（与存储层语义一致）。
    #[tokio::test]
    async fn delete_session_removes_row_idempotently() {
        let (engine, storage, dir) = engine_with_db("session-delete").await;
        let session = engine.create_session(None).await.expect("创建会话应成功");

        engine.delete_session(session.id).await.expect("删除应成功");
        assert!(
            storage
                .get_session(session.id)
                .await
                .expect("查询应成功")
                .is_none(),
            "删除后会话不应可见"
        );

        // 不存在（含刚删除的）会话：幂等成功
        engine
            .delete_session(session.id)
            .await
            .expect("重复删除应幂等成功");
        engine
            .delete_session(Uuid::nil())
            .await
            .expect("不存在会话应幂等成功");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 级联删除：会话与消息 / utt 块 / L1 摘要整体移除；不存在的会话幂等成功。
    #[tokio::test]
    async fn delete_session_cascade_removes_related_rows() {
        let (engine, storage, dir) = engine_with_db("session-cascade").await;
        seed_persona(&storage, "char-0001").await;
        let session_id = seed_session(&storage, "char-0001", 3, 1_000).await;
        seed_utt_block(&storage, session_id, "char-0001", "原文块").await;
        let mut l1 = MemoryL1::new(session_id, "摘要".to_string(), None);
        l1.persona_uid = Some("char-0001".to_string());
        storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");

        engine
            .delete_session_cascade(session_id)
            .await
            .expect("级联删除应成功");

        assert!(
            storage
                .get_session(session_id)
                .await
                .expect("查询应成功")
                .is_none()
        );
        assert!(
            storage
                .list_messages(session_id)
                .await
                .expect("查询消息应成功")
                .is_empty(),
            "级联删除后不应残留消息"
        );
        assert!(
            storage
                .list_memory_l1(session_id)
                .await
                .expect("查询 L1 应成功")
                .is_empty(),
            "级联删除后不应残留 L1"
        );
        assert!(
            storage
                .list_utt_blocks_by_persona("char-0001")
                .await
                .expect("查询块应成功")
                .is_empty(),
            "级联删除后不应残留 utt 块"
        );

        // 不存在会话：幂等成功
        engine
            .delete_session_cascade(Uuid::nil())
            .await
            .expect("不存在会话应幂等成功");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 发送目标解析：活跃会话原样返回；已关闭 / 不存在 / 未指定均新建并绑定人格。
    #[tokio::test]
    async fn resolve_send_session_keeps_active_and_rebuilds_closed_or_missing() {
        let (engine, storage, dir) = engine_with_db("session-resolve-send").await;
        seed_persona(&storage, "char-0001").await;

        // 活跃会话：原样返回（不新建）
        let active = seed_session_with_messages(&storage, "char-0001", 1, 1_000).await;
        let resolved = engine
            .resolve_send_session(Some("char-0001"), Some(active))
            .await
            .expect("解析应成功");
        assert_eq!(resolved, active, "活跃会话应原样返回");

        // 已关闭会话：新建（绑定人格、未关闭）
        let closed = seed_closed_session_with_messages(&storage, "char-0001", 1, 2_000).await;
        let rebuilt = engine
            .resolve_send_session(Some("char-0001"), Some(closed))
            .await
            .expect("解析应成功");
        assert_ne!(rebuilt, closed, "已关闭会话应触发新建");
        let row = storage
            .get_session(rebuilt)
            .await
            .expect("查询应成功")
            .expect("新会话应存在");
        assert_eq!(row.persona_uid.as_deref(), Some("char-0001"));
        assert!(row.ended_at.is_none(), "新会话应处于进行中");
        let original = storage
            .get_session(closed)
            .await
            .expect("查询应成功")
            .expect("原会话应存在");
        assert!(original.ended_at.is_some(), "原会话状态不应被改写");

        // 不存在的会话：新建（未提供人格时不绑定）
        let missing = Uuid::new_v4();
        let rebuilt2 = engine
            .resolve_send_session(None, Some(missing))
            .await
            .expect("解析应成功");
        assert_ne!(rebuilt2, missing);
        let row2 = storage
            .get_session(rebuilt2)
            .await
            .expect("查询应成功")
            .expect("新会话应存在");
        assert!(row2.persona_uid.is_none(), "未提供人格时新会话暂不绑定");

        // 未指定会话：新建（绑定人格）
        let created = engine
            .resolve_send_session(Some("char-0001"), None)
            .await
            .expect("解析应成功");
        let row3 = storage
            .get_session(created)
            .await
            .expect("查询应成功")
            .expect("新会话应存在");
        assert_eq!(row3.persona_uid.as_deref(), Some("char-0001"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
