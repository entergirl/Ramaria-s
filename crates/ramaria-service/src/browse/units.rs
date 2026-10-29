//! crates/ramaria-service/src/browse/units.rs - Ramaria 记忆单元证据导出
//!
//! 设计特点:
//! - 四层单一入口：L1 直读摘要区间、L2 经事件溯源、L3 经引用与证据、utt 直读块区间
//! - 软引用容错：单元不存在、区间未回填或端点不在会话内时贡献空集合，只记调试日志
//! - 去重口径：同一 L1 只展开一次，同一消息只保留首次出现；区间内保持消息时间序
//! - 会话消息按 `(created_at, id)` 排序后定位区间，同一会话只全量读取一次
//! - 隐私：日志只记层名与计数，不记消息内容

use std::collections::{HashMap, HashSet};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::Message;
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// 记忆单元 → 消息 id 集合
// =========================================================

/// 记忆单元覆盖的消息 id 集合（UUID 字符串）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `layer`: 单元层级，取值 `l1` / `l2` / `l3` / `utt`。
/// - `id`: 单元主键；`l1` 为 UUID 字符串，`l2` / `l3` / `utt` 为十进制整数。
///
/// 返回:
/// - 按来源顺序展开、按首次出现去重后的消息 id 列表；
/// - 单元不存在或无可用区间时返回空列表（非错误）。
///
/// 说明:
/// - L1 读 `start_msg_id` / `end_msg_id` 区间；
/// - L2 取 `event_sources` 的源 L1 逐个展开；
/// - L3 依次取 `ref_l1_id`、`ref_event_id` 与 `trait_evidence` 事件（同一 L1 只展开一次）；
/// - utt 读块自身的消息区间。
pub(crate) async fn memory_unit_sources(
    engine: &Engine,
    layer: &str,
    id: &str,
) -> RamariaResult<Vec<String>> {
    let storage = engine.storage_ref().as_ref();
    let mut cache = MessageCache::default();
    let mut seen_l1: HashSet<Uuid> = HashSet::new();
    let mut seen_msg: HashSet<Uuid> = HashSet::new();
    let mut collected: Vec<Uuid> = Vec::new();

    match layer {
        "l1" => {
            let l1_id = parse_uuid_id(layer, id)?;
            expand_l1(
                storage,
                l1_id,
                &mut cache,
                &mut seen_l1,
                &mut seen_msg,
                &mut collected,
            )
            .await?;
        }
        "l2" => {
            let event_id = parse_int_id(layer, id)?;
            for source in storage.list_event_sources_by_event(event_id).await? {
                expand_l1(
                    storage,
                    source.l1_id,
                    &mut cache,
                    &mut seen_l1,
                    &mut seen_msg,
                    &mut collected,
                )
                .await?;
            }
        }
        "l3" => {
            let trait_id = parse_int_id(layer, id)?;
            let Some(trait_row) = storage.get_trait(trait_id).await? else {
                tracing::debug!(layer, "记忆单元不存在，证据集合为空");
                return Ok(Vec::new());
            };
            if let Some(l1_id) = trait_row.ref_l1_id {
                expand_l1(
                    storage,
                    l1_id,
                    &mut cache,
                    &mut seen_l1,
                    &mut seen_msg,
                    &mut collected,
                )
                .await?;
            }
            // 直接引用事件在前，证据记录补充在后；同一事件只展开一次
            let mut event_ids: Vec<i64> = trait_row.ref_event_id.into_iter().collect();
            for evidence in storage.list_evidence_by_trait(trait_id).await? {
                if !event_ids.contains(&evidence.event_id) {
                    event_ids.push(evidence.event_id);
                }
            }
            for event_id in event_ids {
                for source in storage.list_event_sources_by_event(event_id).await? {
                    expand_l1(
                        storage,
                        source.l1_id,
                        &mut cache,
                        &mut seen_l1,
                        &mut seen_msg,
                        &mut collected,
                    )
                    .await?;
                }
            }
        }
        "utt" => {
            let block_id = parse_int_id(layer, id)?;
            let Some(block) = storage.get_utt_block(block_id).await? else {
                tracing::debug!(layer, "记忆单元不存在，证据集合为空");
                return Ok(Vec::new());
            };
            cache
                .append_range(
                    storage,
                    block.session_id,
                    block.start_msg_id,
                    block.end_msg_id,
                    &mut seen_msg,
                    &mut collected,
                )
                .await?;
        }
        other => {
            return Err(RamariaError::validation(format!(
                "未知记忆层级: {other}（可选 l1 / l2 / l3 / utt）"
            )));
        }
    }

    tracing::debug!(layer, count = collected.len(), "记忆单元证据导出完成");
    Ok(collected.iter().map(Uuid::to_string).collect())
}

// =========================================================
// 展开与去重
// =========================================================

/// 展开单条 L1 的消息区间（同一 L1 只展开一次；不存在或未回填区间时跳过）。
async fn expand_l1(
    storage: &dyn StorageBackend,
    l1_id: Uuid,
    cache: &mut MessageCache,
    seen_l1: &mut HashSet<Uuid>,
    seen_msg: &mut HashSet<Uuid>,
    collected: &mut Vec<Uuid>,
) -> RamariaResult<()> {
    if !seen_l1.insert(l1_id) {
        return Ok(());
    }
    let Some(l1) = storage.get_memory_l1(l1_id).await? else {
        tracing::debug!("L1 摘要不存在，跳过该来源");
        return Ok(());
    };
    let (Some(start), Some(end)) = (l1.start_msg_id, l1.end_msg_id) else {
        tracing::debug!("L1 摘要未回填消息区间，贡献为空");
        return Ok(());
    };
    cache
        .append_range(storage, l1.session_id, start, end, seen_msg, collected)
        .await
}

/// 会话消息缓存：同一用例内同一会话只全量读取一次。
#[derive(Default)]
struct MessageCache {
    sessions: HashMap<Uuid, Vec<Message>>,
}

impl MessageCache {
    /// 从缓存或存储取会话消息后调用 [`append_slice`]（同一会话只全量读取一次）。
    async fn append_range(
        &mut self,
        storage: &dyn StorageBackend,
        session_id: Uuid,
        start: Uuid,
        end: Uuid,
        seen_msg: &mut HashSet<Uuid>,
        collected: &mut Vec<Uuid>,
    ) -> RamariaResult<()> {
        if let Some(messages) = self.sessions.get(&session_id) {
            append_slice(messages, start, end, seen_msg, collected);
            return Ok(());
        }
        let mut messages = storage.list_messages(session_id).await?;
        messages.sort_by(|a, b| {
            a.created_at
                .cmp(&b.created_at)
                .then_with(|| a.id.cmp(&b.id))
        });
        append_slice(&messages, start, end, seen_msg, collected);
        self.sessions.insert(session_id, messages);
        Ok(())
    }
}

/// 追加消息区间内的消息（按首次出现去重）。
///
/// 说明:
/// - 区间端点任一不在 `messages` 内（软引用失效）时不追加，不报错；
/// - 端点顺序颠倒时按两者位置之间的闭区间取值，避免异常数据丢失覆盖范围。
fn append_slice(
    messages: &[Message],
    start: Uuid,
    end: Uuid,
    seen_msg: &mut HashSet<Uuid>,
    collected: &mut Vec<Uuid>,
) {
    let (Some(start_idx), Some(end_idx)) = (
        messages.iter().position(|m| m.id == start),
        messages.iter().position(|m| m.id == end),
    ) else {
        tracing::debug!("消息区间端点不在会话内，贡献为空");
        return;
    };
    let (lo, hi) = if start_idx <= end_idx {
        (start_idx, end_idx)
    } else {
        (end_idx, start_idx)
    };
    for message in &messages[lo..=hi] {
        if seen_msg.insert(message.id) {
            collected.push(message.id);
        }
    }
}

// =========================================================
// 输入解析
// =========================================================

/// 解析 UUID 形态的单元 id（`l1` 层）。
fn parse_uuid_id(layer: &str, id: &str) -> RamariaResult<Uuid> {
    Uuid::parse_str(id).map_err(|e| {
        RamariaError::validation(format!("记忆层级 {layer} 的 id 必须是 UUID: {id}（{e}）"))
    })
}

/// 解析正整数形态的单元 id（`l2` / `l3` / `utt` 层）。
fn parse_int_id(layer: &str, id: &str) -> RamariaResult<i64> {
    let parsed = id.parse::<i64>().map_err(|e| {
        RamariaError::validation(format!(
            "记忆层级 {layer} 的 id 必须是十进制整数: {id}（{e}）"
        ))
    })?;
    if parsed <= 0 {
        return Err(RamariaError::validation(format!(
            "记忆层级 {layer} 的 id 必须为正整数: {id}"
        )));
    }
    Ok(parsed)
}
