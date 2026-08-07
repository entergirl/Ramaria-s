//! rust/crates/ramaria-cli/src/commands/utt.rs - utt 话语块管理命令
//!
//! 设计特点:
//! - rebuild: 重建全部会话的 utt 话语块（供探针切分参数定稿后重切）
//! - 默认增量语义：逐会话委托 `UttBuilder::rebuild_all`（已一致的块自动跳过）
//! - `--force`：先清空全部 utt_blocks 再全量重建（切分参数 θ_gap/条数上限
//!   变更后必须使用——增量语义只重切每会话最后一块，旧块不会按新参数重切）
//! - 完成后自动刷新内存检索器（utt 向量通道 `L0:{utt_block_id}`）

use std::sync::Arc;

use ramaria_memory::utt::builder::UttBuilder;

/// utt 命令的子命令。
pub enum UttCmd {
    /// 重建全部会话的 utt 话语块
    Rebuild { force: bool },
}

/// 执行 utt 命令。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: UttCmd) -> anyhow::Result<()> {
    match cmd {
        UttCmd::Rebuild { force } => rebuild(app, force).await,
    }
}

/// 重建 utt 话语块。
///
/// 参数:
/// - `force`: 先清空全部 utt_blocks 再全量重建（切分参数变更后必须使用）。
async fn rebuild(app: &Arc<ramaria_app::App>, force: bool) -> anyhow::Result<()> {
    // 读取生效配置（config.toml + DB 双写合并），确保使用当前切分参数
    let config_path = std::path::PathBuf::from(&app.config().paths.config_dir).join("config.toml");
    let sync = ramaria_app::ConfigSyncService::new(app.storage().clone(), config_path);
    let cfg = sync
        .load_config_only()
        .await
        .map_err(|e| anyhow::anyhow!("读取配置失败: {e}"))?;

    if !cfg.utt.enabled {
        crate::ui::warn("utt 配置未启用（[utt].enabled=false），跳过重建");
        return Ok(());
    }

    crate::ui::info(&format!(
        "当前切分参数: θ_gap={} 分钟, 单块上限={} 条",
        cfg.utt.theta_gap_minutes, cfg.utt.max_msgs_per_block
    ));

    // --force：清空全部 utt_blocks，保证按新参数全量重切
    if force {
        let sessions = app.storage().list_sessions().await?;
        let mut removed = 0usize;
        for session in &sessions {
            removed += app
                .storage()
                .delete_utt_blocks_by_session(session.id)
                .await?;
        }
        crate::ui::info(&format!("已清空 {removed} 个旧 utt 块（--force 全量重切）"));
    }

    let builder = UttBuilder::from_config(&cfg.utt);
    // embedding provider 可选（None 时块照常入库、仅无向量）
    let embedding = app.embedding_provider();
    let embedder: Option<&dyn ramaria_core::EmbeddingProvider> =
        embedding.as_ref().map(|arc| arc.as_ref());

    let start = std::time::Instant::now();
    let stats = builder
        .rebuild_all(app.storage().as_ref(), embedder)
        .await
        .map_err(|e| anyhow::anyhow!("utt 全量构建失败: {e}"))?;
    let elapsed = start.elapsed();

    crate::ui::success(&format!(
        "utt 块构建完成 — 新建 {} / 跳过 {} / 删除 {}，embedding 成功 {} / 失败 {}，耗时 {:.1}s",
        stats.chunks_created,
        stats.chunks_skipped,
        stats.chunks_removed,
        stats.embedding_ok,
        stats.embedding_failed,
        elapsed.as_secs_f64()
    ));

    // 刷新内存检索器（含 utt 向量通道），使新块立即可检索
    let doc_count = app
        .rebuild_retriever()
        .await
        .map_err(|e| anyhow::anyhow!("检索索引重建失败: {e}"))?;
    crate::ui::info(&format!("检索器已刷新（{doc_count} 篇文档）"));

    Ok(())
}
