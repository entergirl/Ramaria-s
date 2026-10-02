//! crates/ramaria-service/src/persona/regenerate.rs - Ramaria 人格 L1 重生成模块
//!
//! 设计特点:
//! - 离线重建路径：全量枚举目标人格消息 → 按枚举顺序去重会话 → 逐会话重生成 L1
//! - 单 session 内部已有重试与退避；外层连续失败达阈值判定 LLM 不可用并提前终止
//! - 幂等：会话已有目标人格的 L1 时按跳过处理（不计成功 / 失败，也不影响连续失败计数）
//! - 严格按 persona_uid 隔离：只处理目标人格的记录，不跨人格聚合
//! - L2/L3 级联不在本模块触发，由宿主拿到结果后自行触发（避免阻塞当前调用）
//! - 隐私：日志中的个人标识经 `mask_id` 脱敏；日志只记录结构化计数，不含消息正文

use std::collections::HashSet;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine::Engine;

// =========================================================
// regenerate_import_l1（人格 L1 重生成）
// =========================================================

/// 外层连续失败阈值：单 session 内部已有重试与退避，外层连续 3 次失败即判定 LLM 不可用。
const MAX_CONSECUTIVE_L1_FAILURES: u32 = 3;

/// 人格 L1 重生成结果（供宿主构造用户提示与统计展示）。
///
/// 字段约定:
/// - `l1_regenerated` / `l1_failed`: 生成成功 / 失败的会话数（单会话内部重试耗尽后才计入失败）。
/// - `total_sessions`: 参与重生成的会话总数（含跳过与未处理会话）。
/// - `early_terminated`: 是否因连续失败提前终止。
/// - `remaining_skipped`: 提前终止时未处理的会话数（未提前终止为 0）。
/// - `message`: 面向用户的提示文案。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaRegenerateOutcome {
    pub l1_regenerated: usize,
    pub l1_failed: usize,
    pub total_sessions: usize,
    pub early_terminated: bool,
    pub remaining_skipped: usize,
    pub message: String,
}

/// 为某人格的导入会话重新生成 L1 摘要（导入失败后的离线重建路径）。
///
/// 流程:
/// 1. 校验 UID 非空并确认人格存在（否则返回业务校验错误）；
/// 2. 全量枚举该人格消息并推导会话列表（去重按消息枚举顺序保留首次出现，顺序确定）；
/// 3. 逐会话按单段口径重生成 L1，连续失败达 [`MAX_CONSECUTIVE_L1_FAILURES`] 次判定 LLM 不可用并提前终止；
/// 4. 按成功 / 部分失败 / 提前终止三个分支构造提示文案。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 计数与提示文案；空 UID 与人格不存在返回 `Validation` 错误。
///
/// 说明:
/// - L2/L3 级联不在本用例内触发：宿主拿到结果后自行触发 [`Engine::trigger_l2_check`]
///   （提示文案中的"L2/L3 正在后台处理中"即指该宿主行为，避免阻塞当前调用）。
/// - 幂等：会话已有目标人格的 L1 时按跳过处理（不计成功 / 失败，也不影响连续失败计数）。
pub(crate) async fn regenerate_import_l1(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<PersonaRegenerateOutcome> {
    if persona_uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(persona_uid).await?.is_none() {
        return Err(RamariaError::validation(format!(
            "人格不存在: uid={persona_uid}"
        )));
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        "重新生成导入会话的 L1 摘要"
    );

    // 离线重建路径：必须覆盖该人格的全部会话，故全量枚举其消息（不做截断）；
    // 若将来出现 persona 消息的浏览 / 展示需求，须另走分页查询。
    let messages = storage.list_messages_by_persona(persona_uid).await?;

    // 会话去重：按消息枚举顺序保留首次出现（确定性顺序便于复现），
    // 不使用 HashSet 的迭代顺序，避免会话处理顺序随哈希随机化漂移。
    let mut seen_sessions = HashSet::new();
    let mut session_ids: Vec<Uuid> = Vec::new();
    for message in &messages {
        if seen_sessions.insert(message.session_id) {
            session_ids.push(message.session_id);
        }
    }

    if session_ids.is_empty() {
        return Ok(PersonaRegenerateOutcome {
            l1_regenerated: 0,
            l1_failed: 0,
            total_sessions: 0,
            early_terminated: false,
            remaining_skipped: 0,
            message: "该人格没有关联的导入消息，无需处理。".to_string(),
        });
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        session_count = session_ids.len(),
        message_count = messages.len(),
        "找到关联的导入 session，开始重新生成 L1"
    );

    let total = session_ids.len();
    let mut l1_regenerated = 0usize;
    let mut l1_failed = 0usize;
    let mut consecutive_failures: u32 = 0;
    let mut early_terminated = false;
    let mut remaining_skipped = 0usize;

    for (idx, sid) in session_ids.iter().enumerate() {
        match engine
            .regenerate_l1_no_cascade(*sid, Some(persona_uid), None, None)
            .await
        {
            Ok(Some(_)) => {
                l1_regenerated += 1;
                consecutive_failures = 0;
                tracing::debug!(session_id = %sid, "L1 重新生成成功");
            }
            Ok(None) => {
                // 会话已有目标人格的 L1：幂等跳过，不计成功 / 失败，也不影响连续失败计数
                tracing::debug!(session_id = %sid, "L1 无需生成，跳过");
            }
            Err(e) => {
                l1_failed += 1;
                consecutive_failures += 1;
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    consecutive_failures,
                    "L1 重新生成失败"
                );

                if consecutive_failures >= MAX_CONSECUTIVE_L1_FAILURES {
                    remaining_skipped = total.saturating_sub(idx + 1);
                    tracing::warn!(
                        persona_uid = %mask_id(persona_uid),
                        consecutive_failures,
                        l1_regenerated,
                        l1_failed,
                        remaining_skipped,
                        "L1 连续失败达到上限，判定 LLM 不可用，跳过剩余会话"
                    );
                    early_terminated = true;
                    break;
                }
            }
        }
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        l1_regenerated,
        l1_failed,
        total,
        early_terminated,
        remaining_skipped,
        "L1 重新生成完成"
    );

    let message = if early_terminated {
        format!(
            "L1 连续失败 {MAX_CONSECUTIVE_L1_FAILURES} 次，已提前终止。成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接后重试。剩余 {remaining_skipped} 个 session 未处理。"
        )
    } else if l1_failed > 0 {
        format!(
            "L1 重新生成完成: 成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
        )
    } else {
        format!("L1 全部重新生成成功 ({l1_regenerated}/{total})。L2/L3 正在后台处理中...")
    };

    Ok(PersonaRegenerateOutcome {
        l1_regenerated,
        l1_failed,
        total_sessions: total,
        early_terminated,
        remaining_skipped,
        message,
    })
}
