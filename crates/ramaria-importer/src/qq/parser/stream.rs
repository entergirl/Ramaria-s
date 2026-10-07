//! crates/ramaria-importer/src/qq/parser/stream.rs - 流式解析驱动与对外解析入口
//!
//! 设计特点:
//! - 顶层对象单趟流式消费，峰值内存从约 3~4× 文件降到约 1× 文件
//! - chatInfo 捕获为单条 Value；messages 数组逐元素解码，仅持有单条消息
//! - 依赖 qce 导出顺序（chatInfo 位于 messages 之前），异常顺序防御性跳过并计入报告
//! - 语法错误从 deserializer 提取行/列位置结构化返回，不 panic
//! - `parse_qq_export` 为唯一对外解析入口：校验顶层字段形态后切割会话并填充报告

use std::collections::HashSet;
use std::fs;
use std::path::Path;

use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;

use crate::error;
use crate::traits::{ImportMemberStat, ImportReport, ImportedSession, ParsedMessage};

use super::detect::decode_bytes;
use super::message::parse_json_message;
use super::sessions::split_into_sessions;
use super::time::ts_ms_to_date;

// =========================================================
// 流式解析（顶层对象单趟消费，显著降低大文件峰值内存）
// =========================================================
// 说明:
// - 不再把整份 JSON 物化为 `serde_json::Value` 树，而是用
//   `serde_json::Deserializer` 配合自定义 Visitor 逐元素消费。
// - chatInfo 为小块数据，捕获为单条 Value；messages 数组逐元素解码，
//   每次仅持有单条消息，峰值内存从约 3~4× 文件降到约 1× 文件。
// - 依赖 qce 导出顺序：顶层对象中 `chatInfo` 位于 `messages` 之前
//   （该顺序由 shuakami/qq-chat-exporter v6.x 保证）。
// - 语法错误从 deserializer 提取行/列位置结构化返回，不 panic。

/// chatInfo 解析出的元信息，驱动消息角色映射与报告填充。
struct ChatMeta {
    /// 导出者 QQ 内部 UID。
    self_uid: String,
    /// 导出者显示名称。
    self_name: String,
    /// 导出者 QQ 号（chatInfo.selfUin），缺失或为空时为 None。
    self_uin: Option<String>,
    /// 对话名称。
    chat_name: String,
    /// 对话类型（private / group）。
    chat_type: String,
    /// 对话对方 QQ 内部 UID（chatInfo.peerUid）。
    peer_uid: String,
    /// 对话对方 QQ 号（chatInfo.peerUin），缺失或为空时为 None。
    peer_uin: Option<String>,
}

impl ChatMeta {
    /// 从 chatInfo Value 提取元信息，字段缺失时采用与整读解析一致的默认值。
    fn from_value(v: &serde_json::Value) -> Self {
        // QQ 号字段缺失或为空字符串时视为未提供（None）。
        let opt_str = |key: &str| {
            v.get(key)
                .and_then(|x| x.as_str())
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        Self {
            self_uid: v
                .get("selfUid")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            self_name: v
                .get("selfName")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            self_uin: opt_str("selfUin"),
            chat_name: v
                .get("name")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            chat_type: v
                .get("type")
                .and_then(|x| x.as_str())
                .unwrap_or("unknown")
                .to_string(),
            peer_uid: v
                .get("peerUid")
                .and_then(|x| x.as_str())
                .unwrap_or("")
                .to_string(),
            peer_uin: opt_str("peerUin"),
        }
    }
}

/// 单趟流式解析的累积上下文。
///
/// 字段职责:
/// - `chat_meta`: 从 chatInfo 提取的元信息（消息角色映射依赖）。
/// - `report`: 与整读解析同构的诊断报告，逐条消息累积统计。
/// - `seen_keys`: 文件内 (id, timestamp) 去重集合。
/// - `parsed_messages`: 按文件序去重后保留的首现消息解析结果（后按时间稳定排序）。
/// - 形态标志位: 记录 chatInfo / messages 顶层字段出现与形态，供格式校验。
struct ParseCtx {
    chat_meta: Option<ChatMeta>,
    report: ImportReport,
    seen_keys: HashSet<(String, i64)>,
    parsed_messages: Vec<ParsedMessage>,
    chat_info_seen: bool,
    messages_seen: bool,
    messages_is_array: bool,
}

/// 顶层 map 的 seed：逐 key 流式分派。
struct TopLevelSeed<'a> {
    ctx: &'a mut ParseCtx,
}

impl<'de> DeserializeSeed<'de> for TopLevelSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        deserializer.deserialize_map(TopLevelVisitor { ctx: self.ctx })
    }
}

/// 顶层 map 的 visitor：识别 chatInfo 与 messages，其余 key 跳过。
struct TopLevelVisitor<'a> {
    ctx: &'a mut ParseCtx,
}

impl<'de> Visitor<'de> for TopLevelVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("QQ 导出 JSON 顶层对象")
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let ctx = self.ctx;
        while let Some(key) = map.next_key::<String>()? {
            match key.as_str() {
                "chatInfo" => {
                    let v: serde_json::Value = map.next_value()?;
                    ctx.chat_info_seen = true;
                    ctx.chat_meta = Some(ChatMeta::from_value(&v));
                }
                "messages" => {
                    map.next_value_seed(MessagesSeed { ctx: &mut *ctx })?;
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        Ok(())
    }
}

/// messages 数组的 seed：流式逐元素解码。
struct MessagesSeed<'a> {
    ctx: &'a mut ParseCtx,
}

impl<'de> DeserializeSeed<'de> for MessagesSeed<'_> {
    type Value = ();

    fn deserialize<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
    where
        D: serde::de::Deserializer<'de>,
    {
        deserializer.deserialize_seq(MessagesVisitor { ctx: self.ctx })
    }
}

/// messages 值的 visitor。
///
/// 职责:
/// - 数组形态（`visit_seq`）时逐元素解码为单条 Value 并复用既有单条解析。
/// - 非数组形态（对象/标量/null）时只标记形态，不报错——交由上层按
///   "缺少 messages" 的 format_mismatch 语义决策，与原整读解析一致。
struct MessagesVisitor<'a> {
    ctx: &'a mut ParseCtx,
}

impl<'de> Visitor<'de> for MessagesVisitor<'_> {
    type Value = ();

    fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str("messages 数组")
    }

    fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let ctx = &mut *self.ctx;
        ctx.messages_seen = true;
        ctx.messages_is_array = true;
        while let Some(elem) = seq.next_element::<serde_json::Value>()? {
            consume_message(&mut *ctx, elem);
        }
        Ok(())
    }

    fn visit_map<A>(self, mut map: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        self.ctx.messages_seen = true;
        // 消费完整对象以保证语法校验不因提前返回而遗漏尾部输入。
        while map.next_key::<IgnoredAny>()?.is_some() {
            map.next_value::<IgnoredAny>()?;
        }
        Ok(())
    }

    fn visit_str<E>(self, _v: &str) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_string<E>(self, _v: String) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_i64<E>(self, _v: i64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_u64<E>(self, _v: u64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_f64<E>(self, _v: f64) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_bool<E>(self, _v: bool) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E>
    where
        E: serde::de::Error,
    {
        self.ctx.messages_seen = true;
        Ok(())
    }
}

/// 处理单条 messages 元素：文件内去重、解析并计入报告。
fn consume_message(ctx: &mut ParseCtx, elem: serde_json::Value) {
    ctx.report.total_raw += 1;

    let id = elem
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let ts = elem.get("timestamp").and_then(|v| v.as_i64()).unwrap_or(0);
    let key = (id, ts);

    // Layer 1 去重：文件内 (id, timestamp) 联合键，保留首现
    if !ctx.seen_keys.insert(key) {
        ctx.report.dedup_removed += 1;
        return;
    }

    // 依赖 qce 导出顺序（chatInfo 在 messages 之前）以提供角色映射所需的元信息；
    // 遇到该顺序不满足的异常结构时防御性跳过解析（不 panic）。
    // 丢弃必须可观测：计入报告的 skipped_missing_meta，首条触发一次 warn（避免逐条刷屏）。
    let Some(meta) = ctx.chat_meta.as_ref() else {
        ctx.report.skipped_missing_meta += 1;
        if ctx.report.skipped_missing_meta == 1 {
            tracing::warn!(
                file = %ctx.report.file_path,
                "检测到 messages 出现在 chatInfo 之前（或缺失 chatInfo），消息因缺少元信息被丢弃"
            );
        }
        return;
    };

    let parsed = parse_json_message(&elem, &meta.self_uid, &meta.self_name, &mut ctx.report);
    if let Some(msg) = parsed {
        ctx.parsed_messages.push(msg);
    }
}

/// 将 serde_json 解析错误转为带行列位置的 json_parse_error。
fn json_parse_error_with_position(path: &Path, e: &serde_json::Error) -> RamariaError {
    let line = e.line();
    let detail = if line > 0 {
        format!("{}（第 {line} 行第 {} 列）", e, e.column())
    } else {
        e.to_string()
    };
    error::json_parse_error(&path.display().to_string(), &detail)
}

// =========================================================
// 成员分布聚合
// =========================================================

/// 按发送者聚合解析结果，生成成员分布统计。
///
/// 归并规则:
/// - 按 `sender_uid` 归并；空 UID 的消息不参与统计；
/// - `name` 取该发送者最后一条非空显示名（消息序即时间序）；
/// - `uin` 取该发送者首个非空值；
/// - `message_count` 计该发送者成功解析的消息条数。
///
/// 排序:
/// - 消息数降序；同条数按名称升序；名称相同按 UID 升序（输出完全确定）。
pub(super) fn aggregate_members(messages: &[ParsedMessage]) -> Vec<ImportMemberStat> {
    let mut stats: std::collections::BTreeMap<String, ImportMemberStat> =
        std::collections::BTreeMap::new();
    for msg in messages {
        if msg.sender_uid.is_empty() {
            continue;
        }
        let entry = stats
            .entry(msg.sender_uid.clone())
            .or_insert_with(|| ImportMemberStat {
                uid: msg.sender_uid.clone(),
                uin: None,
                name: String::new(),
                message_count: 0,
            });
        entry.message_count += 1;
        if entry.uin.is_none() {
            entry.uin = msg.sender_uin.clone().filter(|u| !u.is_empty());
        }
        if !msg.sender_name.is_empty() {
            entry.name = msg.sender_name.clone();
        }
    }
    let mut members: Vec<ImportMemberStat> = stats.into_values().collect();
    members.sort_by(|a, b| {
        b.message_count
            .cmp(&a.message_count)
            .then_with(|| a.name.cmp(&b.name))
            .then_with(|| a.uid.cmp(&b.uid))
    });
    members
}

// =========================================================
// 主解析函数（对外接口）
// =========================================================

/// 解析 qq-chat-exporter v6.x JSON 聊天记录文件。
///
/// 这是 QQ 导入器的唯一对外解析接口。
///
/// 参数:
/// - `file_path`: 文件路径。
/// - `gap_minutes`: session 切割时间间隔阈值（分钟），默认 10。
///
/// 返回:
/// - `(sessions, report)`: 解析后的 session 列表和诊断报告。
///
/// 错误:
/// - 文件不存在 → `file_not_found`
/// - JSON 解析失败 → `json_parse_error`
/// - 格式不匹配 → `format_mismatch`
pub fn parse_qq_export(
    file_path: &Path,
    gap_minutes: u32,
) -> RamariaResult<(Vec<ImportedSession>, ImportReport)> {
    if !file_path.exists() {
        return Err(error::file_not_found(&file_path.display().to_string()));
    }

    let gap_ms = (gap_minutes as i64) * 60 * 1000;
    let gap_minutes_u = (gap_ms / 60_000) as u32;

    // 读取并解码文件（编码检测需要整读）
    let bytes =
        fs::read(file_path).map_err(|e| error::read_error(&file_path.display().to_string(), e))?;
    let json_str = decode_bytes(&bytes)?;

    let mut ctx = ParseCtx {
        chat_meta: None,
        report: ImportReport {
            file_path: file_path.display().to_string(),
            gap_minutes: gap_minutes_u,
            ..Default::default()
        },
        seen_keys: HashSet::new(),
        parsed_messages: Vec::new(),
        chat_info_seen: false,
        messages_seen: false,
        messages_is_array: false,
    };

    // ── 单趟流式消费顶层对象（chatInfo + messages 逐元素）──
    {
        let mut de = serde_json::Deserializer::from_str(&json_str);
        TopLevelSeed { ctx: &mut ctx }
            .deserialize(&mut de)
            .map_err(|e| json_parse_error_with_position(file_path, &e))?;
        de.end()
            .map_err(|e| json_parse_error_with_position(file_path, &e))?;
    }

    // ── 校验顶层字段出现与形态（优先级与原整读解析一致）──
    if !ctx.chat_info_seen {
        return Err(error::format_mismatch(
            "QQ Chat Exporter JSON（含 chatInfo 字段）",
            "请确认文件是由 shuakami/qq-chat-exporter v6.x 导出的 JSON 格式。",
        ));
    }
    if !ctx.messages_seen || !ctx.messages_is_array {
        return Err(error::format_mismatch(
            "QQ Chat Exporter JSON（含 messages 数组）",
            "文件中缺少 messages 字段。",
        ));
    }

    let meta = ctx
        .chat_meta
        .as_ref()
        .expect("chatInfo_seen 为真时 chat_meta 必已填充");

    let mut report = ctx.report;
    report.self_id = meta.self_uid.clone();
    report.self_name = meta.self_name.clone();
    report.self_uin = meta.self_uin.clone();
    report.chat_name = meta.chat_name.clone();
    report.chat_type = meta.chat_type.clone();
    // 对方标识直接从 chatInfo 提取（v6.x 无需扫描消息列表）
    report.other_uid = meta.peer_uid.clone();
    report.other_uin = meta.peer_uin.clone();
    report.other_name = meta.chat_name.clone();

    tracing::info!(
        file = %report.file_path,
        self_name = %mask_id(&report.self_name),
        chat_name = %mask_id(&report.chat_name),
        chat_type = %report.chat_type,
        peer_uid = %mask_id(&report.other_uid),
        total_raw = report.total_raw,
        "开始解析 QQ JSON 聊天记录 (v6.x)"
    );

    // ── 按时间戳升序稳定排序（去重后保留首现次序；与原整读解析一致）──
    let mut parsed_messages = ctx.parsed_messages;
    parsed_messages.sort_by_key(|m| m.created_at);

    // ── 成员分布聚合（按发送者归并；空 UID 不参与）──
    report.members = aggregate_members(&parsed_messages);

    // ── Session 切割 ──
    let sessions = split_into_sessions(&parsed_messages, gap_ms);
    report.session_count = sessions.len();

    // ── 时间范围 ──
    if let (Some(first), Some(last)) = (parsed_messages.first(), parsed_messages.last()) {
        report.time_start = ts_ms_to_date(first.created_at);
        report.time_end = ts_ms_to_date(last.created_at);
    }

    if report.skipped_missing_meta > 0 {
        tracing::warn!(
            file = %report.file_path,
            skipped_missing_meta = report.skipped_missing_meta,
            "存在因缺少 chatInfo 元信息被丢弃的消息（messages 先于 chatInfo 出现或 chatInfo 缺失）"
        );
    }

    tracing::info!(
        sessions = report.session_count,
        success = report.total_success(),
        degraded = report.total_degraded(),
        skipped = report.total_skipped(),
        skipped_system = report.skipped_system,
        skipped_missing_meta = report.skipped_missing_meta,
        dedup_removed = report.dedup_removed,
        "QQ JSON 解析完成"
    );

    if sessions.is_empty() {
        report
            .warnings
            .push("未解析出任何有效消息（全部被跳过或不支持）".to_string());
    }

    Ok((sessions, report))
}
