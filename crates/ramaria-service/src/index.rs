//! crates/ramaria-service/src/index.rs - 检索索引构建、懒加载与增量镜像
//!
//! 设计特点:
//! - 懒加载：首次召回前构建一次（进程启动不为大库付加载代价），构建完成后整体替换
//! - 显式重建：`rebuild` 跳过懒加载早退与冷却窗口，供宿主手动刷新整库索引
//! - 原子替换：新索引在临时实例上完整构建，成功后一次写入替换；读者要么见旧索引，
//!   要么见新索引，不会读到半成品；构建失败旧索引保持可用并置"重建失败"告警位
//! - 数据来源与在线管线一致：各 persona 的未吸收 L1 + L2 事件 + utt 块，另加无主 L1
//! - 降级链：嵌入模型缺失 / 向量通道关闭 → 仅 BM25 + 关键词镜像（不阻塞构建）；
//!   缺失向量补算失败只跳过该条目；关键词词表读取失败 → 空词典（纯 bigram 口径）
//! - 向量持久化：构建期按模型指纹读取记忆向量，缺失条目在索引可用后分批补算并写回
//! - BM25 词典增强迁移：旧分词版本 + 已确认词表非空 → 重建切为词典增强口径并写回版本标记
//! - 增量镜像：L1 生成后同步进检索器与关键词镜像（不重建整库）
//! - 代次刷新：召回前比对库内语料戳与 BM25 分词代次，其他进程写入 / 词典升级后
//!   重建内存索引（同进程脏标记保留为加载窗口内的兜底）；
//!   代次变化触发的重建再受 `[index].refresh_interval_seconds` 冷却窗口约束（0 = 不节流）
//! - 边界：本模块负责内存索引与记忆向量的维护；除记忆向量与 BM25 分词版本标记外不写业务表

mod build;
mod rebuild;
mod stamp;
pub(crate) mod status;
mod vectors;
mod view;

pub(crate) use build::index_utt_blocks_incrementally;
use rebuild::build_and_swap;
pub use stamp::{IndexBuildFailure, IndexStamp};

use std::collections::HashSet;

use ramaria_core::error::RamariaResult;
use ramaria_core::lock::write_recover;
use ramaria_core::types::{IndexStatus, MemoryL1, VECTOR_DOC_L1, now_ms};
use ramaria_memory::keyword::{CommaSeparatedNormalizer, KeywordNormalizer};

use crate::engine::Engine;

use status::read_stamp;
pub(crate) use status::{IndexStatusState, percent};
use view::l1_view;

// =========================================================
// 懒加载与显式重建
// =========================================================

/// 确保检索索引已加载且与库内语料同代（懒加载 + 代次刷新）。
///
/// 流程:
/// 1. 读取库内代次快照（语料统计 + BM25 分词代次）；
/// 2. 已加载、无脏标记、代次一致 → 直接返回 false；
/// 3. 否则走 [`build_and_swap`] 完整构建路径（清脏后构建）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - `Ok(true)`: 本次调用完成构建（首次加载 / 脏标记重建 / 代次变化刷新）。
/// - `Ok(false)`: 索引已就绪且无需重建。
///
/// 说明:
/// - 并发调用可能重复构建（幂等：最后一次替换生效）。
/// - 脏标记协同（避免丢 L1）：加载窗口内封存产生的新 L1 进不了内存索引，其增量镜像会置脏；
///   本函数在开始构建前清脏，构建期间再产生的增量会重新置脏 → 下次调用再补一次（收敛）。
/// - 代次协同（跨进程）：构建时记录的是构建前读取的快照；构建窗口内其他进程的新写入
///   会让下次比对不等 → 再刷新一次（收敛，不漏新记忆）。
/// - 构建失败时置"重建失败"告警位并上抛错误（旧索引保持可用，见 [`build_and_swap`]）。
pub(crate) async fn ensure_loaded(engine: &Engine) -> RamariaResult<bool> {
    // 后台构建在途 / 状态机处于 Building：本次不参与构建，按"索引可先用"返回
    if engine.is_index_build_inflight() || engine.index_status().status == IndexStatus::Building {
        tracing::debug!("索引正在构建，跳过本次懒加载");
        return Ok(false);
    }

    // 已加载且无脏标记时先判冷却窗口与短窗口：窗口内无需读库内代次（省去全表统计）
    if engine.is_retriever_loaded() && !engine.index_dirty() {
        if !engine.index_rebuild_cooldown_elapsed() {
            tracing::debug!(
                interval_seconds = engine.config().index.refresh_interval_seconds,
                "索引重建冷却窗口内：本次沿用现有索引"
            );
            return Ok(false);
        }
    }

    let stamp = read_stamp(engine).await;

    // 已加载且无脏标记 → 确认代次是否仍一致；一致则免构建
    if engine.is_retriever_loaded() && !engine.index_dirty() {
        if engine.index_stamp() == Some(stamp) {
            return Ok(false);
        }
        // 代次变化（其他进程写入 / 分词代次升级）触发的重建受配置的最小间隔约束：
        // 冷却窗口内沿用现有索引（窗口过后的下一次召回在同一分支补上重建），
        // 用于写入密集期抑制整库重建风暴；首次加载与同进程脏标记路径不受此约束。
        if !engine.index_rebuild_cooldown_elapsed() {
            tracing::debug!(
                bm25_version = stamp.bm25_version,
                interval_seconds = engine.config().index.refresh_interval_seconds,
                "索引代次已变化，但在重建冷却窗口内：本次沿用现有索引"
            );
            return Ok(false);
        }
        tracing::info!(
            bm25_version = stamp.bm25_version,
            corpus_tracked = stamp.corpus.is_some(),
            "索引代次变化（其他进程写入或分词代次升级），重建内存索引"
        );
    }
    // 与后台构建 / 显式重建串行（避免两次构建互相覆盖）；
    // 拿到锁后再清脏：等待期间的增量会被本次构建读取，不会丢失。
    let _guard = engine.index_build_lock().lock().await;
    engine.clear_index_dirty();
    build_and_swap(engine).await?;
    Ok(true)
}

/// 强制全量重建内存检索索引（跳过懒加载早退与冷却窗口）。
///
/// 用法:
/// - 宿主显式刷新（批量导入完成 / 设置变更 / 诊断修复等场景）调用；
/// - 与懒加载路径共用同一构建实现，不受 `[index].refresh_interval_seconds` 约束。
///
/// 返回:
/// - `Ok(total)`: 重建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
/// - `Err(..)`: 构建失败（旧索引保持可用，告警位置位）。
pub(crate) async fn rebuild(engine: &Engine) -> RamariaResult<usize> {
    let _guard = engine.index_build_lock().lock().await;
    build_and_swap(engine).await
}
// 增量镜像（L1 生成后）
// =========================================================

/// 把新生成的 L1 同步进检索器与关键词镜像（不重建整库）。
///
/// 行为:
/// - 检索器已加载 → 生成摘要向量（嵌入可用时）后增量索引；
///   未加载 → 跳过（下次懒加载会从存储全量载入，不会漏）。
/// - 关键词镜像 → 倒排增量 + 词典池累积（语义层不随增量重建，保持既有方言）；
///   持久化为 `pending` 的词条不进入内存池——词表与持久化状态保持一致
///   （pending 不算已建立词条，不参与分词字典 / 语义层口径）。
///
/// 降级:
/// - 向量生成失败 → 仅 BM25（记 debug，不阻塞）；
/// - 词条状态查询失败 → 跳过 pending 过滤（按旧口径全部入池）。
pub(crate) async fn index_l1_into_mirrors(engine: &Engine, l1: &MemoryL1) {
    let doc = l1_view(l1);

    // 向量（向量通道开启且嵌入可用时；失败/不可用 → None）
    let vector = if engine.config().retrieval.enable_vector {
        match engine.embedding_ref() {
            Some(provider) if provider.is_available() => match provider.embed(&l1.summary).await {
                Ok(vector) => Some(vector),
                Err(e) => {
                    tracing::debug!(error = %e, "L1 增量向量生成失败，仅入 BM25 索引");
                    None
                }
            },
            _ => None,
        }
    } else {
        None
    };

    // 向量落库（与内存镜像同源；失败只记日志，不影响检索索引）
    if let Some(vec) = vector.as_deref() {
        crate::embedding_store::store(engine, VECTOR_DOC_L1, &l1.id.to_string(), vec).await;
    }

    // 检索器增量
    {
        // 构建在途时写旧槽会在替换后丢失：直接置脏，由下一次构建覆盖该条
        if engine.is_index_build_inflight() || engine.index_status().status == IndexStatus::Building
        {
            engine.mark_index_dirty();
            tracing::debug!(
                l1_id = %l1.id,
                "检索索引构建中，已置脏标记（本次 L1 由下次构建覆盖）"
            );
        } else {
            let slot = engine.retriever_slot();
            let mut guard = write_recover(&*slot, "index.retriever_slot");
            if let Some(retriever) = guard.as_mut() {
                retriever.index_l1_with_vector(&doc, vector);
                tracing::debug!(l1_id = %l1.id, "L1 已增量加入检索器索引");
            } else {
                // 索引未加载：本次增量进不了内存索引，置脏标记保证下次加载会重建并覆盖该 L1
                // （否则"加载窗口内封存的 L1"在进程生命周期内一直检索不到）
                engine.mark_index_dirty();
                tracing::debug!(
                    l1_id = %l1.id,
                    "检索索引未加载，已置脏标记（下次加载重建，本次 L1 不会漏）"
                );
            }
        }
    }

    // 关键词镜像增量
    {
        let tokens = CommaSeparatedNormalizer.normalize(doc.keywords.as_deref().unwrap_or(""));
        let now = now_ms();

        // pending 词条不进入内存池（established 词表与持久化状态一致）；
        // 状态查询在拿镜像写锁之前完成，不持锁跨 await。
        let texts: Vec<String> = tokens.iter().map(|t| t.as_str().to_string()).collect();
        let pending_texts: HashSet<String> = match engine
            .storage_ref()
            .list_keyword_statuses(&texts)
            .await
        {
            Ok(rows) => rows
                .into_iter()
                .filter(|(_, alias_status)| alias_status.as_deref() == Some("pending"))
                .map(|(keyword, _)| keyword)
                .collect(),
            Err(e) => {
                tracing::debug!(error = %e, "词条状态查询失败，pending 过滤跳过（按旧口径入池）");
                HashSet::new()
            }
        };
        let accepted: Vec<ramaria_core::keyword::KeywordToken> = tokens
            .into_iter()
            .filter(|token| !pending_texts.contains(token.as_str()))
            .collect();

        let mirror = engine.keyword_mirror();
        let mut guard = write_recover(&*mirror, "index.keyword_mirror");
        guard.index_l1(&doc);
        guard.upsert_pool_tokens(&accepted, now);
        // 语义层陈旧可观测（节流 5 分钟，日志不含词条文本）
        guard.warn_if_fuzzy_stale(now);
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
