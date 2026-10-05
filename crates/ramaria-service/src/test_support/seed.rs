//! crates/ramaria-service/src/test_support/seed.rs - Ramaria 服务层测试用造数模块
//!
//! 设计特点:
//! - 造数覆盖 persona / L1 / 消息 / 会话 / utt 块，供各用例测试复用，避免口径漂移；
//! - 时间戳显式传入，便于空闲检查与排序类用例构造边界；
//! - 临时目录按 tag + 纳秒唯一命名，调用方负责清理（`std::fs::remove_dir_all`）；
//! - 造数一律通过真实存储句柄写入，与生产读写路径一致。

use std::path::PathBuf;

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    MemoryL1, Message, MessageRole, MessageSource, Persona, PersonaKind, UttBlock, now_ms,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

// =========================================================
// 临时目录
// =========================================================

/// 创建唯一临时目录（调用方负责清理）。
pub(crate) fn temp_dir(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应可读")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!("ramaria-service-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
    dir
}

// =========================================================
// 造数
// =========================================================

/// 造一个 persona 行（L1 / facts / sessions 的外键依赖），类型固定为 char。
pub(crate) async fn seed_persona(storage: &SqliteStorage, uid: &str) {
    seed_persona_kind(storage, uid, PersonaKind::Char).await;
}

/// 造一个指定类型 persona 行（user / rama 等类型分支的闸门用例使用）。
pub(crate) async fn seed_persona_kind(storage: &SqliteStorage, uid: &str, kind: PersonaKind) {
    let persona = Persona::new(
        uid.to_string(),
        "测试人格".to_string(),
        kind,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("插入 persona 应成功");
}

/// 造一条带 persona 归属的 L1 摘要（自动建会话满足外键）。
pub(crate) async fn seed_l1(
    storage: &SqliteStorage,
    persona: &str,
    summary: &str,
    keywords: Option<&str>,
    created_at: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    let mut l1 = MemoryL1::new(session.id, summary.to_string(), None);
    l1.persona_uid = Some(persona.to_string());
    l1.keywords = keywords.map(str::to_string);
    l1.created_at = created_at;
    storage.save_memory_l1(&l1).await.expect("写入 L1 应成功");
    l1.id
}

/// 造 N 条会话消息（角色按 user / assistant 交替，created_at 自 base 递增）。
pub(crate) async fn seed_messages(
    storage: &SqliteStorage,
    session_id: Uuid,
    persona: &str,
    count: usize,
    base_ts: i64,
) {
    for i in 0..count {
        let role = if i % 2 == 0 {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        let mut message = Message::new(
            session_id,
            role,
            format!("消息内容 {i}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.to_string()));
        message.created_at = base_ts + i as i64;
        storage
            .save_message(&message)
            .await
            .expect("写入消息应成功");
    }
}

/// 造一个带消息的活跃会话（供封存 / 空闲检查用例）。
pub(crate) async fn seed_session_with_messages(
    storage: &SqliteStorage,
    persona: &str,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    session.id
}

/// 造一段足够久远的对话历史（含本地用户消息，消息时间 45 天前）。
///
/// 用途:
/// - 让"自动"开关的人格通过解锁判定（存在本地用户消息）；
/// - 时间取活跃时段 30 天统计窗口之外且早于退避 / 空闲判定窗口，不干扰既有用例语义。
pub(crate) async fn seed_dialogue_history(storage: &SqliteStorage, persona: &str) {
    seed_session_with_messages(storage, persona, 2, now_ms() - 45 * 86_400_000).await;
}

/// 造一个带通道标识的活跃会话 + 消息（供"外部对话超时另起"等回流用例）。
pub(crate) async fn seed_channel_session(
    storage: &SqliteStorage,
    persona: &str,
    channel: &str,
    external_ref: Option<&str>,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = storage
        .create_session_in_channel(Some(persona), channel, external_ref)
        .await
        .expect("创建通道会话应成功");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    session.id
}

/// 造一个带消息的已关闭会话（供桥接与封存后读取用例）。
pub(crate) async fn seed_closed_session_with_messages(
    storage: &SqliteStorage,
    persona: &str,
    count: usize,
    base_ts: i64,
) -> Uuid {
    let session = storage
        .create_session(Some(persona))
        .await
        .expect("创建会话应成功");
    seed_messages(storage, session.id, persona, count, base_ts).await;
    storage
        .close_session(session.id)
        .await
        .expect("关闭会话应成功");
    session.id
}

/// 造一条 utt 话语块（供桥接一级来源 / 原文通道用例）。
///
/// 前置条件:
/// - 目标会话已有消息（块的消息区间外键指向真实消息）。
///
/// 返回:
/// - 落库后的块 id。
pub(crate) async fn seed_utt_block(
    storage: &SqliteStorage,
    session_id: Uuid,
    persona: &str,
    block_text: &str,
) -> i64 {
    let messages = storage
        .list_messages(session_id)
        .await
        .expect("读取会话消息应成功");
    let start_msg_id = messages
        .first()
        .map(|message| message.id)
        .expect("utt 块来源会话应已有消息");
    let end_msg_id = messages
        .last()
        .map(|message| message.id)
        .expect("utt 块来源会话应已有消息");
    let block = UttBlock::new(
        persona.to_string(),
        session_id,
        start_msg_id,
        end_msg_id,
        block_text.to_string(),
        messages.len().max(1) as u32,
        60_000,
    );
    storage
        .insert_utt_block(&block)
        .await
        .expect("插入 utt 块应成功")
}
