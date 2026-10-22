//! crates/ramaria-memory/src/recall/assemble.rs - 召回装配实现（在线管线与服务层同源）
//!
//! 设计特点:
//! - 单份实现：检索 → 子层过滤 → 衰减重排 → Persona-Aware 过滤 → 段落与 utt 渲染
//! - 锁纪律：std 读锁只覆盖同步内存检索；embedding 生成与 touch_l1 在锁外 await
//! - 静默降级：嵌入 / 镜像 / 索引任一缺席都返回结构完整的空输出

use std::sync::Arc;

use ramaria_core::types::{PersonaKind, now_ms};
use uuid::Uuid;

use crate::bm25::DocId;
use crate::decay::{DecayConfig, calc_retention};
use crate::keyword::CompositeIndex;
use crate::keyword::service::{KeywordPoolSnapshot, query_text_labels};
use crate::rag::{RagConfig, filter_by_persona, format_context_text};
use crate::retriever::{SearchRequest, SearchResult, UttHit};
use crate::utils::EMBED_CALL_TIMEOUT;

use super::types::{
    KeywordMirrorSource, RecallChannels, RecallHit, RecallInput, RecallOutput, RetrieverSource,
};

// =========================================================
// 共用召回装配
// =========================================================

/// 执行一次完整召回装配。
///
/// 流程:
/// 1. 闸门判定：两个通道全关 → 直接返回空输出（不生成向量、不检索）；
/// 2. 查询向量：RAG 或（白名单内）utt 需要时生成，失败降级为 None；
/// 3. 关键词镜像预取：锁内取快照 → 锁外异步查询 → 纯 `(label, score)` 交检索融合；
/// 4. 多通道检索（读锁内同步）：BM25 + 向量 + 图谱 + 关键词镜像，RRF 融合；
/// 5. 记忆子层过滤：按 `memory_layers` 剔除未请求的 L1 / L2 文档（图谱实体跟随 `l2`；两层全开 = 无操作）；
/// 6. 时间衰减（含访问加成）→ touch_l1 刷新访问时间 → 按衰减后分数重排；
/// 7. Persona-Aware 过滤（阈值取显式载体 `persona_kind`）→ 段落渲染（`[相关记忆]`）→ 覆盖集合记录；
/// 8. utt 原文检索与预算渲染（白名单内且闸门开启）。
///
/// 参数:
/// - `input`: 召回输入（依赖句柄 + 生效配置 + 闸门 + 时间基准）。
///
/// 返回:
/// - 召回输出；任何降级路径都返回结构完整的输出而非错误（调用方按空值处理）。
///
/// 说明:
/// - 本函数是在线管线与服务层 recall 的唯一召回实现，行为等价由两入口共用同一份代码保证。
pub async fn assemble_recall<R: RetrieverSource + ?Sized, K: KeywordMirrorSource + ?Sized>(
    input: RecallInput<'_, R, K>,
) -> RecallOutput {
    let mut output = RecallOutput::default();

    // ---- 1. 闸门判定：两通道全关时直接返回（不生成向量、不检索） ----
    if !input.gates.memory_rag && !input.gates.utt {
        tracing::debug!("召回闸门全关，跳过检索装配");
        return output;
    }

    // utt 是否对本 persona 生效（原文白名单双闸门：开关 + 人格类型；类型取显式载体）。
    // 查询向量仅在 RAG 或 utt（白名单内）需要时生成，避免无效 embedding 调用。
    let utt_persona_allowed = input.gates.utt
        && input
            .persona_kind
            .is_some_and(|kind| input.utt.persona_kind_whitelist.contains(&kind));
    let need_query_vec = input.gates.memory_rag || utt_persona_allowed;

    // ---- 2. 查询向量（锁外 await；失败降级为 None） ----
    let query_vec: Option<Vec<f32>> = if !need_query_vec {
        tracing::debug!("无需查询向量，仅执行无需向量的通道");
        None
    } else {
        match input.embedding {
            Some(provider) if provider.is_available() => {
                match tokio::time::timeout(EMBED_CALL_TIMEOUT, provider.embed(input.query)).await {
                    Ok(Ok(vec)) => {
                        tracing::debug!(dim = vec.len(), "查询向量已生成");
                        Some(vec)
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(%e, "查询向量生成失败，向量通道降级");
                        None
                    }
                    Err(_) => {
                        tracing::warn!(
                            timeout_ms = EMBED_CALL_TIMEOUT.as_millis() as u64,
                            "查询向量生成超时，向量通道降级"
                        );
                        None
                    }
                }
            }
            Some(_) => {
                tracing::debug!("嵌入模型不可用，跳过向量通道");
                None
            }
            None => {
                tracing::debug!("嵌入模型未配置，跳过向量通道");
                None
            }
        }
    };

    // ---- 3. 关键词镜像通道预取（混合检索第四通道） ----
    // 镜像查询异步（语义层需 embedding），必须在检索器读锁之前完成；
    // 预取后仅把 (label, score) 纯数据交给同步 search（无句柄泄漏）。
    let mut keyword_hits: Option<Vec<(String, f64)>> = None;
    if input.gates.memory_rag && input.retrieval.enable_keyword_channel {
        let kw_top_k = input.retrieval.l1_retrieve_top_k as usize;
        let snapshot: Option<(Arc<CompositeIndex>, KeywordPoolSnapshot)> =
            input.keyword_mirror.with_keyword_mirror(|mirror| {
                mirror
                    .filter(|svc| svc.doc_count() > 0)
                    .map(|svc| (svc.composite_arc(), svc.pool_snapshot()))
            });
        match snapshot {
            Some((composite, pool)) => {
                let hits = query_text_labels(
                    &composite,
                    &pool,
                    input.query,
                    input.persona_uid,
                    input.embedding,
                    kw_top_k,
                )
                .await;
                if hits.is_empty() {
                    tracing::debug!("关键词镜像无命中，跳过关键词通道");
                } else {
                    tracing::debug!(hits = hits.len(), "关键词镜像通道命中");
                    keyword_hits = Some(hits);
                }
            }
            None => {
                tracing::debug!("关键词镜像无文档，跳过关键词通道");
            }
        }
    }

    // ---- 4. 多通道检索 + utt 检索（读锁内同步，无 await） ----
    let query_owned = input.query.to_string();
    let persona_owned = input.persona_uid.map(str::to_string);
    let search_top_k = input.retrieval.l1_retrieve_top_k as usize;
    let utt_top_k = input.utt.retrieve_top_k as usize;
    let rag_active = input.gates.memory_rag;
    let keyword_for_search = keyword_hits.clone();
    let query_vec_snapshot = query_vec.clone();

    let (mut results, utt_hits): (Vec<SearchResult>, Vec<UttHit>) =
        input.retriever.with_retriever(|retriever| {
            let Some(retriever) = retriever else {
                tracing::debug!("检索索引未加载，本次召回为空（不视为故障）");
                return (Vec::new(), Vec::new());
            };

            // 4.1 摘要路多通道检索（RAG 闸门关闭时不执行）
            let results = if rag_active {
                let request = SearchRequest {
                    query: query_owned.clone(),
                    persona_uid: persona_owned.clone(),
                    top_k: search_top_k,
                    filter_share: true,
                };
                match (query_vec_snapshot.as_deref(), keyword_for_search) {
                    (Some(qv), Some(hits)) => {
                        retriever.search_with_keyword_hits(&request, Some(qv), Some(hits))
                    }
                    (Some(qv), None) => retriever.search(&request, Some(qv)),
                    (None, Some(hits)) => {
                        retriever.search_with_keyword_hits(&request, None, Some(hits))
                    }
                    (None, None) => retriever.search(&request, None),
                }
            } else {
                Vec::new()
            };

            // 4.2 utt 原文块检索（白名单内且闸门开启；严格按 persona 隔离）
            let utt_hits = if utt_persona_allowed {
                match persona_owned.as_deref() {
                    Some(puid) => retriever.search_utt(
                        &query_owned,
                        query_vec_snapshot.as_deref(),
                        utt_top_k,
                        Some(puid),
                    ),
                    None => Vec::new(),
                }
            } else {
                Vec::new()
            };

            (results, utt_hits)
        });

    // ---- 4.3 记忆子层过滤（L1 / L2 分层开关） ----
    // 只请求单一子层时，另一层既不进段落文本也不进结构化条目（服务层 include 语义）；
    // 图谱实体来源为 L2 事件，跟随 l2 开关，仅请求 L1 时同样被剔除；
    // 两层全开（在线管线默认）时整个分支跳过，零开销、行为不变。
    if !(input.memory_layers.l1 && input.memory_layers.l2) {
        let before = results.len();
        results.retain(|r| input.memory_layers.allows(&r.layer));
        tracing::debug!(
            before,
            after = results.len(),
            l1 = input.memory_layers.l1,
            l2 = input.memory_layers.l2,
            "记忆子层过滤已应用（未请求的子层不参与段落与条目）"
        );
    }

    // ---- 5. 时间衰减（含访问加成）+ touch 刷新 + 重排 ----
    if !results.is_empty() {
        let decay_config_l1 = DecayConfig::from_core(input.decay, "l1");
        let decay_config_l2 = DecayConfig::from_core(input.decay, "l2");

        // 收集命中的 L1 文档 id（供后续 touch 接线刷新 last_accessed_at）
        let mut touched_l1_ids: Vec<Uuid> = Vec::new();

        for r in &mut results {
            let decay_config = if r.layer == "l2" {
                &decay_config_l2
            } else {
                &decay_config_l1
            };

            // salience: SearchResult 不携带此字段，使用中性值 0.5
            let salience = 0.5;
            // calc_retention = calc_decay_r + apply_access_boost：
            // 近期被访问的 L1（last_accessed_at 已由 touch 刷新）保留率保底，
            // 使"刚聊过的话题"在衰减排序中更易召回。
            let decay_factor = calc_retention(
                r.created_at,
                r.last_accessed_at,
                input.now_ms,
                salience,
                decay_config,
            );
            r.rrf_score *= decay_factor;

            if r.layer == "l1" {
                if let DocId::L1(id) = r.doc_id {
                    touched_l1_ids.push(id);
                }
            }

            tracing::trace!(
                doc_id = %r.doc_id,
                layer = %r.layer,
                last_accessed = ?r.last_accessed_at,
                decay_factor = format!("{:.4}", decay_factor),
                rrf_adjusted = format!("{:.4}", r.rrf_score),
                "时间衰减已应用（含访问加成）"
            );
        }

        // ---- 5.1 touch 接线：检索命中更新 last_accessed_at ----
        // 异步更新不阻塞检索：失败仅 warn（本次降级为无访问加成），
        // 成功使近期被检索的 L1 在下次检索中获得保底保留率（recent_boost_floor）。
        if !touched_l1_ids.is_empty() {
            let ids_for_touch = touched_l1_ids;
            if let Err(e) = input.storage.touch_l1(&ids_for_touch, input.now_ms).await {
                tracing::warn!(
                    count = ids_for_touch.len(),
                    error = %e,
                    "L1 访问时间刷新失败（touch 降级，访问加成本次不生效）"
                );
            } else {
                tracing::debug!(
                    count = ids_for_touch.len(),
                    "L1 访问时间已刷新（touch 接线，激活 recent_boost_*）"
                );
            }
        }

        // 重新按衰减后 rrf_score 降序排序
        results.sort_by(|a, b| {
            b.rrf_score
                .partial_cmp(&a.rrf_score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        // ---- 5.2 通道命中计数（诊断；基于融合后结果） ----
        output.channels = RecallChannels {
            vector: results.iter().filter(|r| r.vector_score.is_some()).count(),
            bm25: results.iter().filter(|r| r.bm25_score.is_some()).count(),
            keyword: keyword_hits.as_ref().map(Vec::len).unwrap_or(0),
            graph: results.iter().filter(|r| r.graph_score.is_some()).count(),
        };
        output.fused_count = results.len();

        // ---- 5.3 Persona-Aware 过滤 + 段落渲染 ----
        // 阈值按显式载体 `persona_kind` 选取；未知类型（None）保守按最严（char 阈值）。
        let persona_kind = input.persona_kind.unwrap_or(PersonaKind::Char);
        let rag_config = RagConfig::from_retrieval_config(input.retrieval);
        let filtered = filter_by_persona(&results, persona_kind, &rag_config);
        output.filtered_count = filtered.len();

        if filtered.is_empty() {
            tracing::debug!("Persona-Aware 过滤后无结果");
        } else {
            let context = format_context_text(&filtered, &rag_config);

            // 记录"实际进入上下文文本"的文档标识（RAG 覆盖集合）。
            // 语义边界: 与 format_context_text 同批——只取过滤后、受
            // rag_max_memories 截断约束的文档（L1 uuid / L2 事件 id）；
            // 图谱实体无文档映射，不纳入覆盖集合。
            output.doc_labels = filtered
                .iter()
                .take(rag_config.max_memories)
                .filter(|r| matches!(&r.doc_id, DocId::L1(_) | DocId::L2(_)))
                .map(|r| r.doc_id.to_string())
                .collect();

            output.hits = filtered
                .iter()
                .take(rag_config.max_memories)
                .map(|r| RecallHit {
                    layer: r.layer.clone(),
                    id: r.doc_id.to_string(),
                    text: r.doc_summary.clone(),
                    score: r.rrf_score,
                    created_at: r.created_at,
                })
                .collect();

            tracing::debug!(
                total_results = results.len(),
                filtered = filtered.len(),
                context_chars = context.chars().count(),
                covered_labels = output.doc_labels.len(),
                "记忆上下文已组装（含时间衰减）"
            );

            output.memory_context = Some(context);
        }
    } else {
        tracing::debug!("无记忆上下文（utt 通道继续）");
    }

    // ---- 6. utt 原文片段渲染（预算裁剪在渲染内完成，不做块内截断） ----
    if !utt_hits.is_empty() {
        let rendered = crate::prompt::builder::render_utt_context(
            &utt_hits,
            input.utt.max_block_chars as usize,
        );
        if !rendered.is_empty() {
            tracing::debug!(
                persona_uid = input.persona_uid.unwrap_or("none"),
                hits = utt_hits.len(),
                budget_chars = input.utt.max_block_chars,
                "utt 原文片段已渲染（不记录内容）"
            );
            output.utt_context = Some(rendered);
        }
    } else if utt_persona_allowed {
        tracing::debug!("utt 原文块无命中，跳过注入");
    } else {
        tracing::debug!(
            utt_gate = input.gates.utt,
            persona_uid = input.persona_uid.unwrap_or("none"),
            "utt 通道未生效（闸门关闭或 persona 不在白名单），跳过原文注入"
        );
    }

    output
}

// =========================================================
// 辅助
// =========================================================

/// 返回当前 Unix 毫秒时间（与内核 `now_ms` 同源，便于调用方构造输入）。
pub fn current_now_ms() -> i64 {
    now_ms()
}
