//! crates/ramaria-service/src/utt.rs - utt 话语块重建用例
//!
//! 设计特点:
//! - 重建基准：以当前生效配置（config.toml 与 DB 侧合并）的 `[utt]` 组为切分参数；
//!   `[utt].enabled = false` 时跳过重建并返回禁用结果（统计为零）
//! - `force` 语义：先逐会话清空全部旧块再全量重切；切分参数变更后必须使用
//!   （增量语义只重切每会话尾块，旧块不会按新参数重切）
//! - 完成后刷新内存检索索引（含 utt 向量通道），使新块立即可检索
//! - 统计口径：新建 / 跳过 / 删除 / embedding 成败与索引文档数（L1 + L2，不含 utt 块）
//! - 降级：embedding 不可用时块照常入库（仅无向量），不阻塞重建

use std::time::Instant;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::EmbeddingProvider;
use ramaria_memory::utt::builder::UttBuilder;

use crate::engine::Engine;

// =========================================================
// 结果类型
// =========================================================

/// utt 块重建结果。
///
/// 字段约定:
/// - `rebuilt`: 是否执行了重建（`false` = `[utt].enabled = false`，未做任何构建与清理，
///   统计字段为零，切分参数仍为当前生效值）；
/// - `force`: 本次是否按 `--force` 全量重切口径执行；
/// - `force_removed`: `--force` 预清理删除的旧块数（逐会话删除之和）；
/// - `chunks_*` / `embedding_*`: 全量构建统计的聚合口径（与构建器同源）；
/// - `doc_count`: 重建后内存检索索引的文档数（L1 + L2，不含 utt 块）。
#[derive(Debug, Clone, Default)]
pub struct UttRebuildOutcome {
    pub rebuilt: bool,
    pub force: bool,
    pub theta_gap_minutes: u32,
    pub max_msgs_per_block: u32,
    pub force_removed: usize,
    pub chunks_created: usize,
    pub chunks_skipped: usize,
    pub chunks_removed: usize,
    pub embedding_ok: usize,
    pub embedding_failed: usize,
    pub elapsed_ms: u64,
    pub doc_count: usize,
}

// =========================================================
// 重建用例
// =========================================================

/// 重建全部会话的 utt 话语块（可选 `--force` 全量重切）并刷新检索索引。
///
/// 流程:
/// 1. 读取生效配置（config.toml 与 DB 侧合并）；`[utt].enabled = false` 时跳过；
/// 2. `force = true` 时逐会话清空旧块（保证按新参数全量重切）；
/// 3. `UttBuilder::rebuild_all` 逐会话重建（增量语义，已一致块自动跳过）；
/// 4. 刷新内存检索索引（含 utt 向量通道），返回索引文档数。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `force`: 先清空全部旧块再全量重建。
///
/// 返回:
/// - `UttRebuildOutcome`：统计与耗时；配置未启用时 `rebuilt = false`。
pub(crate) async fn rebuild(engine: &Engine, force: bool) -> RamariaResult<UttRebuildOutcome> {
    // ---- 1. 读取生效配置（确保使用当前切分参数） ----
    let cfg = engine.load_full_config().await?;

    let mut outcome = UttRebuildOutcome {
        force,
        theta_gap_minutes: cfg.utt.theta_gap_minutes,
        max_msgs_per_block: cfg.utt.max_msgs_per_block,
        ..Default::default()
    };

    if !cfg.utt.enabled {
        tracing::info!("utt 配置未启用，跳过话语块重建");
        return Ok(outcome);
    }

    let storage = engine.storage_ref();

    // ---- 2. --force：清空全部旧块 ----
    if force {
        let sessions = storage.list_sessions().await?;
        let mut removed = 0usize;
        for session in &sessions {
            removed += storage.delete_utt_blocks_by_session(session.id).await?;
        }
        outcome.force_removed = removed;
        tracing::info!(removed, "--force：已清空旧 utt 块（全量重切）");
    }

    // ---- 3. 全量重建（embedding 不可用时块照常入库、仅无向量） ----
    let builder = UttBuilder::from_config(&cfg.utt);
    let embedding = engine.embedding_ref();
    let embedder: Option<&dyn EmbeddingProvider> = embedding.as_ref().map(|arc| arc.as_ref());

    let start = Instant::now();
    let stats = builder.rebuild_all(storage.as_ref(), embedder).await?;
    outcome.elapsed_ms = start.elapsed().as_millis() as u64;

    outcome.chunks_created = stats.chunks_created;
    outcome.chunks_skipped = stats.chunks_skipped;
    outcome.chunks_removed = stats.chunks_removed;
    outcome.embedding_ok = stats.embedding_ok;
    outcome.embedding_failed = stats.embedding_failed;

    // ---- 4. 刷新内存检索索引（含 utt 向量通道），使新块立即可检索 ----
    outcome.doc_count = engine.rebuild_index().await?;
    outcome.rebuilt = true;

    tracing::info!(
        force,
        force_removed = outcome.force_removed,
        chunks_created = outcome.chunks_created,
        chunks_skipped = outcome.chunks_skipped,
        chunks_removed = outcome.chunks_removed,
        embedding_ok = outcome.embedding_ok,
        embedding_failed = outcome.embedding_failed,
        elapsed_ms = outcome.elapsed_ms,
        doc_count = outcome.doc_count,
        "utt 块重建完成"
    );

    Ok(outcome)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use ramaria_core::types::{Message, MessageRole, MessageSource, Persona, PersonaKind};
    use uuid::Uuid;

    use crate::engine::EngineOptions;

    /// 装配真实引擎（配置路径在临时目录；无嵌入模型，embedding 走降级路径）。
    async fn utt_engine(tag: &str, config_toml: Option<&str>) -> (Engine, PathBuf) {
        let dir = crate::test_support::temp_dir(tag);
        let config_path = dir.join("config.toml");
        if let Some(text) = config_toml {
            std::fs::write(&config_path, text).expect("写入配置应成功");
        }
        let engine = Engine::open_with(
            EngineOptions::new(dir.join("assistant.db")).with_config_path(config_path),
        )
        .await
        .expect("引擎装配应成功");
        (engine, dir)
    }

    /// 造一个带消息的会话（消息时间自 base 递增、全部归属同一人格）。
    async fn seed_session(engine: &Engine, uid: &str, count: usize, base_ts: i64) -> Uuid {
        let storage = engine.storage_ref();
        let persona = Persona::new(
            uid.to_string(),
            "测试人格".to_string(),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage
            .create_persona(&persona)
            .await
            .expect("插入 persona 应成功");
        let session = storage
            .create_session(Some(uid))
            .await
            .expect("创建会话应成功");
        for i in 0..count {
            let role = if i % 2 == 0 {
                MessageRole::User
            } else {
                MessageRole::Assistant
            };
            let mut message =
                Message::new(session.id, role, format!("消息 {i}"), MessageSource::Local)
                    .with_persona_uid(Some(uid.to_string()));
            message.created_at = base_ts + i as i64;
            storage
                .save_message(&message)
                .await
                .expect("写入消息应成功");
        }
        session.id
    }

    /// 空库：重建走通并返回零统计（不依赖真实模型与网络）。
    #[tokio::test]
    async fn rebuild_on_empty_db_returns_zero_stats() {
        let (engine, dir) = utt_engine("utt-empty", None).await;

        let outcome = engine
            .rebuild_utt_blocks(false)
            .await
            .expect("空库重建应成功");
        assert!(outcome.rebuilt);
        assert!(!outcome.force);
        assert_eq!(outcome.chunks_created, 0);
        assert_eq!(outcome.chunks_skipped, 0);
        assert_eq!(outcome.force_removed, 0);
        assert_eq!(outcome.doc_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 有会话：首次建块 → 二次幂等跳过 → `--force` 清理并按当前参数重切。
    #[tokio::test]
    async fn rebuild_creates_blocks_then_force_resplits() {
        let (engine, dir) = utt_engine("utt-sessions", None).await;
        let session_id = seed_session(&engine, "char-0001", 3, 1_000).await;

        // 首次重建：3 条相邻消息切为 1 块
        let outcome = engine.rebuild_utt_blocks(false).await.expect("重建应成功");
        assert!(outcome.rebuilt);
        assert_eq!(outcome.chunks_created, 1);
        assert_eq!(outcome.chunks_skipped, 0);
        assert_eq!(outcome.force_removed, 0);
        // doc_count 口径：L1 + L2（不含 utt 块）
        assert_eq!(
            outcome.doc_count, 0,
            "无 L1/L2 文档时索引文档数应为 0（utt 块不计入）"
        );

        let blocks = engine
            .storage_ref()
            .list_utt_blocks_by_persona("char-0001")
            .await
            .expect("读取块应成功");
        assert_eq!(blocks.len(), 1, "块应已入库");
        assert_eq!(blocks[0].session_id, session_id);

        // 二次重建：已一致块幂等跳过
        let outcome = engine.rebuild_utt_blocks(false).await.expect("重建应成功");
        assert_eq!(outcome.chunks_created, 0);
        assert_eq!(outcome.chunks_skipped, 1);

        // --force：先清空旧块再全量重切
        let outcome = engine.rebuild_utt_blocks(true).await.expect("重切应成功");
        assert!(outcome.force);
        assert_eq!(outcome.force_removed, 1);
        assert_eq!(outcome.chunks_created, 1);
        let blocks = engine
            .storage_ref()
            .list_utt_blocks_by_persona("char-0001")
            .await
            .expect("读取块应成功");
        assert_eq!(blocks.len(), 1);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `[utt].enabled = false`：跳过重建并返回禁用结果。
    #[tokio::test]
    async fn rebuild_skips_when_disabled() {
        let (engine, dir) = utt_engine("utt-disabled", Some("[utt]\nenabled = false\n")).await;

        let outcome = engine
            .rebuild_utt_blocks(false)
            .await
            .expect("禁用路径应正常返回");
        assert!(!outcome.rebuilt);
        assert_eq!(outcome.chunks_created, 0);
        assert_eq!(outcome.force_removed, 0);
        assert_eq!(outcome.doc_count, 0);

        let _ = std::fs::remove_dir_all(&dir);
    }
}
