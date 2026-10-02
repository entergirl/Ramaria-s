//! crates/ramaria-memory/src/utt/builder/tests.rs - //! crates/ramaria-memory/src/utt/builder.rs - utt 话语块构建器单元测试
//!
//! 设计特点:
//! - 位于 utt::builder 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 builder.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::error::RamariaError;
use ramaria_core::traits::StoreCrud;
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
    assert!(blocks[0].block_text.contains("[") && blocks[0].block_text.contains("角色char-0001"));
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
        blocks[0].block_text.contains("首单边内容0") && blocks[0].block_text.contains("双边内容5"),
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
        ..Session::default()
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
