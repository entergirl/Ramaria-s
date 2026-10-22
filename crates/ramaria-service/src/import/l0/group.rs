//! crates/ramaria-service/src/import/l0/group.rs - Ramaria 群聊导入（多画像）
//!
//! 设计特点:
//! - 成员聚合按发送者归并（跳过空 UID），展示名取最近一条非空值
//! - 逐成员准备画像，会话归属导出者；导入侧过滤跳过的成员不建画像
//! - 消息按发送者分派归属，成员分布随结果上报

use std::collections::BTreeMap;

use ramaria_core::error::RamariaResult;
use ramaria_core::privacy::mask_id;
use ramaria_core::types::MemberRole;
use ramaria_importer::qq::{PersonaSide, build_persona_uid, ensure_qq_persona};
use ramaria_importer::writer::{ImportWriter, MemberDispatch, PersonaDispatch};
use sqlx::SqlitePool;

use super::tools::resolve_persona_name;
use super::types::{ImportL0Outcome, ImportRequest};
use crate::import::analyze::report_time_range;

// =========================================================
// 群聊导入（多画像）
// =========================================================

/// 群聊成员聚合（按发送者归并的在群展示信息）。
///
/// 字段约定:
/// - `uin`: 平台账号级 ID（首个非空值）；
/// - `name`: 最近一条非空显示名；
/// - `group_nickname`: 群名片（首个非空值）；
/// - `role`: 群内角色（首个非空值）。
struct GroupMemberAgg {
    uin: Option<String>,
    name: String,
    group_nickname: Option<String>,
    role: Option<MemberRole>,
}

/// 群聊导入：按发送者聚合成员、逐成员准备 persona，消息按发送者分派归属。
///
/// 参数:
/// - `pool`: 数据库连接池；
/// - `sessions`: 解析后的会话列表；
/// - `report`: 解析报告（导出者标识 / 成员分布）；
/// - `req`: 导入请求（导入侧过滤；群聊忽略对方画像覆盖参数）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、导出者画像标识与 session UUID 列表。
pub(super) async fn import_group(
    pool: &SqlitePool,
    sessions: &[ramaria_importer::ImportedSession],
    report: &ramaria_importer::ImportReport,
    req: &ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    // ---- 1. 成员聚合（按时间序；跳过空 UID） ----
    let mut aggs: BTreeMap<String, GroupMemberAgg> = BTreeMap::new();
    for session in sessions {
        for parsed in &session.messages {
            if parsed.sender_uid.is_empty() {
                continue;
            }
            let agg = aggs
                .entry(parsed.sender_uid.clone())
                .or_insert_with(|| GroupMemberAgg {
                    uin: None,
                    name: String::new(),
                    group_nickname: None,
                    role: None,
                });
            if agg.uin.is_none() {
                agg.uin = parsed.sender_uin.clone();
            }
            if agg.group_nickname.is_none() {
                agg.group_nickname = parsed.group_nickname.clone();
            }
            if agg.role.is_none() {
                agg.role = parsed.member_role;
            }
            // 显示名取最近一条非空值（消息序即时间序）
            if !parsed.sender_name.is_empty() {
                agg.name = parsed.sender_name.clone();
            }
        }
    }

    // ---- 2. 逐成员准备 persona（BTreeMap 稳定序；导入侧过滤跳过不建） ----
    // 群聊按成员自动准备 persona，不使用单一"对方"画像覆盖参数
    if req.other_persona_uid.is_some() || req.other_persona_name.is_some() {
        tracing::debug!(
            "群聊导入按成员自动准备 persona，忽略对方画像覆盖参数（other_persona_uid / other_persona_name）"
        );
    }
    let self_requested_name = req
        .persona_name
        .clone()
        .unwrap_or_else(|| report.self_name.clone());
    let all_personas = ramaria_storage::repo::personas::list_all(pool)
        .await
        .map_err(|e| {
            tracing::error!(error = %e, "查询已有 persona 列表失败");
            e
        })?;
    let max_qq_seq: u32 = all_personas
        .iter()
        .filter(|p| p.source == "qq")
        .map(|p| p.seq as u32)
        .max()
        .unwrap_or(0);

    let mut seq = max_qq_seq + 1;
    let mut members: BTreeMap<String, MemberDispatch> = BTreeMap::new();
    let mut self_persona_uid: Option<String> = None;
    for (uid, agg) in &aggs {
        let is_self = uid == &report.self_id;
        let side_of = if is_self {
            PersonaSide::Me
        } else {
            PersonaSide::Other
        };
        if !req.side.needs_persona(side_of) {
            tracing::debug!(sender = %mask_id(uid), "导入侧过滤：跳过该成员 persona");
            continue;
        }

        // 名字：self 用请求覆盖名 / 文件解析名；他人用聚合显示名，空则回退发送者 UID
        let name = if is_self {
            self_requested_name.clone()
        } else if agg.name.is_empty() {
            uid.clone()
        } else {
            agg.name.clone()
        };
        // 显式 UID 仅支持导出者；他人按 QQ 号 / 平台 UID / 递增序号生成
        let provided = if is_self {
            req.self_persona_uid.as_deref()
        } else {
            None
        };
        let default_uid = build_persona_uid(side_of, provided, agg.uin.as_deref(), uid, seq);
        seq += 1;
        let resolved = ensure_qq_persona(pool, &default_uid, &name, Some(uid))
            .await
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    persona_uid = %mask_id(&default_uid),
                    persona_name = %mask_id(&name),
                    "创建/查找群聊成员 persona 失败"
                );
                e
            })?;
        tracing::debug!(
            sender = %mask_id(uid),
            persona_uid = %mask_id(&resolved),
            "群聊成员 Persona 已准备"
        );
        if is_self {
            self_persona_uid = Some(resolved.clone());
        }
        members.insert(
            uid.clone(),
            MemberDispatch {
                persona_uid: resolved,
                group_nickname: agg.group_nickname.clone(),
                role: agg.role,
            },
        );
    }

    // ---- 3. 会话归属：导出者 persona 优先，缺失时取首个已准备成员 ----
    let owner_uid = self_persona_uid
        .clone()
        .or_else(|| members.values().next().map(|m| m.persona_uid.clone()));

    tracing::info!(
        member_count = aggs.len(),
        persona_count = members.len(),
        "群聊成员画像准备完成"
    );
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = ?self_persona_uid.as_deref().map(mask_id),
        owner_persona = ?owner_uid.as_deref().map(mask_id),
        side = ?req.side,
        "准备执行群聊 L0 写入"
    );

    // ---- 4. L0 写入（按发送者分派；未命中成员映射的消息丢弃并计入） ----
    let outcome = ImportWriter::write_l0(
        pool,
        sessions,
        PersonaDispatch::Multi {
            self_uid: &report.self_id,
            members: &members,
            owner_uid: owner_uid.as_deref(),
        },
        req.side,
        req.file_path.parent(),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "群聊导入写入失败");
        e
    })?;

    tracing::info!(
        sessions_written = outcome.sessions_written,
        messages_written = outcome.messages_written,
        messages_dropped = outcome.messages_dropped,
        "群聊 L0 写入完成"
    );
    if outcome.messages_dropped > 0 {
        tracing::warn!(
            messages_dropped = outcome.messages_dropped,
            "群聊导入存在因画像缺失被丢弃的消息（不记录消息内容）"
        );
    }

    // ---- 5. 构建结果 ----
    let time_range = report_time_range(report);
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    let persona_name =
        resolve_persona_name(pool, self_persona_uid.as_deref(), &self_requested_name).await;

    Ok(ImportL0Outcome {
        report_summary,
        sessions_written: outcome.sessions_written,
        messages_written: outcome.messages_written,
        messages_dropped: outcome.messages_dropped,
        persona_uid: self_persona_uid,
        persona_name,
        // 群聊无单一"对方"画像：成员标识经 `members` 分布承载
        other_persona_uid: None,
        other_persona_name: String::new(),
        self_name: report.self_name.clone(),
        chat_name: report.chat_name.clone(),
        time_range,
        skipped_count,
        session_ids: outcome.session_ids,
        mode: req.mode,
        chat_type: report.chat_type.clone(),
        members: report.members.clone(),
    })
}
