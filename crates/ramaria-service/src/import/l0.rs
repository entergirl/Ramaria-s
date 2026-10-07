//! crates/ramaria-service/src/import/l0.rs - Ramaria QQ 聊天记录 L0 写入模块
//!
//! 设计特点:
//! - 写入路径按对话类型分流：私聊双画像准备（导出者 / 对方），群聊按发送者聚合成员并逐成员准备画像
//! - 双画像准备：UID 优先级 > QQ 号 > 平台 UID > 递增序号；导入侧过滤跳过时不创建
//! - 群聊成员画像：成员聚合按发送者归并（跳过空 UID），UID 前缀 user- / char-，会话归属导出者
//! - `ImportWriter::write_l0` 写入会话与消息（指纹去重，失败补偿删除会话）
//! - 画像名从库内回读上报（复用既有 persona 时以库内实际注册名为准），回读失败回退展示名
//! - 未附着 SQLite 连接池时显式报错（`Engine::attach_sqlite_pool`），不 panic
//! - 隐私：个人标识一律经 `mask_id` 脱敏，文件路径只留文件名

use std::collections::BTreeMap;
use std::path::PathBuf;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::types::MemberRole;
use ramaria_importer::ImportSource;
use ramaria_importer::qq::{
    ImportSide, PersonaSide, QqImporter, build_persona_uid, ensure_qq_persona,
};
use ramaria_importer::writer::{ImportWriter, MemberDispatch, PersonaDispatch};
use sqlx::SqlitePool;
use uuid::Uuid;

use crate::engine::Engine;

use super::analyze::report_time_range;
use super::detect::{detect_format, ensure_json_extension, path_log_label};

// =========================================================
// 请求与结果类型
// =========================================================

/// 导入模式。
///
/// 状态:
/// - `Fast`: 仅写入 L0（会话 + 消息），不触发后续记忆加工；
/// - `Deep`: L0 写入后由宿主继续调用 L1 批量生成与深度处理触发。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportMode {
    /// 快速导入：仅写入 L0 消息
    Fast,
    /// 深度导入：L0 之后继续 L1 摘要与 L2/L3 级联
    Deep,
}

impl ImportMode {
    /// 返回模式的稳定字符串标识（`fast` / `deep`）。
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Deep => "deep",
        }
    }
}

/// L0 导入请求。
///
/// 字段约定:
/// - `persona_name` / `other_persona_name`: 画像显示名覆盖（None 时用文件内解析名）；
/// - `self_persona_uid` / `other_persona_uid`: 画像 UID 显式指定（None 时按平台策略生成）；
/// - `side`: 导入侧过滤（仅我方 / 仅对方 / 双方）。
#[derive(Debug, Clone)]
pub struct ImportRequest {
    /// 聊天记录文件路径（.json）
    pub file_path: PathBuf,
    /// 导入模式
    pub mode: ImportMode,
    /// session 切割时间间隔（分钟）
    pub gap_minutes: u32,
    /// 导入侧过滤
    pub side: ImportSide,
    /// 导出者画像显示名（None → 文件内解析名）
    pub persona_name: Option<String>,
    /// 导出者画像 UID（None → 按平台策略生成）
    pub self_persona_uid: Option<String>,
    /// 对方画像显示名（None → 文件内解析名）
    pub other_persona_name: Option<String>,
    /// 对方画像 UID（None → 按平台策略生成）
    pub other_persona_uid: Option<String>,
}

/// L0 写入结果（供宿主展示并继续触发 L1 / 深度处理）。
#[derive(Debug, Clone)]
pub struct ImportL0Outcome {
    /// 解析报告摘要（人类可读文本）
    pub report_summary: String,
    /// 写入的 session 数
    pub sessions_written: usize,
    /// 写入的消息总数
    pub messages_written: usize,
    /// 因画像缺失被丢弃的消息数
    pub messages_dropped: usize,
    /// 导出者画像 UID（导入侧过滤跳过时为 None）
    pub persona_uid: Option<String>,
    /// 导出者画像名称（库内实际注册名；画像未创建或回读失败时为请求覆盖名 / 文件解析名）
    pub persona_name: String,
    /// 对方画像 UID（导入侧过滤跳过时为 None）
    pub other_persona_uid: Option<String>,
    /// 对方画像名称（库内实际注册名；画像未创建或回读失败时为请求覆盖名 / 文件解析名）
    pub other_persona_name: String,
    /// 导出者名称（从文件中解析，供预览对照；与 `persona_name` 可能不同）
    pub self_name: String,
    /// 对话对象名称
    pub chat_name: String,
    /// 消息时间范围（`起 ~ 止`；未知时为"未知"）
    pub time_range: String,
    /// 跳过的消息数（撤回 + 空 + 未知类型）
    pub skipped_count: usize,
    /// 写入的 session UUID 列表（供继续触发 L1 与深度处理）
    pub session_ids: Vec<Uuid>,
    /// 本次导入模式
    pub mode: ImportMode,
    /// 对话类型（private / group / unknown）
    pub chat_type: String,
    /// 解析成员分布（消息数降序；私聊为双方），供宿主展示
    pub members: Vec<ramaria_importer::ImportMemberStat>,
}

// =========================================================
// 用例入口
// =========================================================

/// 执行 QQ 聊天记录 L0 导入（画像准备 + 会话 / 消息写入）。
///
/// 流程:
/// 1. 连接池与扩展名校验；
/// 2. 格式检测与文件解析（空会话显式报错）；
/// 3. 按对话类型分流：私聊准备双画像（导出者 / 对方），
///    群聊按发送者聚合成员并逐成员准备 persona（按导入侧过滤跳过时不创建）；
/// 4. `ImportWriter::write_l0` 写入会话与消息（指纹去重，失败补偿删除会话）；
///    画像名从库内回读上报（回读失败回退请求覆盖名 / 文件解析名）。
///
/// 参数:
/// - `engine`: 服务层引擎（须已附着 SQLite 连接池）；
/// - `req`: 导入请求（文件 / 模式 / 切割间隔 / 导入侧 / 画像覆盖）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、双画像标识与 session UUID 列表。
pub(crate) async fn write_l0(
    engine: &Engine,
    req: ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    let Some(pool) = engine.sqlite_pool() else {
        return Err(RamariaError::unsupported(
            "导入用例需要 SQLite 连接池（Engine::attach_sqlite_pool）",
        ));
    };
    let path = req.file_path.as_path();
    ensure_json_extension(path)?;

    tracing::info!(
        file = %path_log_label(path),
        mode = %req.mode.as_str(),
        gap_minutes = req.gap_minutes,
        side = ?req.side,
        "开始 QQ 聊天记录导入"
    );

    // ---- 1. 格式检测与文件解析 ----
    let is_qq = detect_format(engine, path).await?;

    if !is_qq {
        return Err(RamariaError::validation(format!(
            "文件 '{}' 不是 QQ 聊天记录格式。请确认文件来自 qq-chat-exporter v6.x 导出的 JSON。",
            path.display()
        )));
    }

    let importer = QqImporter::new();
    let (sessions, report) = importer.parse(path, req.gap_minutes).inspect_err(|_| {
        tracing::warn!(file = %path_log_label(path), "QQ 聊天记录文件解析失败");
    })?;

    if sessions.is_empty() {
        return Err(RamariaError::validation(format!(
            "文件中没有可导入的消息。解析报告: 原始 {} 条，成功 0 条，跳过 {} 条。",
            report.total_raw,
            report.total_skipped()
        )));
    }

    tracing::info!(
        session_count = sessions.len(),
        total_success = report.total_success(),
        total_degraded = report.total_degraded(),
        total_skipped = report.total_skipped(),
        "文件解析完成"
    );

    // ---- 2. 画像准备与写入（按对话类型分流） ----
    // 群聊：按发送者聚合成员并逐成员准备 persona；私聊：导出者 / 对方双画像
    if report.chat_type == "group" {
        import_group(&pool, &sessions, &report, &req).await
    } else {
        import_dual(&pool, &sessions, &report, &req).await
    }
}

// =========================================================
// 私聊导入（双画像）
// =========================================================

/// 私聊导入：准备导出者 / 对方双画像并写入会话与消息。
///
/// 参数:
/// - `pool`: 数据库连接池；
/// - `sessions`: 解析后的会话列表；
/// - `report`: 解析报告（画像标识与名称来源）；
/// - `req`: 导入请求（导入侧过滤与画像覆盖）。
///
/// 返回:
/// - `ImportL0Outcome`：写入统计、双画像标识与 session UUID 列表。
async fn import_dual(
    pool: &SqlitePool,
    sessions: &[ramaria_importer::ImportedSession],
    report: &ramaria_importer::ImportReport,
    req: &ImportRequest,
) -> RamariaResult<ImportL0Outcome> {
    // ---- 1. 双画像准备 ----
    // 查询已有 QQ persona 最大 seq（用于 UID 递增序号兜底）
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

    // 1a. 导出者（我方）：UID 前缀 user-（kind=user）；导入侧过滤跳过时不创建
    // 请求覆盖名仅作创建入参；复用既有 persona 时以库内名为准（结果构建阶段回读）
    let self_requested_name = req
        .persona_name
        .clone()
        .unwrap_or_else(|| report.self_name.clone());
    let self_default_uid = build_persona_uid(
        PersonaSide::Me,
        req.self_persona_uid.as_deref(),
        report.self_uin.as_deref(),
        &report.self_id,
        max_qq_seq + 1,
    );
    let self_persona_uid = if req.side.needs_persona(PersonaSide::Me) {
        let resolved = ensure_qq_persona(
            pool,
            &self_default_uid,
            &self_requested_name,
            Some(&report.self_id),
        )
        .await
        .map_err(|e| {
            tracing::error!(
                error = %e,
                persona_uid = %mask_id(&self_default_uid),
                persona_name = %mask_id(&self_requested_name),
                "创建/查找导出者 persona 失败"
            );
            e
        })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&self_requested_name),
            "导出者 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过导出者 persona");
        None
    };

    // 1b. 对方：UID 前缀 char-；导入侧过滤跳过时不创建
    let other_name = req.other_persona_name.clone().unwrap_or_else(|| {
        if report.other_name.is_empty() {
            report.chat_name.clone()
        } else {
            report.other_name.clone()
        }
    });
    let other_ref_id = if report.other_uid.is_empty() {
        None
    } else {
        Some(report.other_uid.as_str())
    };
    let other_default_uid = build_persona_uid(
        PersonaSide::Other,
        req.other_persona_uid.as_deref(),
        report.other_uin.as_deref(),
        &report.other_uid,
        max_qq_seq + 2,
    );
    tracing::debug!(
        other_uid = %mask_id(&other_default_uid),
        other_name = %mask_id(&other_name),
        other_ref_id = ?other_ref_id.map(mask_id),
        "准备创建对方 persona"
    );
    let other_persona_uid = if req.side.needs_persona(PersonaSide::Other) {
        let resolved = ensure_qq_persona(pool, &other_default_uid, &other_name, other_ref_id)
            .await
            .map_err(|e| {
                tracing::error!(
                    error = %e,
                    other_uid = %mask_id(&other_default_uid),
                    other_name = %mask_id(&other_name),
                    "创建/查找对方 persona 失败"
                );
                e
            })?;
        tracing::info!(
            persona_uid = %mask_id(&resolved),
            persona_name = %mask_id(&other_name),
            "对方 Persona 已准备"
        );
        Some(resolved)
    } else {
        tracing::info!("导入侧过滤：跳过对方 persona");
        None
    };

    // ---- 2. L0 写入（按导入侧过滤消息；跳过侧画像为 None） ----
    tracing::debug!(
        sessions_count = sessions.len(),
        self_persona = ?self_persona_uid.as_deref().map(mask_id),
        other_persona = ?other_persona_uid.as_deref().map(mask_id),
        side = ?req.side,
        "准备执行 L0 写入"
    );

    let outcome = ImportWriter::write_l0(
        pool,
        sessions,
        PersonaDispatch::Dual {
            self_uid: &report.self_id,
            self_persona_uid: self_persona_uid.as_deref(),
            other_persona_uid: other_persona_uid.as_deref(),
        },
        req.side,
        req.file_path.parent(),
    )
    .await
    .map_err(|e| {
        tracing::error!(error = %e, "导入写入失败");
        e
    })?;

    tracing::info!(
        sessions_written = outcome.sessions_written,
        messages_written = outcome.messages_written,
        messages_dropped = outcome.messages_dropped,
        "L0 写入完成"
    );
    if outcome.messages_dropped > 0 {
        tracing::warn!(
            messages_dropped = outcome.messages_dropped,
            "导入存在因画像缺失被丢弃的消息（不记录消息内容）"
        );
    }

    // ---- 3. 构建结果 ----
    let time_range = report_time_range(report);
    let report_summary = report.summary();
    let skipped_count = report.total_skipped();
    // 文件解析名：预览对照口径，与画像实际注册名（`persona_name`）区分
    let self_name = report.self_name.clone();
    let chat_name = report.chat_name.clone();

    // 画像名从库内回读（复用既有 persona 时可能与请求覆盖名 / 文件解析名不同）；
    // 回读失败只记 warn 并回退展示名，不阻塞导入
    let persona_name =
        resolve_persona_name(pool, self_persona_uid.as_deref(), &self_requested_name).await;
    let other_persona_name =
        resolve_persona_name(pool, other_persona_uid.as_deref(), &other_name).await;

    Ok(ImportL0Outcome {
        report_summary,
        sessions_written: outcome.sessions_written,
        messages_written: outcome.messages_written,
        messages_dropped: outcome.messages_dropped,
        persona_uid: self_persona_uid,
        persona_name,
        other_persona_uid,
        other_persona_name,
        self_name,
        chat_name,
        time_range,
        skipped_count,
        session_ids: outcome.session_ids,
        mode: req.mode,
        chat_type: report.chat_type.clone(),
        members: report.members.clone(),
    })
}

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
async fn import_group(
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

// =========================================================
// 内部工具
// =========================================================

/// 回读 persona 在库内的实际注册名（不阻塞导入）。
///
/// 说明:
/// - persona 可能按 uid / ref_id 复用既有条目，上报名以库内值为准；
/// - `persona_uid` 为 None（导入侧过滤跳过）或回读失败时返回 `fallback`
///   （请求覆盖名 / 文件解析名），仅记 warn。
pub(super) async fn resolve_persona_name(
    pool: &SqlitePool,
    persona_uid: Option<&str>,
    fallback: &str,
) -> String {
    let Some(uid) = persona_uid else {
        return fallback.to_string();
    };
    match ramaria_storage::repo::personas::get_by_uid(pool, uid).await {
        Ok(Some(persona)) => persona.name,
        Ok(None) => {
            tracing::warn!(
                persona_uid = %mask_id(uid),
                "回读 persona 名称未命中（回退请求覆盖名 / 文件解析名）"
            );
            fallback.to_string()
        }
        Err(error) => {
            tracing::warn!(
                persona_uid = %mask_id(uid),
                error = %error,
                "回读 persona 名称失败（回退请求覆盖名 / 文件解析名，不阻塞导入）"
            );
            fallback.to_string()
        }
    }
}
