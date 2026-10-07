//! crates/ramaria-memory/src/utt/builder.rs - utt 话语块构建器
//!
//! 设计特点:
//! - 全量构建（`rebuild_all`，幂等）与增量构建（`build_session`，封存钩子调用）
//! - 增量语义：只重切"最后一个已入库块"及其后的消息，更早的块原样保留
//! - 幂等判定：重切首块与库中最后一块的 (start,end,msg_count) 一致 → 跳过写入
//! - embedding 由调用方注入 `EmbeddingProvider`；失败降级（块照常入库，记 warn）
//! - 原文隐私：块文本含发言人标记；构建日志只记计数与 ID，不记原文内容
//!
//! 块文本格式:
//! - 每行 `[YYYY-MM-DD HH:MM] 角色: 内容`，块内消息按时间升序
//! - 角色名：目标 persona 用其注册名（查询失败回退 uid），用户消息显示"用户"
//! - 消息正文中的 `[图片#{hash}]` 占位符按附件描述渲染为 `[图片: {描述}]`
//!   （无描述 / 查询失败保留原占位符）

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{Local, TimeZone};
use ramaria_core::config::UttConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::lock::lock_recover;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{
    Message, Session, UttBlock, build_render_map, replace_image_placeholders,
};
use tracing::{info, warn};
use uuid::Uuid;

use super::splitter::split_messages;
use super::{UttChunk, UttSplitterConfig, encode_embedding, infer_target_persona_from_messages};

/// rama 自身会话（Session.persona_uid 为 None）使用的块归属 UID。
const RAMA_FALLBACK_UID: &str = "rama-0001";

/// utt 构建配置（切分参数 + 内容级去重开关）。
#[derive(Debug, Clone, Copy, Default)]
pub struct UttBuildConfig {
    /// 切分参数
    pub splitter: UttSplitterConfig,
    /// 内容级去重开关（生产路径经 `from_config` 默认开启）。
    ///
    /// `true` → 同一构建周期内内容未变的块复用已生成的 embedding，
    /// 避免重复会话/未变块全量重算（全量重建退化为增量 O(变动块)）。
    /// `false` → 逐块重算 embedding（性能兜底可关闭）。
    pub content_dedup: bool,
}

/// 一次构建的统计结果（供日志与测试断言）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UttBuildStats {
    /// 处理的会话 ID
    pub session_id: Option<Uuid>,
    /// 新建块数
    pub chunks_created: usize,
    /// 幂等跳过的块数（库中已一致，未写入）
    pub chunks_skipped: usize,
    /// 因重切删除的过期块数
    pub chunks_removed: usize,
    /// 成功生成 embedding 的块数
    pub embedding_ok: usize,
    /// 内容级去重复用的 embedding 块数（未重算）
    pub embedding_reused: usize,
    /// embedding 生成失败的块数（降级为无向量）
    pub embedding_failed: usize,
}

/// utt 话语块构建器。
///
/// 职责:
/// - 从消息序列切分话语块、渲染块文本、生成 embedding、写入存储。
/// - 提供幂等的全量重建与增量构建。
///
/// 使用:
/// - 封存钩子：`build_session`（只处理本会话尾部）。
/// - 全量重建：`rebuild_all`（遍历全部会话，内部逐会话走增量语义；
///   幂等，已一致的块跳过）。CLI 入口：`ramaria utt rebuild`；
///   切分参数变更后需 `--force`（先清空旧块再全量重切）。
pub struct UttBuilder {
    /// 构建配置
    config: UttBuildConfig,
    /// 内容级去重缓存：块文本内容 hash → embedding 向量。
    ///
    /// 说明:
    /// - 在同一构建器生命周期内跨会话复用（`rebuild_all` 遍历全部会话）。
    /// - 内容未变的块复用缓存向量，避免重复重算 embedding。
    /// - 用 `Mutex` 保护：`write_chunk` 在 async 中持锁时间极短（hash 查找/插入），
    ///   不跨 `.await` 持锁（embedding 计算在锁外）。
    embedding_cache: Mutex<HashMap<u64, Vec<f32>>>,
}

impl UttBuilder {
    /// 创建构建器。
    ///
    /// 参数:
    /// - `config`: 切分配置。
    pub fn new(config: UttBuildConfig) -> Self {
        Self {
            config,
            embedding_cache: Mutex::new(HashMap::new()),
        }
    }

    /// 从应用配置创建构建器（`[utt]` 组）。
    ///
    /// 说明:
    /// - 调用方仍需自行检查 `UttConfig.enabled`；本方法只取切分参数。
    pub fn from_config(cfg: &UttConfig) -> Self {
        Self::new(UttBuildConfig {
            splitter: UttSplitterConfig {
                theta_gap_minutes: cfg.theta_gap_minutes,
                max_msgs_per_block: cfg.max_msgs_per_block,
            },
            content_dedup: true,
        })
    }

    // =========================================================
    // 增量构建（封存钩子入口）
    // =========================================================

    /// 增量构建单个会话的话语块。
    ///
    /// 增量语义:
    /// 1. 读取会话全部消息（时间升序）。
    /// 2. 若库中已有该会话最后一块，从"最后一块的 start_msg_id"起重切
    ///    （覆盖旧尾块 + 其后新增消息，保证 θ_gap 边界正确）。
    /// 3. 重切首块与库中最后一块一致 → 幂等跳过；否则删除旧尾块并写入新块。
    /// 4. 更早的块原样保留（不重切、不重新生成 embedding）。
    ///
    /// 降级:
    /// - 消息读取失败 → 返回 Err（由调用方决定是否阻塞，封存路径记 warn 不阻塞）。
    /// - embedding 生成失败 → 块照常入库（embedding=None），记 warn。
    ///
    /// 参数:
    /// - `storage`: 存储后端。
    /// - `session`: 目标会话（`persona_uid` 决定目标 persona）。
    /// - `embedder`: 可选的 embedding provider（None 表示不生成向量）。
    ///
    /// 返回:
    /// - 构建统计（新建/跳过/删除/embedding 结果）。
    pub async fn build_session(
        &self,
        storage: &dyn StorageBackend,
        session: &Session,
        embedder: Option<&dyn EmbeddingProvider>,
    ) -> RamariaResult<UttBuildStats> {
        let mut stats = UttBuildStats {
            session_id: Some(session.id),
            ..Default::default()
        };

        // P0-2 修复：目标 persona 优先取 session.persona_uid；
        // 存量 NULL 会话（历史缺陷）防御性从消息首条 assistant 发言推断，
        // 两者都缺失才回退 rama-0001（rama 自身会话）。
        let messages = storage.list_messages(session.id).await?;
        if messages.is_empty() {
            return Ok(stats);
        }

        let target = session
            .persona_uid
            .clone()
            .or_else(|| infer_target_persona_from_messages(&messages))
            .unwrap_or_else(|| RAMA_FALLBACK_UID.to_string());
        if session.persona_uid.is_none() && target != RAMA_FALLBACK_UID {
            warn!(
                %session.id,
                persona_uid = %target,
                "会话 persona_uid 为 NULL，已从消息推断目标 persona（存量兼容）"
            );
        }

        // 定位增量重切起点：库中最后一块的 start_msg_id
        let last = storage.get_latest_utt_block_by_session(session.id).await?;
        let (_, messages_ref) = match &last {
            Some(block) => match messages.iter().position(|m| m.id == block.start_msg_id) {
                Some(i) => (i, &messages[i..]),
                None => {
                    // 数据不一致：库中块的起点不在消息列表中（如消息被清理）。
                    // 防御：按全量重切处理。
                    warn!(
                        %session.id,
                        block_id = block.id,
                        "库中 utt 块起点消息缺失，按全量重切"
                    );
                    (0, &messages[..])
                }
            },
            None => (0, &messages[..]),
        };

        let chunks = split_messages(messages_ref, Some(&target), &self.config.splitter);
        if chunks.is_empty() {
            // 本会话无任何目标发言 → 无需建块；若库中有旧块（参数调整后变空），清理尾部
            if let Some(block) = &last {
                storage.delete_utt_block(block.id).await?;
                stats.chunks_removed += 1;
            }
            return Ok(stats);
        }

        // 幂等判定：重切首块与库中最后一块一致 → 只处理其后新增的块
        if let Some(block) = &last {
            let first = &chunks[0];
            let same = first.start_msg_id == block.start_msg_id
                && first.end_msg_id == block.end_msg_id
                && first.msg_count == block.msg_count;
            if same {
                stats.chunks_skipped += 1;
                for c in &chunks[1..] {
                    self.write_chunk(storage, c, &target, embedder, &mut stats)
                        .await?;
                }
                return Ok(stats);
            }
            // 尾块内容变化（新增消息或参数调整）→ 删除旧尾块，重写
            storage.delete_utt_block(block.id).await?;
            stats.chunks_removed += 1;
        }

        for c in &chunks {
            self.write_chunk(storage, c, &target, embedder, &mut stats)
                .await?;
        }

        Ok(stats)
    }

    // =========================================================
    // 全量构建（启动 / 索引重建）
    // =========================================================

    /// 全量构建全部会话的话语块（幂等）。
    ///
    /// 说明:
    /// - 遍历全部会话，逐会话委托 [`build_session`]（增量语义），
    ///   已一致的块自动跳过（不重新生成 embedding）。
    /// - 单个会话失败不中断整体：记 warn 并继续（降级不阻塞）。
    ///
    /// 返回:
    /// - 聚合统计（各会话计数之和）。
    pub async fn rebuild_all(
        &self,
        storage: &dyn StorageBackend,
        embedder: Option<&dyn EmbeddingProvider>,
    ) -> RamariaResult<UttBuildStats> {
        let sessions = storage.list_sessions().await?;
        let mut total = UttBuildStats::default();

        for session in &sessions {
            match self.build_session(storage, session, embedder).await {
                Ok(stats) => {
                    total.chunks_created += stats.chunks_created;
                    total.chunks_skipped += stats.chunks_skipped;
                    total.chunks_removed += stats.chunks_removed;
                    total.embedding_ok += stats.embedding_ok;
                    total.embedding_reused += stats.embedding_reused;
                    total.embedding_failed += stats.embedding_failed;
                }
                Err(e) => {
                    warn!(%session.id, %e, "utt 全量构建跳过失败会话（不中断整体）");
                }
            }
        }

        info!(
            sessions = sessions.len(),
            created = total.chunks_created,
            skipped = total.chunks_skipped,
            removed = total.chunks_removed,
            "utt 全量构建完成"
        );
        Ok(total)
    }

    // =========================================================
    // 内部：写入单个块
    // =========================================================

    /// 渲染块文本 → 生成 embedding（含内容级去重）→ 入库。
    ///
    /// 内容级去重（T-V16-5-002）:
    /// - 同一构建周期内，内容未变的块（`block_text` 哈希一致）复用缓存向量，
    ///   避免重复会话/未变块全量重算 embedding。
    /// - 去重关闭、hash 缺失、embedding 不可用时回退逐块重算。
    async fn write_chunk(
        &self,
        storage: &dyn StorageBackend,
        chunk: &UttChunk,
        target: &str,
        embedder: Option<&dyn EmbeddingProvider>,
        stats: &mut UttBuildStats,
    ) -> RamariaResult<()> {
        let target_name = resolve_persona_name(storage, target).await;
        let descriptions = load_attachment_descriptions(storage, &chunk.messages).await;
        let block_text = render_block_text(chunk, target, &target_name, &descriptions);

        let mut block = UttBlock::new(
            target.to_string(),
            chunk.messages[0].session_id,
            chunk.start_msg_id,
            chunk.end_msg_id,
            block_text,
            chunk.msg_count,
            chunk.time_span_ms,
        );

        // embedding 生成（失败降级：块照常入库，仅无向量）
        if let Some(provider) = embedder {
            let content_hash = content_hash(&block.block_text);
            // 内容级去重：命中缓存 → 复用向量，不触发新的 embedding 推理。
            let reused = if self.config.content_dedup {
                let cached = lock_recover(&self.embedding_cache, "utt_builder.embedding_cache")
                    .get(&content_hash)
                    .cloned();
                match cached {
                    Some(vec) => {
                        block.embedding = Some(encode_embedding(&vec));
                        stats.embedding_reused += 1;
                        true
                    }
                    None => false,
                }
            } else {
                false
            };

            if !reused {
                match provider.embed(&block.block_text).await {
                    Ok(vec) => {
                        // 去重开启时写入缓存，供后续同内容块复用（锁内短操作，不跨 await）。
                        if self.config.content_dedup {
                            lock_recover(&self.embedding_cache, "utt_builder.embedding_cache")
                                .insert(content_hash, vec.clone());
                        }
                        block.embedding = Some(encode_embedding(&vec));
                        stats.embedding_ok += 1;
                    }
                    Err(e) => {
                        stats.embedding_failed += 1;
                        warn!(
                            session_id = %block.session_id,
                            start_msg_id = %block.start_msg_id,
                            %e,
                            "utt 块 embedding 生成失败，降级为无向量（不影响入库）"
                        );
                    }
                }
            }
        }

        let id = storage.insert_utt_block(&block).await?;
        stats.chunks_created += 1;
        info!(
            block_id = id,
            session_id = %block.session_id,
            persona_uid = %block.persona_uid,
            msg_count = block.msg_count,
            "utt 话语块已入库"
        );
        Ok(())
    }
}

// =========================================================
// 块文本渲染
// =========================================================

/// 解析 persona 注册名（查询失败回退 uid）。
async fn resolve_persona_name(storage: &dyn StorageBackend, uid: &str) -> String {
    match storage.get_persona_by_uid(uid).await {
        Ok(Some(p)) if !p.name.is_empty() => p.name,
        _ => uid.to_string(),
    }
}

/// 加载消息附件描述映射（`[图片#{hash}]` → 描述；失败按空映射降级）。
///
/// 说明:
/// - 仅取描述已完成的附件（渲染条件由 [`build_render_map`] 统一判定）；
/// - 查询失败记 warn 并返回空映射：块文本保留占位符原文，不阻塞构建。
async fn load_attachment_descriptions(
    storage: &dyn StorageBackend,
    messages: &[Message],
) -> HashMap<String, String> {
    let message_ids: Vec<Uuid> = messages.iter().map(|m| m.id).collect();
    if message_ids.is_empty() {
        return HashMap::new();
    }
    match storage.list_attachments_by_messages(&message_ids).await {
        Ok(rows) => build_render_map(&rows),
        Err(e) => {
            warn!(error = %e, "消息附件查询失败，块文本保留占位符原文");
            HashMap::new()
        }
    }
}

/// 计算块文本的内容哈希（用于内容级去重）。
///
/// 说明:
/// - 用 64 位 FNV-1a 哈希（碰撞概率在去重场景可接受；即使碰撞也只是多算一次 embedding，
///   不影响正确性——向量仍来自同内容文本）。
/// - 不记录原文，仅作为去重键，符合原文隐私约束。
fn content_hash(text: &str) -> u64 {
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in text.as_bytes() {
        hash ^= *byte as u64;
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash
}

/// 渲染消息的发言人标记。
///
/// 规则:
/// - `msg.persona_uid == target_uid` → `target_name`（目标 persona 发言）。
/// - `msg.persona_uid` 为 None（用户消息）→ "用户"。
/// - 其他 uid（跨 persona 防御）→ 直接显示 uid。
fn speaker_label(
    msg: &ramaria_core::types::Message,
    target_uid: &str,
    target_name: &str,
) -> String {
    match msg.persona_uid.as_deref() {
        Some(uid) if uid == target_uid => target_name.to_string(),
        Some(uid) => uid.to_string(),
        None => "用户".to_string(),
    }
}

/// 将时间戳格式化为 `YYYY-MM-DD HH:MM`（本地时区）。
///
/// 说明:
/// - 时间戳非法（超出 chrono 范围）时回退为原始毫秒值，不 panic。
fn format_block_time(created_at_ms: i64) -> String {
    match Local.timestamp_millis_opt(created_at_ms) {
        chrono::LocalResult::Single(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
        _ => created_at_ms.to_string(),
    }
}

/// 渲染块文本：`[时间] 角色: 内容` 行序列（时间升序）。
///
/// 参数:
/// - `chunk`: 切分结果。
/// - `target_uid`: 目标 persona UID。
/// - `target_name`: 目标 persona 注册名（已解析）。
/// - `descriptions`: 附件描述映射（`[图片#{hash}]` → 描述；空映射时正文零变化）。
///
/// 返回:
/// - 多行块文本（供 `UttBlock.block_text` 持久化与【原文片段】注入）。
pub fn render_block_text(
    chunk: &UttChunk,
    target_uid: &str,
    target_name: &str,
    descriptions: &HashMap<String, String>,
) -> String {
    let mut lines = Vec::with_capacity(chunk.messages.len());
    for m in &chunk.messages {
        let speaker = speaker_label(m, target_uid, target_name);
        let time = format_block_time(m.created_at);
        let content = replace_image_placeholders(&m.content, descriptions);
        lines.push(format!("[{time}] {speaker}: {content}"));
    }
    lines.join("\n")
}

// =========================================================
// 单元测试（内存 SQLite，真实 StorageBackend 语义）
// =========================================================

#[cfg(test)]
mod tests;
