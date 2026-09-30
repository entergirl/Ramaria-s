//! crates/ramaria-cli/src/commands/utt.rs - utt 话语块管理命令
//!
//! 设计特点:
//! - rebuild: 重建全部会话的 utt 话语块（供探针切分参数定稿后重切）
//! - 默认增量语义：逐会话委托构建器（已一致的块自动跳过）
//! - `--force`：先清空全部 utt_blocks 再全量重建（切分参数 θ_gap/条数上限
//!   变更后必须使用——增量语义只重切每会话最后一块，旧块不会按新参数重切）
//! - 完成后自动刷新内存检索器（utt 向量通道 `L0:{utt_block_id}`）
//! - `--json` 输出信封（rebuilt / force / 块计数 / embedding 统计 / 耗时 / 文档数）

use anyhow::Context;
use ramaria_service::Engine;
use std::sync::Arc;

/// utt 命令的子命令。
pub enum UttCmd {
    /// 重建全部会话的 utt 话语块
    Rebuild { force: bool },
}

/// 执行 utt 命令。
pub async fn run(engine: &Arc<Engine>, cmd: UttCmd, json: bool) -> anyhow::Result<()> {
    match cmd {
        UttCmd::Rebuild { force } => rebuild(engine, force, json).await,
    }
}

/// 重建 utt 话语块。
///
/// 参数:
/// - `force`: 先清空全部 utt_blocks 再全量重建（切分参数变更后必须使用）。
/// - `json`: `--json` 信封输出（配置未启用与成功路径均输出结构化数据）。
async fn rebuild(engine: &Arc<Engine>, force: bool, json: bool) -> anyhow::Result<()> {
    // 读取生效配置（config.toml + DB 双写合并），确保使用当前切分参数
    let cfg = engine
        .load_full_config()
        .await
        .map_err(|e| anyhow::anyhow!("读取配置失败: {e}"))?;

    if !cfg.utt.enabled {
        if json {
            return crate::json::emit_ok(&serde_json::json!({
                "rebuilt": false,
                "reason": "utt_disabled",
            }));
        }
        crate::ui::warn("utt 配置未启用（[utt].enabled=false），跳过重建");
        return Ok(());
    }

    crate::ui::info(&format!(
        "当前切分参数: θ_gap={} 分钟, 单块上限={} 条",
        cfg.utt.theta_gap_minutes, cfg.utt.max_msgs_per_block
    ));

    // 重建（--force 时先清空全部旧块再全量重切；完成后刷新内存检索器）
    let outcome = engine
        .rebuild_utt_blocks(force)
        .await
        .context("utt 全量构建失败")?;

    if force {
        crate::ui::info(&format!(
            "已清空 {} 个旧 utt 块（--force 全量重切）",
            outcome.force_removed
        ));
    }

    if json {
        return crate::json::emit_ok(&serde_json::json!({
            "rebuilt": true,
            "force": force,
            "chunks_created": outcome.chunks_created,
            "chunks_skipped": outcome.chunks_skipped,
            "chunks_removed": outcome.chunks_removed,
            "embedding_ok": outcome.embedding_ok,
            "embedding_failed": outcome.embedding_failed,
            "elapsed_ms": outcome.elapsed_ms,
            "doc_count": outcome.doc_count,
        }));
    }

    crate::ui::success(&format!(
        "utt 块构建完成 — 新建 {} / 跳过 {} / 删除 {}，embedding 成功 {} / 失败 {}，耗时 {:.1}s",
        outcome.chunks_created,
        outcome.chunks_skipped,
        outcome.chunks_removed,
        outcome.embedding_ok,
        outcome.embedding_failed,
        outcome.elapsed_ms as f64 / 1000.0
    ));
    crate::ui::info(&format!("检索器已刷新（{} 篇文档）", outcome.doc_count));

    Ok(())
}
