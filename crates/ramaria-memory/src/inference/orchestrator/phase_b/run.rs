//! crates/ramaria-memory/src/inference/orchestrator/phase_b/run.rs - Phase B 推断编排主流程
//!
//! 设计特点:
//! - 加载旧 traits → 因果链/动机文本注入 → 三步推断 → 后处理 diff → 持久化。
//! - 首轮推断（无旧 traits）跳过 post_process diff 计算。
//! - LLM 推断失败时降级至 mock_infer，不阻塞主流程。
//! - 单条 trait 落库失败仅告警跳过，不影响其余 trait。

use ramaria_core::{
    RamariaResult,
    traits::{LlmProvider, StorageBackend},
    types::{TraitSource, TraitStatus},
};
use tracing::{debug, error, info, warn};

use crate::inference::{
    causal::{
        extract_causal_features, extract_causal_features_extended, format_causal_features_text,
    },
    inferrer::{
        InferrerConfig, PostProcessResult, format_motive_stats, mock_infer, post_process_inference,
    },
    stats::StatsSummary,
};

use super::super::types::{PhaseBResult, PhaseBSource};
use super::three_step::run_three_step_inference;

// =========================================================
// Phase B: LLM 三步结构化推断编排
// =========================================================

/// 执行 Phase B 推断：三步 prompt → LLM → JSON 解析 → post_process → 写入 DB。
///
/// 流程:
/// 1. 从 DB 加载已有 trait 列表（用于后处理对比）。
/// 2. 构建 Step 1 prompt，调用 LLM 获取逐分类性格信号。
/// 3. 构建 Step 2 prompt，调用 LLM 进行跨分类一致性分析。
/// 4. 构建 Step 3 prompt，调用 LLM 合成结构化性格画像。
/// 5. 任一 LLM 步骤失败 → 降级至 mock_infer（基于统计规则推断）。
/// 6. 首轮推断（无旧 traits）跳过 post_process diff 计算。
/// 7. 将推断结果持久化到 personality_traits 表。
///
/// 参数:
/// - `llm`: LLM provider，用于三步推断。
/// - `storage`: 存储后端，用于读写 personality_traits。
/// - `stats`: Phase A 统计摘要。
/// - `persona_uid`: 目标人格标识。
/// - `config`: 推断器配置。
/// - `causal_extended_enabled`: 是否注入 A8 扩展特征（时延分布 + 情绪沿链走势）。
///   `false` → 回退 v1.7 仅注入链长/循环模式（文本逐字节等价）。
///
/// 返回:
/// - PhaseBResult：包含保存/更新/废弃的 trait 数量及推断来源。
pub async fn run_phase_b_inference(
    llm: &dyn LlmProvider,
    storage: &dyn StorageBackend,
    stats: &StatsSummary,
    persona_uid: &str,
    config: &InferrerConfig,
    causal_extended_enabled: bool,
) -> RamariaResult<PhaseBResult> {
    let persona_owned = persona_uid.to_string();

    // ---- 1. 加载已有 traits ----
    let old_traits = storage
        .list_traits_by_persona(&persona_owned)
        .await
        .map_err(|e| {
            error!(persona_uid = %persona_owned, error = %e, "Phase B: 加载已有 traits 失败");
            e
        })?;

    let is_first_round = old_traits.is_empty();
    if is_first_round {
        info!(persona_uid = %persona_owned, "Phase B: 首轮推断，跳过 post_process diff 和 drift_detection");
    } else {
        info!(
            persona_uid = %persona_owned,
            old_trait_count = old_traits.len(),
            "Phase B: 加载已有 traits，执行增量推断"
        );
    }

    // ---- 1.5. 因果链特征提取（A8） ----
    let causal_text = match storage
        .list_event_relations_by_persona(&persona_owned)
        .await
    {
        Ok(relations) if !relations.is_empty() => {
            // 查询该 persona 的所有事件用于类别映射
            let events = storage
                .list_events_by_persona(&persona_owned, 0, 10000)
                .await
                .unwrap_or_default();
            // 独立开关：开启时补齐时延分布 + 情绪沿链走势扩展段；
            // 关闭时回退 v1.7 路径（仅链长/循环模式），文本逐字节等价。
            let features = if causal_extended_enabled {
                extract_causal_features_extended(&events, &relations)
            } else {
                extract_causal_features(&events, &relations)
            };
            let text = format_causal_features_text(&features);
            if !text.is_empty() {
                debug!(
                    persona_uid = %persona_owned,
                    chain_length = features.chain_length,
                    cycle_count = features.cyclic_patterns.len(),
                    latency_sampled = features.latency_stats.sampled_edge_count,
                    emotion_sampled = features.emotion_trend.sampled_node_count,
                    "Phase B: 因果链特征提取完成"
                );
            }
            text
        }
        Ok(_) => {
            debug!(persona_uid = %persona_owned, "Phase B: 无事件关系数据，跳过因果链分析");
            String::new()
        }
        Err(e) => {
            warn!(persona_uid = %persona_owned, error = %e, "Phase B: 查询事件关系失败，跳过因果链分析");
            String::new()
        }
    };

    // ---- 1.6. 动机维度统计文本（E 模块） ----
    let motive_stats_text = if !stats.motive_stats.is_empty() {
        let text = format_motive_stats(&stats.motive_stats, 5);
        if !text.is_empty() {
            debug!(
                persona_uid = %persona_owned,
                motive_count = stats.motive_stats.len(),
                "Phase B: 动机维度统计已格式化"
            );
        }
        text
    } else {
        debug!(persona_uid = %persona_owned, "Phase B: 无动机数据，跳过动机维度统计");
        String::new()
    };

    // ---- 2. 三步 LLM 推断（含降级） ----
    let causal_text_ref: Option<&str> = if causal_text.is_empty() {
        None
    } else {
        Some(&causal_text)
    };
    let motive_stats_ref: Option<&str> = if motive_stats_text.is_empty() {
        None
    } else {
        Some(&motive_stats_text)
    };
    let inference_result = run_three_step_inference(
        llm,
        stats,
        persona_uid,
        config,
        causal_text_ref,
        motive_stats_ref,
    )
    .await;

    let (result, source) = match inference_result {
        Ok(r) => {
            info!(persona_uid = %persona_owned, trait_count = r.traits.len(), "Phase B: LLM 三步推断完成");
            (r, PhaseBSource::LlmInference)
        }
        Err(e) => {
            warn!(persona_uid = %persona_owned, error = %e, "Phase B: LLM 推断失败，降级至 mock_infer");
            (
                mock_infer(stats, &persona_owned),
                PhaseBSource::MockFallback,
            )
        }
    };

    // ---- 3. 后处理：与旧 traits 对比 ----
    let post_result = if is_first_round {
        // 首轮推断：所有 trait 直接新增，不做 diff
        info!(persona_uid = %persona_owned, "Phase B: 首轮推断，所有 trait 直接新增");
        PostProcessResult {
            to_add: result.traits.clone(),
            to_update: vec![],
            to_deprecate: vec![],
            diffs: vec![],
        }
    } else {
        post_process_inference(&result, &old_traits, &persona_owned)
    };

    // ---- 4. 持久化 ----
    let mut traits_saved = 0usize;
    let mut traits_updated = 0usize;
    let mut traits_deprecated = 0usize;
    let mut active_trait_ids: Vec<i64> = Vec::new();

    // 已有 trait 的 ID 列表（用于 Phase C）
    // 先收集未废弃的旧 trait ID
    for t in &old_traits {
        if t.status == TraitStatus::Active {
            active_trait_ids.push(t.id);
        }
    }

    // 4a. 新增 trait
    for mut t in post_result.to_add {
        t.persona_uid = persona_owned.clone();
        t.source = TraitSource::Inferred;
        t.status = TraitStatus::Active;
        // 首轮推断置信度初始值
        if t.confidence == 0.0 {
            t.confidence = 0.5;
        }
        if t.evidence == 0.0 {
            t.evidence = 1.0;
        }
        if t.consistency == 0.0 {
            t.consistency = 0.5;
        }

        match storage.save_trait(&t).await {
            Ok(id) => {
                traits_saved += 1;
                active_trait_ids.push(id);
                debug!(
                    persona_uid = %persona_owned,
                    trait_label = %t.trait_label,
                    trait_id = id,
                    "Phase B: 新增 trait 已保存"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    trait_label = %t.trait_label,
                    error = %e,
                    "Phase B: 新增 trait 保存失败（跳过，不影响其他 trait）"
                );
            }
        }
    }

    // 4b. 更新已有 trait
    // `to_update` 元素为 (old_id: i64, updated_trait: PersonalityTrait)
    for (old_id, mut updated_trait) in post_result.to_update {
        updated_trait.id = old_id;
        updated_trait.persona_uid = persona_owned.clone();
        updated_trait.source = TraitSource::Inferred;
        updated_trait.status = TraitStatus::Active;

        match storage.save_trait(&updated_trait).await {
            Ok(_) => {
                traits_updated += 1;
                if !active_trait_ids.contains(&old_id) {
                    active_trait_ids.push(old_id);
                }
                debug!(
                    persona_uid = %persona_owned,
                    trait_label = %updated_trait.trait_label,
                    old_id,
                    "Phase B: 更新 trait 已保存"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    trait_label = %updated_trait.trait_label,
                    error = %e,
                    "Phase B: 更新 trait 保存失败（跳过）"
                );
            }
        }
    }

    // 4c. 废弃旧 trait（`to_deprecate` 为旧 trait ID 列表）
    for old_id in post_result.to_deprecate {
        match storage
            .update_trait_status(old_id, TraitStatus::Deprecated)
            .await
        {
            Ok(_) => {
                traits_deprecated += 1;
                active_trait_ids.retain(|&id| id != old_id);
                debug!(
                    persona_uid = %persona_owned,
                    old_id,
                    "Phase B: trait 已标记废弃"
                );
            }
            Err(e) => {
                warn!(
                    persona_uid = %persona_owned,
                    old_id,
                    error = %e,
                    "Phase B: 废弃 trait 状态更新失败（跳过）"
                );
            }
        }
    }

    info!(
        persona_uid = %persona_owned,
        saved = traits_saved,
        updated = traits_updated,
        deprecated = traits_deprecated,
        source = ?source,
        "Phase B: 推断完成并持久化"
    );

    Ok(PhaseBResult {
        traits_saved,
        traits_updated,
        traits_deprecated,
        source,
        trait_ids: active_trait_ids,
        traits: result.traits,
    })
}
