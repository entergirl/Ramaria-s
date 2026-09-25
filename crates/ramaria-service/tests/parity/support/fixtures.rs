//! crates/ramaria-service/tests/parity/support/fixtures.rs - 对照场景固定口径造数
//!
//! 设计特点:
//! - 四路径共用：persona / 会话 / 消息 / L1 的造数集中在此，避免各路径各写一套导致口径漂移
//! - 时间可控：fixture 时间取"当前时间 - 固定偏移"，保证衰减与时间戳口径在跨运行、跨进程下稳定
//!   （不写入随机时间、不依赖系统时钟绝对值）
//! - 返回可断言：造数函数返回关键标识（会话 id / L1 id），调用方直接用于用例与结果读取
//! - 只走公开存储 API：不做 SQL 直插，与生产写入路径保持一致语义
//! - 失败即环境错误：任一步失败按 `ParityError::Env` 上报，附步骤说明

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{
    MemoryL1, Message, MessageRole, MessageSource, Persona, PersonaKind, now_ms,
};
use ramaria_storage::SqliteStorage;
use uuid::Uuid;

use super::error::{ParityError, ParityResult};

/// fixture 时间基准偏移：当前时间往前 1 小时。
///
/// 说明:
/// - 用于让衰减分数脱离"绝对时间"：fixture 每次运行都以当前时间为基准回溯固定间隔，
///   因此同输入在同一实现上的分数在跨运行之间保持稳定（对照与基线冻结的前提）。
pub const FIXTURE_AGE_MS: i64 = 3_600_000;

/// 计算 fixture 时间戳：当前时间 - 固定偏移 + 附加偏移。
///
/// 参数:
/// - `offset_ms`: 在基准时间之上的附加偏移（用于构造"更早 / 更晚"的多条记忆）。
pub fn fixture_ts(offset_ms: i64) -> i64 {
    now_ms() - FIXTURE_AGE_MS + offset_ms
}

/// 造一个 persona 行（会话 / L1 / 事实的外键依赖）。
///
/// 说明:
/// - 幂等：uid 已存在时直接返回，支持在同一环境内重复执行场景（重复造数不报唯一约束错误）。
pub async fn seed_persona(storage: &SqliteStorage, uid: &str) -> ParityResult<()> {
    let existing = storage
        .get_persona_by_uid(uid)
        .await
        .map_err(|e| ParityError::env(format!("查询 persona {uid}"), e))?;
    if existing.is_some() {
        return Ok(());
    }

    let persona = Persona::new(
        uid.to_string(),
        "对照测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .map_err(|e| ParityError::env(format!("种子写入 persona {uid}"), e))?;
    Ok(())
}

/// 造一个带消息的活跃会话（本地通道）。
///
/// 参数:
/// - `persona`: 会话归属人格。
/// - `message_count`: 消息条数（角色按 user / assistant 交替）。
/// - `base_ts`: 首条消息时间（Unix 毫秒），后续消息按毫秒递增。
pub async fn seed_active_session(
    storage: &SqliteStorage,
    persona: &str,
    message_count: usize,
    base_ts: i64,
) -> ParityResult<Uuid> {
    let session = storage
        .create_session(Some(persona))
        .await
        .map_err(|e| ParityError::env("创建对照测试会话", e))?;
    for index in 0..message_count {
        let role = if index % 2 == 0 {
            MessageRole::User
        } else {
            MessageRole::Assistant
        };
        let mut message = Message::new(
            session.id,
            role,
            format!("对照消息 {index}"),
            MessageSource::Local,
        )
        .with_persona_uid(Some(persona.to_string()));
        message.created_at = base_ts + index as i64;
        storage
            .save_message(&message)
            .await
            .map_err(|e| ParityError::env(format!("写入对照消息 {index}"), e))?;
    }
    Ok(session.id)
}

/// 造一条带 persona 归属的 L1 摘要（自动建会话满足外键）。
///
/// 参数:
/// - `persona`: L1 归属人格。
/// - `summary`: 摘要文本（检索匹配的主要依据）。
/// - `keywords`: 逗号分隔关键词（关键词镜像与 BM25 词典输入）。
/// - `created_at`: 创建时间（Unix 毫秒；用 [`fixture_ts`] 构造）。
pub async fn seed_l1(
    storage: &SqliteStorage,
    persona: &str,
    summary: &str,
    keywords: Option<&str>,
    created_at: i64,
) -> ParityResult<Uuid> {
    let session = storage
        .create_session(Some(persona))
        .await
        .map_err(|e| ParityError::env("创建 L1 所属会话", e))?;
    let mut l1 = MemoryL1::new(session.id, summary.to_string(), None);
    l1.persona_uid = Some(persona.to_string());
    l1.keywords = keywords.map(str::to_string);
    l1.created_at = created_at;
    storage
        .save_memory_l1(&l1)
        .await
        .map_err(|e| ParityError::env("写入对照 L1", e))?;
    Ok(l1.id)
}
