//! crates/ramaria-service/src/keyword.rs - 关键词词典用例（列表 / 幂等注入 / 待确认别名 / 建议与裁决）
//!
//! 设计特点:
//! - 只读列表 + 别名裁决状态机：三态（canonical / alias / pending）展示与
//!   pending → alias（确认合并）/ canonical（驳回晋升）迁移
//! - seed 幂等注入：整体校验后去重，已存在词条保持现状（不递增 use_count、
//!   不改别名状态），新词条从 use_count 0 起写入
//! - 待确认别名建议：汇总关键词池与内存镜像使用量，把相似词对经筛选后登记为
//!   pending（单次运行有登记上限；已建立词条不重复登记）
//! - confirm 且词条已是 alias 时恒为幂等成功（不写库，`already_applied` 置位）；
//!   其余非 pending 报业务校验错误
//! - 非法输入显式校验：关键词文本经 `KeywordToken` 标准化（空 / 超长拒绝），
//!   词条不存在 / 非 pending 均返回业务校验错误，不静默成功
//! - 日志脱敏：别名文本在日志中只保留长度与短哈希标签，正文不入日志

use std::collections::HashMap;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::keyword::KeywordToken;
use ramaria_core::lock::read_recover;
use ramaria_core::traits::StorageBackend;
use ramaria_memory::keyword::AliasManager;

use crate::engine::Engine;
use crate::types::{
    AliasAction, AliasResolveOutcome, AliasResolveRequest, KeywordEntryView, KeywordPoolView,
    KeywordSeedItem, KeywordSeedOutcome, KeywordSuggestionOutcome, PendingAliasView,
};

// =========================================================
// 词条状态与文本校验
// =========================================================

/// 词条状态文本（keyword_pool 口径: canonical / alias / pending）。
///
/// 说明:
/// - `NULL` / `"canonical"` 及未知取值统一兜底为 `canonical`（与词表装载口径一致）。
pub(crate) fn status_of(alias_status: &Option<String>) -> &'static str {
    match alias_status.as_deref() {
        Some("alias") => "alias",
        Some("pending") => "pending",
        _ => "canonical",
    }
}

/// 校验关键词文本为合法标准化 token。
///
/// 返回:
/// - 成功时返回 trim + 英文小写后的标准化 token。
/// - 空 / 纯空白 / 超长（> 256 字节）时返回业务校验错误。
pub(crate) fn parse_keyword(raw: &str) -> RamariaResult<KeywordToken> {
    KeywordToken::new(raw).ok_or_else(|| {
        RamariaError::validation(format!("无效关键词: '{raw}'（需非空且不超过 256 字符）"))
    })
}

// =========================================================
// 关键词池列表
// =========================================================

/// 列出关键词池全部词条（含三态计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 全量词条视图（存储层稳定排序）与 canonical / alias / pending 三态计数。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<KeywordPoolView> {
    let entries = engine.storage_ref().list_keyword_pool_entries().await?;

    let mut canonical_count = 0usize;
    let mut alias_count = 0usize;
    let mut pending_count = 0usize;
    let mut keywords = Vec::with_capacity(entries.len());

    for e in entries {
        match status_of(&e.alias_status) {
            "canonical" => canonical_count += 1,
            "alias" => alias_count += 1,
            _ => pending_count += 1,
        }
        keywords.push(KeywordEntryView {
            keyword: e.keyword,
            use_count: e.use_count,
            status: status_of(&e.alias_status).to_string(),
            canonical_id: e.canonical_id,
            canonical_keyword: e.canonical_keyword,
            created_at: e.created_at,
        });
    }

    tracing::debug!(
        total = keywords.len(),
        canonical = canonical_count,
        alias = alias_count,
        pending = pending_count,
        "关键词池列表完成"
    );
    Ok(KeywordPoolView {
        total: keywords.len(),
        canonical_count,
        alias_count,
        pending_count,
        keywords,
    })
}

// =========================================================
// 关键词 seed（幂等手工注入）
// =========================================================

/// 幂等手工注入规范词（对应入口的 `keyword seed`）。
///
/// 流程:
/// 1. 整体解析校验（任一非法即报错，不部分写入）；
/// 2. 去重（保留首次出现顺序；重复注入同一词条只计一次）；
/// 3. 已存在词条保持现状（如实回传其状态，不写库）；
///    新词条幂等插入（并发下由主键冲突 DO NOTHING 兜底，use_count 从 0 起）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `keywords`: 待注入的规范词文本列表（未标准化的原始输入）。
///
/// 返回:
/// - `KeywordSeedOutcome`：新插入 / 跳过计数与逐条结果；
///   空输入返回空结果（非错误），非法词条返回业务校验错误。
pub(crate) async fn seed(
    engine: &Engine,
    keywords: &[String],
) -> RamariaResult<KeywordSeedOutcome> {
    // 先整体解析校验（任一无效即报错，不部分写入）
    let mut tokens: Vec<KeywordToken> = Vec::with_capacity(keywords.len());
    for raw in keywords {
        tokens.push(parse_keyword(raw)?);
    }

    // 去重（保留首次出现顺序；重复注入同一词条只计一次）
    let mut seen: Vec<String> = Vec::with_capacity(tokens.len());
    tokens.retain(|token| {
        let text = token.as_str().to_string();
        if seen.contains(&text) {
            false
        } else {
            seen.push(text);
            true
        }
    });

    // 已存在词条：保持现状（如实回传状态，不写库）；新词条走幂等插入
    let entries = engine.storage_ref().list_keyword_pool_entries().await?;
    let mut results: Vec<KeywordSeedItem> = Vec::with_capacity(tokens.len());

    for token in &tokens {
        let keyword = token.as_str();
        let item = match entries.iter().find(|entry| entry.keyword == keyword) {
            Some(existing) => KeywordSeedItem {
                keyword: keyword.to_string(),
                inserted: false,
                status: status_of(&existing.alias_status).to_string(),
            },
            None => {
                let inserted = engine.storage_ref().seed_keyword_canonical(keyword).await?;
                KeywordSeedItem {
                    keyword: keyword.to_string(),
                    inserted,
                    status: "canonical".to_string(),
                }
            }
        };
        results.push(item);
    }

    let seeded = results.iter().filter(|item| item.inserted).count();
    let skipped = results.len() - seeded;
    tracing::debug!(seeded, skipped, "关键词 seed 完成");
    Ok(KeywordSeedOutcome {
        seeded,
        skipped,
        results,
    })
}

// =========================================================
// 待确认别名
// =========================================================

/// 列出全部待确认别名冲突（pending，别名 → 建议规范词）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 待确认别名视图列表（无待确认项时为空列表）。
pub(crate) async fn pending_aliases(engine: &Engine) -> RamariaResult<Vec<PendingAliasView>> {
    let pending = engine.storage_ref().list_pending_aliases().await?;
    let views: Vec<PendingAliasView> = pending
        .into_iter()
        .map(|p| PendingAliasView {
            alias_id: p.alias_id,
            alias: p.alias_keyword,
            canonical: p.canonical_keyword,
            created_at: p.created_at,
        })
        .collect();

    tracing::debug!(count = views.len(), "待确认别名列表完成");
    Ok(views)
}

// =========================================================
// 别名裁决（确认 / 驳回）
// =========================================================

/// 确认（合并到规范词）或驳回（晋升独立规范词）单个待确认别名。
///
/// 流程:
/// 1. 标准化别名文本并按标准化文本定位词条（不存在 → 业务校验错误）；
/// 2. 状态判定与迁移：
///    - `pending`: 执行确认 / 驳回；条件更新未命中（状态已变化）→ 业务校验错误；
///      成功时 confirm 返回指向的规范词文本、状态 `alias`；
///      reject 规范词为 None、状态 `canonical`；
///    - confirm 且已是 `alias`: 幂等返回成功（不写库，`already_applied` 置位）；
///    - 其余（reject 且非 pending、confirm 且 canonical）: 业务校验错误。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 裁决请求（别名文本 / 动作）。
///
/// 返回:
/// - 裁决结果（别名、处理后规范词、状态、是否幂等命中）。
pub(crate) async fn resolve_alias(
    engine: &Engine,
    req: AliasResolveRequest,
) -> RamariaResult<AliasResolveOutcome> {
    let token = parse_keyword(&req.alias)?;
    let storage = engine.storage_ref();

    let entries = storage.list_keyword_pool_entries().await?;
    let entry = entries
        .iter()
        .find(|e| e.keyword == token.as_str())
        .ok_or_else(|| RamariaError::validation(format!("关键词 '{}' 不存在", req.alias)))?;

    let status = status_of(&entry.alias_status);
    let confirm = matches!(req.action, AliasAction::Confirm);

    if status != "pending" {
        // confirm 且已是 alias：目标状态已达成，幂等成功（不写库）
        if confirm && status == "alias" {
            tracing::debug!(
                alias = %redact_text_label(token.as_str()),
                "别名裁决幂等返回（已是合并状态）"
            );
            return Ok(AliasResolveOutcome {
                alias: token.as_str().to_string(),
                canonical_keyword: entry.canonical_keyword.clone(),
                status: "alias".to_string(),
                already_applied: true,
            });
        }
        let verb = if confirm { "确认合并" } else { "驳回" };
        return Err(RamariaError::validation(format!(
            "关键词 '{}' 当前状态为 {status}，不是待确认别名（pending），无法{verb}",
            req.alias
        )));
    }

    let changed = if confirm {
        storage.confirm_keyword_alias(entry.rowid).await?
    } else {
        storage.reject_keyword_alias(entry.rowid).await?
    };
    if !changed {
        return Err(RamariaError::validation(format!(
            "词条 '{}' 状态已变化（非待确认别名），操作未执行，请刷新后重试",
            req.alias
        )));
    }

    let action = if confirm { "confirm" } else { "reject" };
    tracing::debug!(
        alias = %redact_text_label(token.as_str()),
        action,
        "别名状态迁移完成"
    );

    Ok(AliasResolveOutcome {
        alias: token.as_str().to_string(),
        canonical_keyword: if confirm {
            entry.canonical_keyword.clone()
        } else {
            None
        },
        status: if confirm { "alias" } else { "canonical" }.to_string(),
        already_applied: false,
    })
}

// =========================================================
// 待确认别名建议（扫描使用量 → 相似词对 → 落库）
// =========================================================

/// 建议扫描的最小使用量阈值（低于此值不参与建议）。
///
/// 取值理由: 过滤仅出现 1-2 次的偶然用词——噪声大且归一收益低；
/// 3 次是"重复出现、值得归一"的最小观测信号。
pub const DEFAULT_SUGGEST_MIN_USE: u32 = 3;

/// 单次运行最多登记的待确认别名数（超出部分本轮不写，避免一次刷出大量条目）。
pub const MAX_PENDING_SUGGESTIONS_PER_RUN: usize = 50;

/// 词条本地状态（筛选与指针解析用；与 keyword_pool 三态一致）。
enum PoolSlot {
    /// 规范词（携带 rowid，作为待确认别名的指针）
    Canonical(i64),
    /// 已确认别名
    Alias,
    /// 待确认别名
    Pending,
}

/// 扫描关键词使用量，把相似词对登记为待确认别名（pending）。
///
/// 流程:
/// 1. 汇总使用量：keyword_pool 行计数与内存镜像计数按文本相加
///    （镜像为空 / 未装载时仅用词池计数）；
/// 2. 已建立词表登记：canonical 行注册为规范词、alias 行注册为已确认别名，
///    建议引擎据此跳过已归一 / 已裁决词条；
/// 3. 调用建议引擎，逐条筛选后落库：
///    - 别名与规范词文本相同、或别名已存在于词池（任意状态）→ 跳过；
///    - 建议目标已存在但不是规范词（alias / pending）→ 跳过（不做反向改指）；
///    - 建议目标不存在 → 幂等注入为规范词后再取其 rowid 作为指针；
///    - 单次运行最多登记 [`MAX_PENDING_SUGGESTIONS_PER_RUN`] 条，超出计入 `truncated`。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `min_use`: 最小使用量阈值（None 时取 [`DEFAULT_SUGGEST_MIN_USE`]）。
///
/// 返回:
/// - 建议结果（扫描 / 建议 / 登记 / 跳过 / 截断计数与汇总提示）。
///
/// 说明:
/// - 本用例为机会性生产：单条建议的注入 / 登记失败记 warn 并继续，不阻塞整体；
///   词池读取失败返回错误，由调用入口按 best-effort 处理。
/// - 日志只记录计数与阈值，不记录词条文本。
pub(crate) async fn suggest_pending_aliases(
    engine: &Engine,
    min_use: Option<u32>,
) -> RamariaResult<KeywordSuggestionOutcome> {
    let min_use = min_use.unwrap_or(DEFAULT_SUGGEST_MIN_USE);
    let storage = engine.storage_ref();

    // ---- 1. 使用量汇总：词池行计数 + 内存镜像计数 ----
    let entries = storage.list_keyword_pool_entries().await?;
    let mut use_counts: HashMap<String, u32> = HashMap::with_capacity(entries.len());
    for entry in &entries {
        use_counts.insert(entry.keyword.clone(), count_to_u32(entry.use_count));
    }
    {
        let mirror = engine.keyword_mirror();
        let guard = read_recover(&*mirror, "keyword.suggest.mirror");
        for entry in guard.pool().iter() {
            let slot = use_counts
                .entry(entry.token.as_str().to_string())
                .or_insert(0);
            *slot = slot.saturating_add(count_to_u32(entry.use_count));
        }
    }

    // ---- 2. 已建立词表登记（pending 未确认，不登记） ----
    let mut manager = AliasManager::new();
    for entry in &entries {
        match status_of(&entry.alias_status) {
            "canonical" => {
                if let Err(e) = manager.register_canonical(&entry.keyword, entry.rowid) {
                    tracing::debug!(error = %e, "规范词登记跳过（文本非法）");
                }
            }
            "alias" => match (entry.canonical_id, entry.canonical_keyword.as_deref()) {
                (Some(canonical_id), Some(canonical_text)) => {
                    if let Err(e) =
                        manager.register_alias(&entry.keyword, canonical_id, canonical_text)
                    {
                        tracing::debug!(error = %e, "别名登记跳过（文本非法）");
                    }
                }
                _ => tracing::debug!(rowid = entry.rowid, "别名行缺规范词指向，跳过登记"),
            },
            _ => {}
        }
    }

    let scanned_tokens = use_counts.len();
    manager.load_use_counts(use_counts);
    let suggestions = manager.suggest_merges(min_use);

    // ---- 3. 筛选与落库（本地状态随登记实时更新） ----
    let mut states: HashMap<String, PoolSlot> = HashMap::with_capacity(entries.len());
    for entry in &entries {
        let slot = match status_of(&entry.alias_status) {
            "canonical" => PoolSlot::Canonical(entry.rowid),
            "alias" => PoolSlot::Alias,
            _ => PoolSlot::Pending,
        };
        states.insert(entry.keyword.clone(), slot);
    }

    let mut inserted = 0usize;
    let mut skipped = 0usize;
    let mut truncated = 0usize;
    let mut attempted = 0usize;

    for suggestion in &suggestions {
        if suggestion.alias_text == suggestion.canonical_text {
            skipped += 1;
            continue;
        }
        if states.contains_key(&suggestion.alias_text) {
            skipped += 1;
            continue;
        }
        if attempted >= MAX_PENDING_SUGGESTIONS_PER_RUN {
            truncated += 1;
            continue;
        }
        let canonical_id = match states.get(&suggestion.canonical_text) {
            Some(PoolSlot::Canonical(rowid)) => *rowid,
            Some(_) => {
                skipped += 1;
                continue;
            }
            None => {
                if let Err(e) = storage
                    .seed_keyword_canonical(&suggestion.canonical_text)
                    .await
                {
                    tracing::warn!(error = %e, "规范词注入失败，跳过该条合并建议");
                    skipped += 1;
                    continue;
                }
                match canonical_rowid(storage.as_ref(), &suggestion.canonical_text).await {
                    Ok(Some(rowid)) => {
                        states.insert(
                            suggestion.canonical_text.clone(),
                            PoolSlot::Canonical(rowid),
                        );
                        rowid
                    }
                    Ok(None) => {
                        skipped += 1;
                        continue;
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "规范词 rowid 查询失败，跳过该条合并建议");
                        skipped += 1;
                        continue;
                    }
                }
            }
        };

        attempted += 1;
        let use_count = suggestion.alias_use_count.max(1);
        match storage
            .upsert_pending_alias(&suggestion.alias_text, canonical_id, use_count)
            .await
        {
            Ok(true) => {
                inserted += 1;
                states.insert(suggestion.alias_text.clone(), PoolSlot::Pending);
            }
            // 并发 / 竞态：词条在筛选后出现，保持现状不覆盖
            Ok(false) => skipped += 1,
            Err(e) => {
                tracing::warn!(error = %e, "待确认别名登记失败，跳过该条合并建议");
                skipped += 1;
            }
        }
    }

    let message = format!(
        "扫描 {scanned_tokens} 个词条，产生 {} 条合并建议：新增 {inserted} 条待确认别名，跳过 {skipped} 条，{truncated} 条因超过单次上限未处理",
        suggestions.len()
    );
    tracing::info!(
        scanned = scanned_tokens,
        suggestions = suggestions.len(),
        inserted,
        skipped,
        truncated,
        min_use,
        "关键词别名建议完成"
    );

    Ok(KeywordSuggestionOutcome {
        scanned_tokens,
        suggestions: suggestions.len(),
        inserted,
        skipped,
        truncated,
        message,
    })
}

/// 重新读取词池行，解析指定文本的规范词 rowid（存在但不是规范词 → None）。
async fn canonical_rowid(
    storage: &dyn StorageBackend,
    keyword: &str,
) -> RamariaResult<Option<i64>> {
    let entries = storage.list_keyword_pool_entries().await?;
    Ok(entries
        .into_iter()
        .find(|entry| entry.keyword == keyword && status_of(&entry.alias_status) == "canonical")
        .map(|entry| entry.rowid))
}

/// i64 计数转换到 u32（负值按 0、溢出饱和）。
fn count_to_u32(value: i64) -> u32 {
    value.clamp(0, i64::from(u32::MAX)) as u32
}

// =========================================================
// 日志脱敏（文件内私有）
// =========================================================

/// 生成文本脱敏标签：`<N chars>#<8 位十六进制哈希>`。
///
/// 用途:
/// - 日志记录别名等用户文本时仅保留长度与哈希，正文不出现在日志中。
fn redact_text_label(text: &str) -> String {
    format!(
        "<{} chars>#{:08x}",
        text.chars().count(),
        fnv1a32(text.as_bytes())
    )
}

/// 64 位 FNV-1a 哈希取低 32 位（仅用于日志标签，不用于安全用途）。
fn fnv1a32(bytes: &[u8]) -> u32 {
    const OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;
    const PRIME: u64 = 0x0000_0100_0000_01b3;

    let mut hash = OFFSET_BASIS;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    (hash & 0xffff_ffff) as u32
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
