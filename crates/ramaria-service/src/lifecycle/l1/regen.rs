//! crates/ramaria-service/src/lifecycle/l1/regen.rs - Ramaria L1 手动重生成
//!
//! 设计特点:
//! - 三种口径：单段 / 幂等无级联 / 渐进式感知，供封存失败补救与批量导入场景
//! - 单段口径从后端配置传播 token 预算并以下限钳制，经任务管理器包裹执行
//! - 每段生成后增量镜像检索器与关键词镜像，无需等待整库重建
//! - 单段与渐进式末尾触发 L2 检查；无级联口径由调用方统一触发

use tracing::{error, info, warn};
use uuid::Uuid;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::MemoryL1;

use crate::engine::Engine;

use super::generate::generate_l1_summary;

// =========================================================
// L1 摘要手动重生成
// =========================================================

/// 为指定会话重新生成单段 L1 摘要（手动重试，末尾触发 L2 检查）。
///
/// 职责:
/// - 供封存中 L1 生成失败后的手动补救；会话可已关闭，也可仍在活跃中。
/// - 与封存单段口径共用底层编排（`backend_config` 预算传播 + `JobManager` 重试 + 读回）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `session_id`: 目标会话。
/// - `persona_uid`: L1 归属人格（`None` = 不绑定）。
/// - `user_prefix`: 覆盖默认"用户："前缀；`None` 用默认。
/// - `assistant_prefix`: 覆盖默认"助手："前缀；`None` 用默认。
///
/// 返回:
/// - `Ok(Some(l1))`: 生成成功（已写库并增量镜像）；
/// - `Ok(None)`: 会话无消息（跳过）。
pub(crate) async fn regenerate_l1(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Option<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1: 会话无消息，跳过");
        return Ok(None);
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "手动重试 L1 摘要"
    );

    match generate_l1_summary(
        engine,
        session_id,
        persona_uid,
        user_prefix,
        assistant_prefix,
        false,
        false,
    )
    .await
    {
        Ok(l1) => {
            info!(%session_id, l1_id = %l1.id, "L1 重试成功");
            crate::index::index_l1_into_mirrors(engine, &l1).await;
            crate::lifecycle::l2_l3::check_l2_trigger(engine, None).await;
            Ok(Some(l1))
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 重试失败");
            Err(e)
        }
    }
}

/// 生成单段 L1 摘要但不触发 L2 级联（幂等；供批量导入场景使用）。
///
/// 幂等性:
/// - 目标 persona 已有 L1 → 跳过生成（避免重复 LLM 调用），仅补索引镜像并返回 `Ok(None)`；
/// - 仅有旧的无归属摘要（`persona_uid` 为空）→ 先清理再生成；
/// - 无任何摘要 → 直接生成。
///
/// 说明:
/// - 与 [`regenerate_l1`] 的差异是跳过末尾 L2 检查；
///   调用方应在全部 L1 生成完成后自行触发级联。
pub(crate) async fn regenerate_l1_no_cascade(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Option<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1_no_cascade: 会话无消息，跳过");
        return Ok(None);
    }

    // 已有目标 persona 的 L1 → 幂等跳过（避免重复 LLM 调用）
    if let Some(target_uid) = persona_uid {
        let existing = storage.list_memory_l1(session_id).await?;
        if let Some(existing_l1) = existing
            .iter()
            .find(|l1| l1.persona_uid.as_deref() == Some(target_uid))
        {
            info!(
                %session_id,
                persona_uid = %target_uid,
                "该会话已有目标 persona 的 L1 摘要，跳过重新生成"
            );
            // 索引镜像仍要确保（此前重建可能未覆盖该条）
            crate::index::index_l1_into_mirrors(engine, existing_l1).await;
            return Ok(None);
        }
    }

    // 清理旧的无归属摘要（避免同一会话新旧两份并存），再做生成
    let deleted = storage.delete_memory_l1_by_session(session_id).await?;
    if deleted > 0 {
        info!(%session_id, deleted, "已清理旧的无归属 L1 摘要");
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "批量 L1 摘要（无级联）"
    );

    match generate_l1_summary(
        engine,
        session_id,
        persona_uid,
        user_prefix,
        assistant_prefix,
        false,
        false,
    )
    .await
    {
        Ok(l1) => {
            info!(%session_id, l1_id = %l1.id, "L1 生成成功（无级联）");
            crate::index::index_l1_into_mirrors(engine, &l1).await;
            Ok(Some(l1))
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 生成失败");
            Err(e)
        }
    }
}

/// 为指定会话重新生成 L1 摘要（渐进式感知口径）。
///
/// 职责:
/// - 与封存路径口径一致：`[l1.progressive]` 开启且会话触发阈值（消息数 / 时间跨度）时
///   按段生成多条 L1；未触发时回退单段摘要。
/// - 供长会话的手动重摘要使用；末尾触发 L2 检查（与 [`regenerate_l1`] 一致的级联语义）。
///
/// 参数:
/// - 与 [`regenerate_l1`] 一致（`persona_uid` / 前缀覆盖）。
///
/// 返回:
/// - `Ok(l1_list)`: 本次生成的全部段 L1（未触发渐进时为 1 条）；
/// - `Ok(vec![])`: 会话无消息。
pub(crate) async fn regenerate_l1_progressive(
    engine: &Engine,
    session_id: Uuid,
    persona_uid: Option<&str>,
    user_prefix: Option<&str>,
    assistant_prefix: Option<&str>,
) -> RamariaResult<Vec<MemoryL1>> {
    let storage = engine.storage_ref().as_ref();
    let messages = storage.list_messages(session_id).await?;
    if messages.is_empty() {
        warn!(%session_id, "regenerate_l1_progressive: 会话无消息，跳过");
        return Ok(Vec::new());
    }

    info!(
        %session_id,
        ?persona_uid,
        msg_count = messages.len(),
        "手动重试 L1 摘要（渐进式感知）"
    );

    let progressive = engine.config().l1.progressive.clone();
    let llm = engine.llm_ref();
    match ramaria_memory::l1::generate_l1_summaries(
        storage,
        llm.as_ref(),
        &progressive,
        ramaria_memory::l1::L1GenerateRequest {
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
            fanout_others: false,
            fanout_include_self: false,
            target_projection: false,
            job_manager_config: Some(engine.job_manager_config()),
        },
    )
    .await
    {
        Ok(l1_list) => {
            info!(
                %session_id,
                l1_count = l1_list.len(),
                "L1 重试成功（渐进式感知）"
            );
            // 每段 L1 都做增量镜像（与既有重试路径一致）
            for l1 in &l1_list {
                crate::index::index_l1_into_mirrors(engine, l1).await;
            }
            crate::lifecycle::l2_l3::check_l2_trigger(engine, None).await;
            Ok(l1_list)
        }
        Err(e) => {
            error!(%session_id, error = %e, "L1 重试失败（渐进式感知）");
            Err(e)
        }
    }
}
