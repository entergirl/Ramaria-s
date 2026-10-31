//! crates/ramaria-eval/src/baseline/build/stage.rs - 基线建库的样本灌入阶段
//!
//! 设计特点:
//! - 人格在灌入前一次性建齐，避免级联时"对话另一方"随人格计数变化而漂移
//! - 逐日会话即一个记忆分组：写入即校验、封存即核对 L1、随后立刻级联
//! - 证据映射按语料消息顺序与会话回读消息逐条对齐，供 qrels 翻译成消息 id
//! - 每个样本灌完后核对未吸收 L1 余量，防止 L2 未消费就继续灌下一天
//! - 只调用服务层用例，不复制 L1 / L2 / L3 算法

use std::collections::HashMap;
use std::path::Path;

use chrono::{NaiveDate, NaiveTime};
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_service::{
    CascadeUntil, Engine, HistoricalIngestRequest, L1BrowseRequest, PersonaFileAction,
    PersonaLoadMode, SessionBrowseRequest, SessionMessagesRequest, TimedTurn,
};
use uuid::Uuid;

use crate::baseline::tier::TierSpec;
use crate::corpus::{Sample, Session};

use super::{CHANNEL, CONVERSATION_PREFIX, PersonaRecord, SampleStats, UNASSORBED_L1_LIMIT};

// =========================================================
// 人格
// =========================================================

/// 为本次建库的全部样本预建人格（写入文件并导入库）。
pub(super) async fn create_personas(
    engine: &Engine,
    selected: &[Sample],
    dir: &Path,
) -> RamariaResult<Vec<PersonaRecord>> {
    std::fs::create_dir_all(dir).map_err(|error| {
        RamariaError::io(format!("创建人格目录失败: {}", dir.display()), Some(error))
    })?;

    let mut records = Vec::with_capacity(selected.len());
    for sample in selected {
        let uid = sample.persona_uid();
        ensure_safe_uid(&uid, &sample.sample_id)?;
        let path = dir.join(format!("{uid}.toml"));
        write_persona_file(&path, &sample.speaker_b)?;
        let outcome = engine
            .persona_load_file(
                &path,
                &uid,
                &sample.speaker_b,
                PersonaLoadMode::CreateMissing,
            )
            .await;
        if outcome.action == PersonaFileAction::Failed {
            return Err(RamariaError::validation(format!(
                "人格导入失败: {uid}（{}）",
                outcome.message
            )));
        }
        records.push(PersonaRecord { uid, source: path });
    }
    Ok(records)
}

/// 人格 uid 只允许 ASCII 字母数字、连字符与下划线，避免拼出非法文件名。
fn ensure_safe_uid(uid: &str, sample_id: &str) -> RamariaResult<()> {
    let safe = !uid.is_empty()
        && uid
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_');
    if safe {
        Ok(())
    } else {
        Err(RamariaError::validation(format!(
            "样本 {sample_id} 的 persona uid 非法（只允许 ASCII 字母数字与连字符 / 下划线）: {uid}"
        )))
    }
}

/// 写人格文件（只写 `assistant_name`）。
fn write_persona_file(path: &Path, assistant_name: &str) -> RamariaResult<()> {
    let mut table = toml::Table::new();
    table.insert(
        "assistant_name".to_string(),
        toml::Value::String(assistant_name.to_string()),
    );
    let text = toml::to_string(&table).map_err(|error| RamariaError::Config {
        context: format!("序列化人格文件失败: {}", path.display()),
        source: Some(Box::new(error)),
        trace_id: None,
    })?;
    std::fs::write(path, text).map_err(|error| {
        RamariaError::io(format!("写入人格文件失败: {}", path.display()), Some(error))
    })
}

// =========================================================
// 样本灌入
// =========================================================

/// 读取人格下已有会话：外部标识 → 会话 id 与消息条数。
async fn existing_sessions(
    engine: &Engine,
    uid: &str,
) -> RamariaResult<HashMap<String, (Uuid, u32)>> {
    let page = engine
        .session_list(SessionBrowseRequest {
            limit: None,
            offset: None,
        })
        .await?;
    Ok(page
        .items
        .into_iter()
        .filter(|session| session.persona_uid.as_deref() == Some(uid))
        .filter_map(|session| {
            session
                .external_ref
                .map(|reference| (reference, (session.id, session.message_count)))
        })
        .collect())
}

/// 灌入单个样本的窗口内会话。
pub(super) async fn ingest_sample(
    engine: &Engine,
    sample: &Sample,
    uid: &str,
    window: &TierSpec,
) -> RamariaResult<SampleStats> {
    let mut stats = SampleStats::default();
    let mut sessions: Vec<&Session> = sample
        .sessions
        .iter()
        .filter(|session| window.includes_date(session.date))
        .collect();
    sessions.sort_by_key(|session| session.date);
    let existing = existing_sessions(engine, uid).await?;

    for session in sessions {
        let date = session.date.format("%Y-%m-%d").to_string();
        let reference = format!("{CONVERSATION_PREFIX}:{}:{date}", sample.user_key);
        if let Some((session_id, message_count)) = existing.get(&reference) {
            if *message_count as usize != session.messages.len() {
                return Err(RamariaError::validation(format!(
                    "{date}: 已存在会话消息数不符（期望 {}，库内 {message_count}）",
                    session.messages.len()
                )));
            }
            let l1_count = engine.memory_l1_by_session(*session_id).await?.len();
            if l1_count > 0 {
                stats.l1 += l1_count;
                let page = engine
                    .session_messages(SessionMessagesRequest {
                        session_id: *session_id,
                        limit: None,
                        offset: None,
                    })
                    .await?;
                if page.total as usize != session.messages.len()
                    || page.messages.len() != session.messages.len()
                {
                    return Err(RamariaError::validation(format!(
                        "{date}: 已存在会话消息数不符（期望 {}，总数 {}，返回 {}）",
                        session.messages.len(),
                        page.total,
                        page.messages.len()
                    )));
                }
                for (source, stored) in session.messages.iter().zip(page.messages.iter()) {
                    stats
                        .artifacts
                        .entry(source.artifact_id())
                        .or_default()
                        .push(stored.id.to_string());
                }
                let cascade = engine.cascade(uid, CascadeUntil::L3).await?;
                stats.consumed += cascade.l1_consumed;
                stats.events += cascade.l2_events;
                stats.traits += cascade.l3_traits;
                stats.sessions += 1;
                stats.messages += session.messages.len();
                eprintln!(
                    "  {uid} {date}: 已存在，跳过写入（L1 {l1_count} / L1 消费 {} / L2 +{} / L3 +{}）",
                    cascade.l1_consumed, cascade.l2_events, cascade.l3_traits
                );
                continue;
            }
            // 会话已关闭但没有 L1：上次封存失败留下的半成品，删除后按正常路径重灌。
            eprintln!("  {uid} {date}: 已存在会话缺少 L1，删除后重新灌入");
            engine.delete_session_cascade(*session_id).await?;
        }
        let day_start = day_start_ms(session.date);
        let turns: Vec<TimedTurn> = session
            .messages
            .iter()
            .enumerate()
            .map(|(index, message)| TimedTurn {
                role: message.role,
                content: message.text.clone(),
                created_at: day_start + index as i64,
            })
            .collect();

        let request = HistoricalIngestRequest {
            messages: turns,
            persona: Some(uid.to_string()),
            conversation_id: Some(format!("{CONVERSATION_PREFIX}:{}:{date}", sample.user_key)),
            channel: CHANNEL.to_string(),
        };
        let ingest = engine.ingest_historical(request).await?;
        if ingest.written != session.messages.len() || ingest.deduplicated != 0 {
            return Err(RamariaError::validation(format!(
                "{date}: 写入条数不符（期望 {}，写入 {}，去重 {}）",
                session.messages.len(),
                ingest.written,
                ingest.deduplicated
            )));
        }

        let seal = engine.seal(ingest.session_id).await?;
        if !seal.sealed || seal.l1_count == 0 {
            return Err(RamariaError::validation(format!(
                "{date}: 封存未完成（sealed={}，L1={}）",
                seal.sealed, seal.l1_count
            )));
        }
        stats.l1 += engine.memory_l1_by_session(ingest.session_id).await?.len();

        let page = engine
            .session_messages(SessionMessagesRequest {
                session_id: ingest.session_id,
                limit: None,
                offset: None,
            })
            .await?;
        if page.total as usize != session.messages.len()
            || page.messages.len() != session.messages.len()
        {
            return Err(RamariaError::validation(format!(
                "{date}: 会话消息数不符（期望 {}，总数 {}，返回 {}）",
                session.messages.len(),
                page.total,
                page.messages.len()
            )));
        }
        for (source, stored) in session.messages.iter().zip(page.messages.iter()) {
            stats
                .artifacts
                .entry(source.artifact_id())
                .or_default()
                .push(stored.id.to_string());
        }

        let cascade = engine.cascade(uid, CascadeUntil::L3).await?;
        stats.consumed += cascade.l1_consumed;
        stats.events += cascade.l2_events;
        stats.traits += cascade.l3_traits;
        stats.sessions += 1;
        stats.messages += session.messages.len();

        eprintln!(
            "  {uid} {date}: 消息 {} / L1 {} / L1 消费 {} / L2 +{} / L3 +{}",
            session.messages.len(),
            seal.l1_count,
            cascade.l1_consumed,
            cascade.l2_events,
            cascade.l3_traits
        );
    }

    let pending = engine
        .memory_l1(L1BrowseRequest {
            persona: Some(uid.to_string()),
            unabsorbed_only: true,
            limit: Some(1),
            offset: Some(0),
        })
        .await?;
    if pending.total >= UNASSORBED_L1_LIMIT {
        eprintln!(
            "  {}: 残留 {} 条未吸收 L1（L2 异步消费，留待后续轮次；清单按实际记录）",
            sample.sample_id, pending.total
        );
    }

    Ok(stats)
}

/// 样本在窗口内的会话数与消息数（语料事实，用于续跑恢复统计）。
///
/// 参数:
/// - sample: 语料样本；
/// - window: 建库窗口（决定哪些会话被灌入）。
///
/// 返回:
/// - 窗口内的会话数与消息数之和；与 ingest_sample 的过滤口径一致。
pub(super) fn window_counts(sample: &Sample, window: &TierSpec) -> (usize, usize) {
    sample
        .sessions
        .iter()
        .filter(|session| window.includes_date(session.date))
        .fold((0usize, 0usize), |(sessions, messages), session| {
            (sessions + 1, messages + session.messages.len())
        })
}

/// 当日零点的 UTC 毫秒。
fn day_start_ms(date: NaiveDate) -> i64 {
    date.and_time(NaiveTime::MIN).and_utc().timestamp_millis()
}
