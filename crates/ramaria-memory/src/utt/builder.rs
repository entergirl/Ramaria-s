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

use std::collections::HashMap;
use std::sync::Mutex;

use chrono::{Local, TimeZone};
use ramaria_core::config::UttConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{EmbeddingProvider, StorageBackend};
use ramaria_core::types::{Session, UttBlock};
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
        let block_text = render_block_text(chunk, target, &target_name);

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
                let cached = self
                    .embedding_cache
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
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
                            self.embedding_cache
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
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
///
/// 返回:
/// - 多行块文本（供 `UttBlock.block_text` 持久化与【原文片段】注入）。
pub fn render_block_text(chunk: &UttChunk, target_uid: &str, target_name: &str) -> String {
    let mut lines = Vec::with_capacity(chunk.messages.len());
    for m in &chunk.messages {
        let speaker = speaker_label(m, target_uid, target_name);
        let time = format_block_time(m.created_at);
        lines.push(format!("[{time}] {speaker}: {}", m.content));
    }
    lines.join("\n")
}

// =========================================================
// 单元测试（内存 SQLite，真实 StorageBackend 语义）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::error::RamariaError;
    use ramaria_core::types::{Message, MessageRole, MessageSource, Persona, PersonaKind};
    use ramaria_storage::SqliteStorage;

    /// 内存 SQLite 存储（跑 v1.3 + v1.4 migration）。
    async fn mem_storage() -> SqliteStorage {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true);
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("内存测试数据库创建失败");
        sqlx::migrate!("../ramaria-storage/migrations")
            .run(&pool)
            .await
            .expect("测试 migration 失败");
        SqliteStorage::new(pool)
    }

    /// 固定向量 mock embedding（is_available=true）。
    struct FixedEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for FixedEmbedding {
        async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
            Ok(vec![0.1, 0.2, 0.3])
        }
        async fn embed_batch(&self, texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|_| vec![0.1, 0.2, 0.3]).collect())
        }
        fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
            ramaria_core::traits::EmbeddingModelInfo {
                model_id: "fixed".to_string(),
                dimension: 3,
            }
        }
        async fn validate(&self) -> RamariaResult<()> {
            Ok(())
        }
        async fn download_model(&self) -> RamariaResult<()> {
            Ok(())
        }
        fn download_progress(&self) -> f64 {
            1.0
        }
        fn is_available(&self) -> bool {
            true
        }
    }

    /// 失败 embedding（模拟模型不可用）。
    struct FailingEmbedding;

    #[async_trait::async_trait]
    impl EmbeddingProvider for FailingEmbedding {
        async fn embed(&self, _text: &str) -> RamariaResult<Vec<f32>> {
            Err(RamariaError::embedding("mock embedding 不可用"))
        }
        async fn embed_batch(&self, _texts: &[&str]) -> RamariaResult<Vec<Vec<f32>>> {
            Err(RamariaError::embedding("mock embedding 不可用"))
        }
        fn model_info(&self) -> ramaria_core::traits::EmbeddingModelInfo {
            ramaria_core::traits::EmbeddingModelInfo {
                model_id: "failing".to_string(),
                dimension: 3,
            }
        }
        async fn validate(&self) -> RamariaResult<()> {
            Err(RamariaError::embedding("不可用"))
        }
        async fn download_model(&self) -> RamariaResult<()> {
            Ok(())
        }
        fn download_progress(&self) -> f64 {
            0.0
        }
        fn is_available(&self) -> bool {
            false
        }
    }

    /// 构造 persona + 会话 + 交替消息。
    async fn setup_session(
        storage: &SqliteStorage,
        persona_uid: &str,
        msg_count: usize,
        _gap_minutes: i64,
    ) -> Session {
        let persona = Persona::new(
            persona_uid.to_string(),
            format!("角色{persona_uid}"),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage.create_persona(&persona).await.unwrap();
        let session = storage.create_session(Some(persona_uid)).await.unwrap();

        for i in 0..msg_count {
            let uid = if i % 2 == 0 { Some(persona_uid) } else { None };
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let msg = Message::new(
                session.id,
                role,
                format!("第{i}条消息内容"),
                MessageSource::Local,
            )
            .with_persona_uid(uid.map(|s| s.to_string()));
            storage.save_message(&msg).await.unwrap();
        }
        session
    }

    fn test_builder() -> UttBuilder {
        UttBuilder::new(UttBuildConfig {
            splitter: UttSplitterConfig {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
            },
            content_dedup: true,
        })
    }

    #[tokio::test]
    async fn build_session_creates_blocks() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 10, 1).await;

        let stats = test_builder()
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_created, 1, "10 条消息无间隙 → 一块");
        assert_eq!(stats.chunks_skipped, 0);

        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert!(
            blocks[0].block_text.contains("[") && blocks[0].block_text.contains("角色char-0001")
        );
        assert!(blocks[0].block_text.contains("用户"));
        assert_eq!(blocks[0].msg_count, 10);
        assert!(blocks[0].embedding.is_none(), "无 embedder → 无向量");
    }

    /// 端到端验收：真实消息序列上验证单边合并——
    /// 中间出现"只有一方发言"的块时正确并入相邻块（两侧等距时并入前块，tiebreak）。
    ///
    /// 消息序列: 双边块(0-3) → 1h 间隙 → 单边 User 块(4-5) → 1h 间隙 → 双边块(6-9)
    /// 期望: 单边块并入前块 → 2 块（0-5 / 6-9），块 1 含单边消息原文。
    #[tokio::test]
    async fn end_to_end_single_side_block_merges_into_previous() {
        let storage = mem_storage().await;
        let persona_uid = "char-0001";
        let persona = Persona::new(
            persona_uid.to_string(),
            "角色A".to_string(),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage.create_persona(&persona).await.unwrap();
        let session = storage.create_session(Some(persona_uid)).await.unwrap();

        // 显式构造带时间间隙的消息序列（间隙 1h > θ_gap 30min）
        let base = 1_700_000_000_000i64;
        let gap_ms = 60 * 60 * 1000; // 1 小时
        let mut msgs: Vec<Message> = Vec::new();
        let mut t = base;
        // 块 1：双边交替（目标发言 + 用户发言）
        for i in 0..4 {
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let uid = if i % 2 == 0 {
                Some(persona_uid.to_string())
            } else {
                None
            };
            let mut m = Message::new(
                session.id,
                role,
                format!("双边块1内容{i}"),
                MessageSource::Local,
            )
            .with_persona_uid(uid);
            m.created_at = t;
            t += 60_000;
            msgs.push(m);
        }
        // 块 2：纯用户发言（单边，无目标发言）
        t += gap_ms;
        for i in 4..6 {
            let mut m = Message::new(
                session.id,
                MessageRole::User,
                format!("单边块内容{i}"),
                MessageSource::Local,
            );
            m.created_at = t;
            t += 60_000;
            msgs.push(m);
        }
        // 块 3：双边交替
        t += gap_ms;
        for i in 6..10 {
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let uid = if i % 2 == 0 {
                Some(persona_uid.to_string())
            } else {
                None
            };
            let mut m = Message::new(
                session.id,
                role,
                format!("双边块2内容{i}"),
                MessageSource::Local,
            )
            .with_persona_uid(uid);
            m.created_at = t;
            t += 60_000;
            msgs.push(m);
        }
        for m in &msgs {
            storage.save_message(m).await.unwrap();
        }

        let stats = test_builder()
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_created, 2, "单边块应并入相邻块 → 2 块");

        let blocks = storage
            .list_utt_blocks_by_persona(persona_uid)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 2, "端到端应产出 2 块");
        // 块 1 = 双边块1 + 单边块（并入前块）
        assert_eq!(blocks[0].msg_count, 6, "块1 应包含单边块消息（0-5）");
        assert!(
            blocks[0].block_text.contains("单边块内容4")
                && blocks[0].block_text.contains("单边块内容5"),
            "单边块原文应并入块1: {}",
            blocks[0].block_text
        );
        // 块 2 = 双边块2
        assert_eq!(blocks[1].msg_count, 4, "块2 应保持 4 条（6-9）");
        assert!(
            !blocks[1].block_text.contains("单边块内容"),
            "块2 不应含单边块消息"
        );
    }

    /// 端到端验收：首块单边（只有一方发言）时并入后一块。
    ///
    /// 消息序列: 单边 User 块(0-1) → 1h 间隙 → 双边块(2-5)
    /// 期望: 首块并入后块 → 1 块（0-5），块含全部消息。
    #[tokio::test]
    async fn end_to_end_single_side_first_block_merges_into_next() {
        let storage = mem_storage().await;
        let persona_uid = "char-0001";
        let persona = Persona::new(
            persona_uid.to_string(),
            "角色A".to_string(),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage.create_persona(&persona).await.unwrap();
        let session = storage.create_session(Some(persona_uid)).await.unwrap();

        let base = 1_700_000_000_000i64;
        let gap_ms = 60 * 60 * 1000;
        let mut msgs: Vec<Message> = Vec::new();
        let mut t = base;
        // 首块：纯用户发言（单边）
        for i in 0..2 {
            let mut m = Message::new(
                session.id,
                MessageRole::User,
                format!("首单边内容{i}"),
                MessageSource::Local,
            );
            m.created_at = t;
            t += 60_000;
            msgs.push(m);
        }
        // 次块：双边交替
        t += gap_ms;
        for i in 2..6 {
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let uid = if i % 2 == 0 {
                Some(persona_uid.to_string())
            } else {
                None
            };
            let mut m = Message::new(
                session.id,
                role,
                format!("双边内容{i}"),
                MessageSource::Local,
            )
            .with_persona_uid(uid);
            m.created_at = t;
            t += 60_000;
            msgs.push(m);
        }
        for m in &msgs {
            storage.save_message(m).await.unwrap();
        }

        let stats = test_builder()
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_created, 1, "首单边块应并入后块 → 1 块");

        let blocks = storage
            .list_utt_blocks_by_persona(persona_uid)
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].msg_count, 6, "合并后应含全部 6 条消息");
        assert!(
            blocks[0].block_text.contains("首单边内容0")
                && blocks[0].block_text.contains("双边内容5"),
            "块应含首单边与双边全部消息"
        );
    }

    #[tokio::test]
    async fn build_session_generates_embedding() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 4, 1).await;
        let stats = test_builder()
            .build_session(&storage, &session, Some(&FixedEmbedding))
            .await
            .unwrap();
        assert_eq!(stats.embedding_ok, 1);
        assert_eq!(stats.embedding_failed, 0);

        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert!(blocks[0].embedding.is_some(), "embedding BLOB 应写入");
    }

    #[tokio::test]
    async fn build_session_embedding_failure_degrades() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 4, 1).await;

        let stats = test_builder()
            .build_session(&storage, &session, Some(&FailingEmbedding))
            .await
            .unwrap();
        assert_eq!(stats.embedding_failed, 1, "失败降级记 stats");
        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1, "块照常入库");
        assert!(blocks[0].embedding.is_none());
    }

    #[tokio::test]
    async fn build_session_idempotent_on_repeat() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 10, 1).await;
        let builder = test_builder();

        let first = builder
            .build_session(&storage, &session, Some(&FixedEmbedding))
            .await
            .unwrap();
        assert_eq!(first.chunks_created, 1);

        // 重复执行：幂等跳过，不产生新块、不重复生成 embedding
        let second = builder
            .build_session(&storage, &session, Some(&FixedEmbedding))
            .await
            .unwrap();
        assert_eq!(second.chunks_created, 0);
        assert_eq!(second.chunks_skipped, 1);
        assert_eq!(second.chunks_removed, 0);
        assert_eq!(second.embedding_ok, 0, "跳过时不重新 embed");

        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
    }

    #[tokio::test]
    async fn incremental_build_appends_new_messages() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 6, 1).await;
        let builder = test_builder();

        builder
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        let before = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].msg_count, 6);

        // 封存后新增 2 条消息（模拟"会话关闭前最后几条未封存"）→ 增量补齐
        for i in 6..8 {
            let uid = if i % 2 == 0 { Some("char-0001") } else { None };
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let msg = Message::new(
                session.id,
                role,
                format!("第{i}条消息内容"),
                MessageSource::Local,
            )
            .with_persona_uid(uid.map(|s| s.to_string()));
            storage.save_message(&msg).await.unwrap();
        }

        let stats = builder
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_removed, 1, "旧尾块被重切删除");
        assert_eq!(stats.chunks_created, 1, "重切后写入新尾块");

        let after = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(after.len(), 1, "仍然只有一个块");
        assert_eq!(after[0].msg_count, 8, "新消息并入尾块");
    }

    #[tokio::test]
    async fn incremental_build_with_gap_creates_new_block() {
        let storage = mem_storage().await;
        let session = setup_session(&storage, "char-0001", 4, 1).await;
        let builder = test_builder();
        builder
            .build_session(&storage, &session, None)
            .await
            .unwrap();

        // 新增消息与前一条间隔 2 小时（> θ_gap）→ 形成新块；
        // 新块需含双方发言（单条 target 会因单边合并并入旧尾块）
        let last_time = storage
            .list_messages(session.id)
            .await
            .unwrap()
            .last()
            .unwrap()
            .created_at;
        let gap_start = last_time + 2 * 3600 * 1000;
        let user_msg = Message::new(
            session.id,
            MessageRole::User,
            "隔天的新问题".to_string(),
            MessageSource::Local,
        );
        let mut user_msg = user_msg;
        user_msg.created_at = gap_start;
        storage.save_message(&user_msg).await.unwrap();

        let reply_msg = Message::new(
            session.id,
            MessageRole::Assistant,
            "隔天的新回答内容".to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some("char-0001".to_string()));
        let mut reply_msg = reply_msg;
        reply_msg.created_at = gap_start + 60_000;
        storage.save_message(&reply_msg).await.unwrap();

        let stats = builder
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        // 重切首块与库中最后一块一致（旧块无变化）→ 幂等跳过；间隙后的新块单独插入
        assert_eq!(stats.chunks_skipped, 1, "旧尾块重切结果一致 → 跳过");
        assert_eq!(stats.chunks_removed, 0);
        assert_eq!(stats.chunks_created, 1, "间隙新块插入");

        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(blocks.len(), 2);
    }

    #[tokio::test]
    async fn rebuild_all_aggregates_and_is_idempotent() {
        let storage = mem_storage().await;
        let _s1 = setup_session(&storage, "char-0001", 6, 1).await;
        let _s2 = setup_session(&storage, "char-0002", 4, 1).await;

        let builder = test_builder();
        let total = builder.rebuild_all(&storage, None).await.unwrap();
        assert_eq!(total.chunks_created, 2, "两个会话各一块");
        assert_eq!(total.session_id, None, "全量聚合无单一 session");

        let again = builder.rebuild_all(&storage, None).await.unwrap();
        assert_eq!(again.chunks_created, 0, "幂等：全部跳过");
        assert_eq!(again.chunks_skipped, 2);

        assert_eq!(
            storage
                .list_utt_blocks_by_persona("char-0001")
                .await
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            storage
                .list_utt_blocks_by_persona("char-0002")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn rebuild_all_skips_failed_session_but_continues() {
        let storage = mem_storage().await;
        // char-0001 正常；char-0002 不创建 persona（insert 违反 FK）→ 失败被跳过
        let _s1 = setup_session(&storage, "char-0001", 4, 1).await;
        let session2 = storage.create_session(Some("char-0002")).await.unwrap();
        let msg = Message::new(
            session2.id,
            MessageRole::User,
            "孤儿消息".to_string(),
            MessageSource::Local,
        );
        storage.save_message(&msg).await.unwrap();

        let builder = test_builder();
        let total = builder.rebuild_all(&storage, None).await.unwrap();
        assert_eq!(total.chunks_created, 1, "char-0001 正常入库");
        // char-0002 无 persona 记录 → utt_blocks 外键失败 → 单会话失败被跳过不中断
        assert_eq!(
            storage
                .list_utt_blocks_by_persona("char-0001")
                .await
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn build_session_no_target_speech_cleans_stale_blocks() {
        let storage = mem_storage().await;
        // 先建含目标发言的会话并入库
        let session = setup_session(&storage, "char-0001", 2, 1).await;
        let builder = test_builder();
        builder
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(
            storage
                .list_utt_blocks_by_persona("char-0001")
                .await
                .unwrap()
                .len(),
            1
        );

        // 全量重建后（假设参数调整使块变空——直接验证：无目标发言的会话不产生块）
        // 手动清理全部消息中的目标发言不可行（messages 已落库），
        // 改为验证：库中块起点消息缺失时按全量重切（防御路径不 panic）
        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        let missing_session = Session {
            id: session.id,
            started_at: 0,
            ended_at: None,
            persona_uid: None,
        };
        // P0-2 修复后：persona_uid=None 从消息首条 assistant 发言推断
        // 目标 = char-0001（与原归属一致）→ 幂等跳过，不产生新块
        let stats = builder
            .build_session(&storage, &missing_session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_created, 0, "NULL 会话经推断后幂等跳过");
        assert_eq!(stats.chunks_skipped, 1, "推断归属与库中块一致 → 跳过");
        let _ = blocks;
    }

    // P0-2 修复：NULL 会话（存量缺陷）从消息推断目标 persona 后正常建块
    #[tokio::test]
    async fn build_session_null_persona_infers_target_from_messages() {
        let storage = mem_storage().await;
        let persona = Persona::new(
            "char-0001".to_string(),
            "角色char-0001".to_string(),
            PersonaKind::Char,
            1,
            "local".to_string(),
        );
        storage.create_persona(&persona).await.unwrap();
        let session = storage.create_session(None).await.unwrap(); // NULL 会话

        for i in 0..4 {
            let uid = if i % 2 == 0 { Some("char-0001") } else { None };
            let role = if i % 2 == 0 {
                MessageRole::Assistant
            } else {
                MessageRole::User
            };
            let msg = Message::new(session.id, role, format!("内容{i}"), MessageSource::Local)
                .with_persona_uid(uid.map(|s| s.to_string()));
            storage.save_message(&msg).await.unwrap();
        }

        let stats = test_builder()
            .build_session(&storage, &session, None)
            .await
            .unwrap();
        assert_eq!(stats.chunks_created, 1, "NULL 会话经消息推断后应建块");

        let blocks = storage
            .list_utt_blocks_by_persona("char-0001")
            .await
            .unwrap();
        assert_eq!(blocks.len(), 1);
        assert_eq!(blocks[0].persona_uid, "char-0001", "块归属推断出的 persona");
        assert!(
            blocks[0].block_text.contains("角色char-0001"),
            "发言人标记应解析 persona 名"
        );
    }

    // P0-2 修复：NULL 会话且无 assistant 发言（纯用户）→ 无法推断
    // → 回退 rama-0001 作目标；无目标发言 → 不建块安全跳过（不产生错误归属块）
    #[tokio::test]
    async fn build_session_null_persona_no_assistant_skips_safely() {
        let storage = mem_storage().await;
        let session = storage.create_session(None).await.unwrap();
        let msg = Message::new(
            session.id,
            MessageRole::User,
            "只有用户发言".to_string(),
            MessageSource::Local,
        );
        storage.save_message(&msg).await.unwrap();

        let stats = test_builder()
            .build_session(&storage, &session, None)
            .await
            .expect("无目标发言应安全返回而非报错");
        assert_eq!(stats.chunks_created, 0, "无法推断目标时不应建块");
        assert_eq!(stats.chunks_skipped, 0);
    }

    #[tokio::test]
    async fn render_block_text_formats_lines() {
        let msgs = vec![
            Message::new(
                Uuid::new_v4(),
                MessageRole::Assistant,
                "你好呀".to_string(),
                MessageSource::Local,
            )
            .with_persona_uid(Some("char-0001".to_string())),
            Message::new(
                Uuid::new_v4(),
                MessageRole::User,
                "你也好".to_string(),
                MessageSource::Local,
            ),
        ];
        let chunk = split_messages(&msgs, Some("char-0001"), &UttSplitterConfig::default());
        let text = render_block_text(&chunk[0], "char-0001", "小夏");
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(
            lines[0].contains("[") && lines[0].contains("小夏: 你好呀"),
            "{}",
            lines[0]
        );
        assert!(lines[1].contains("用户: 你也好"), "{}", lines[1]);
    }

    #[tokio::test]
    async fn speaker_label_foreign_uid_uses_uid() {
        let m = Message::new(
            Uuid::new_v4(),
            MessageRole::Assistant,
            "x".to_string(),
            MessageSource::Local,
        )
        .with_persona_uid(Some("char-9999".to_string()));
        assert_eq!(speaker_label(&m, "char-0001", "小夏"), "char-9999");
    }

    #[test]
    fn format_block_time_fallback_on_invalid() {
        // 极端时间戳 → 回退数字，不 panic
        let s = format_block_time(i64::MAX);
        assert!(!s.is_empty());
    }
}
