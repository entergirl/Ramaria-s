//! crates/ramaria-cli/src/commands/keyword_cmd.rs - 关键词词典管理命令
//!
//! 设计特点:
//! - 子命令词表：list / show / seed / alias list|confirm|reject
//! - list/show/alias list: 只读查询 keyword_pool（repo::keyword 行视图，不写裸 SQL）
//! - seed: 幂等手工注入规范词（use_count 从 0 起，重复执行不改 use_count/别名状态），
//!   供词典增强分词（keyword_pool 规范词 → 分词词典）消费
//! - alias confirm/reject: 写操作，遵循确认惯例（--yes 自动通过；非 TTY 无 --yes 直接失败）；
//!   不存在 / 非 pending 均报业务校验错误（exit 4）
//! - 全部支持全局 `--json` 信封；stdout 只输出数据；日志/提示不记录关键词原文（仅 rowid/count）

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::keyword::KeywordToken;
use ramaria_storage::repo::keyword::{self as kw_repo, KeywordEntryRow};
use sqlx::SqlitePool;

use crate::json;

// =========================================================
// 公共枚举与入口
// =========================================================

/// Keyword 子命令（clap 镜像枚举见 main.rs，dispatch 时转换为本模块类型）。
#[derive(Debug, Clone)]
pub enum KeywordCmd {
    /// 列出 keyword_pool 全部词条
    List,
    /// 查看单个词条详情
    Show {
        /// 关键词文本
        keyword: String,
    },
    /// 手工注入规范词（幂等）
    Seed {
        /// 待注入的规范词文本列表
        keywords: Vec<String>,
    },
    /// 待确认别名管理
    Alias(AliasAction),
}

/// keyword alias 子命令。
#[derive(Debug, Clone)]
pub enum AliasAction {
    /// 列出待确认别名冲突
    List,
    /// 确认别名合并（pending → alias）
    Confirm {
        /// 别名文本
        alias: String,
    },
    /// 驳回别名（晋升独立规范词）
    Reject {
        /// 别名文本
        alias: String,
    },
}

/// 运行 keyword 子命令分发。
///
/// 参数:
/// - `pool`: SQLite 连接池（keyword_pool 直接经 repo::keyword 访问）。
/// - `cmd`: Keyword 子命令。
/// - `json`: JSON 信封输出。
/// - `yes`: 自动确认所有确认点（alias confirm/reject 等）。
pub async fn run(pool: &SqlitePool, cmd: KeywordCmd, json: bool, yes: bool) -> anyhow::Result<()> {
    match cmd {
        KeywordCmd::List => run_list(pool, json).await,
        KeywordCmd::Show { keyword } => run_show(pool, &keyword, json).await,
        KeywordCmd::Seed { keywords } => run_seed(pool, &keywords, json).await,
        KeywordCmd::Alias(AliasAction::List) => run_alias_list(pool, json).await,
        KeywordCmd::Alias(AliasAction::Confirm { alias }) => {
            run_alias_resolve(pool, &alias, json, yes, true).await
        }
        KeywordCmd::Alias(AliasAction::Reject { alias }) => {
            run_alias_resolve(pool, &alias, json, yes, false).await
        }
    }
}

// =========================================================
// 行视图辅助
// =========================================================

/// 词条状态文本（keyword_pool 口径: canonical/alias/pending）。
fn status_of(row: &KeywordEntryRow) -> &'static str {
    match row.alias_status.as_deref() {
        Some("alias") => "alias",
        Some("pending") => "pending",
        _ => "canonical",
    }
}

/// 将原始关键词文本解析为标准化 token。
///
/// 返回业务校验错误（空 / 纯空白 / 超长，exit 4）。
fn parse_keyword(raw: &str) -> anyhow::Result<KeywordToken> {
    KeywordToken::new(raw).ok_or_else(|| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "无效关键词: '{raw}'（需非空且不超过 256 字符）"
        )))
    })
}

/// 从全量行视图中定位单个词条（keyword 已标准化比较）。
async fn find_entry(
    pool: &SqlitePool,
    token: &KeywordToken,
) -> anyhow::Result<Option<KeywordEntryRow>> {
    let entries = kw_repo::list_entries(pool)
        .await
        .context("查询关键词词条失败")?;
    Ok(entries.into_iter().find(|e| e.keyword == token.as_str()))
}

// =========================================================
// list
// =========================================================

/// 列出 keyword_pool 全部词条（含状态与指向的规范词文本）。
async fn run_list(pool: &SqlitePool, json: bool) -> anyhow::Result<()> {
    let entries = kw_repo::list_entries(pool)
        .await
        .context("查询关键词列表失败")?;

    if json {
        let keywords: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "keyword": e.keyword,
                    "use_count": e.use_count,
                    "status": status_of(e),
                    "canonical_keyword": e.canonical_keyword,
                })
            })
            .collect();
        let data = serde_json::json!({
            "total": entries.len(),
            "keywords": keywords,
        });
        return json::emit_ok(&data);
    }

    if entries.is_empty() {
        crate::ui::info("keyword_pool 暂无词条（可用 ramaria keyword seed 手工注入）");
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("词条数", &entries.len().to_string());
    crate::ui::separator();
    for e in &entries {
        let status = status_of(e);
        let arrow = match (&e.canonical_keyword, status) {
            (Some(canonical), "alias" | "pending") => format!("  → {canonical}"),
            _ => String::new(),
        };
        println!("{:<20} {:>4}  {:<8}{arrow}", e.keyword, e.use_count, status);
    }
    Ok(())
}

// =========================================================
// show
// =========================================================

/// 查看单个词条详情。
async fn run_show(pool: &SqlitePool, keyword: &str, json: bool) -> anyhow::Result<()> {
    let token = parse_keyword(keyword)?;
    let entry = find_entry(pool, &token).await?.ok_or_else(|| {
        // 业务校验失败：词条不存在（exit 4）
        anyhow::anyhow!(RamariaError::validation(format!(
            "关键词 '{keyword}' 不存在（keyword_pool 无此词条，可用 ramaria keyword seed 注入）"
        )))
    })?;

    if json {
        let data = serde_json::json!({
            "keyword": entry.keyword,
            "use_count": entry.use_count,
            "status": status_of(&entry),
            "canonical_id": entry.canonical_id,
            "canonical_keyword": entry.canonical_keyword,
            "created_at": crate::util::format_timestamp_iso(entry.created_at),
        });
        return json::emit_ok(&data);
    }

    crate::ui::separator();
    crate::ui::labeled("关键词", &entry.keyword);
    crate::ui::labeled("使用次数", &entry.use_count.to_string());
    crate::ui::labeled("状态", status_of(&entry));
    if let Some(canonical) = &entry.canonical_keyword {
        crate::ui::labeled("规范词", canonical);
    }
    if let Some(ts) = crate::util::format_timestamp(entry.created_at) {
        crate::ui::labeled("登记时间", &ts);
    }
    crate::ui::separator();
    Ok(())
}

// =========================================================
// seed
// =========================================================

/// 手工注入规范词（幂等：已存在保持现状，不递增 use_count、不改别名状态）。
async fn run_seed(pool: &SqlitePool, keywords: &[String], json: bool) -> anyhow::Result<()> {
    // 先整体解析校验（任一无效即报错，不部分写入）
    let mut tokens: Vec<KeywordToken> = Vec::with_capacity(keywords.len());
    for raw in keywords {
        tokens.push(parse_keyword(raw)?);
    }
    // 去重（保留首次出现顺序；重复注入同一词条只计一次）
    let mut seen: Vec<String> = Vec::with_capacity(tokens.len());
    tokens.retain(|t| {
        let s = t.as_str().to_string();
        if seen.contains(&s) {
            false
        } else {
            seen.push(s);
            true
        }
    });

    // 先取全量行视图，判断新词 / 已存在；新词走 seed_canonical（不存在才插入）
    let entries = kw_repo::list_entries(pool)
        .await
        .context("查询关键词词条失败")?;
    let mut results: Vec<serde_json::Value> = Vec::with_capacity(tokens.len());

    for token in &tokens {
        let keyword = token.as_str();
        if let Some(existing) = entries.iter().find(|e| e.keyword == keyword) {
            let status = status_of(existing);
            let inserted = false;
            results.push(serde_json::json!({
                "keyword": keyword,
                "inserted": inserted,
                "status": status,
            }));
            if !json {
                match status {
                    "alias" | "pending" => crate::ui::info(&format!(
                        "词条 '{keyword}' 已是{status}（指向 {}），保持现状；可用 ramaria keyword alias confirm/reject 处理",
                        existing.canonical_keyword.as_deref().unwrap_or("其他词")
                    )),
                    _ => crate::ui::info(&format!(
                        "词条 '{keyword}' 已存在且为规范词，保持现状（幂等，不递增 use_count）"
                    )),
                }
            }
        } else {
            let inserted = kw_repo::seed_canonical(pool, token)
                .await
                .context("种子注入关键词失败")?;
            results.push(serde_json::json!({
                "keyword": keyword,
                "inserted": inserted,
                "status": "canonical",
            }));
            if !json {
                crate::ui::success(&format!(
                    "已注入规范词 '{keyword}'（use_count 0，供词典增强分词）"
                ));
            }
        }
    }

    if json {
        let inserted_count = results
            .iter()
            .filter(|r| r["inserted"].as_bool() == Some(true))
            .count();
        let data = serde_json::json!({
            "seeded": inserted_count,
            "skipped": results.len() - inserted_count,
            "results": results,
        });
        return json::emit_ok(&data);
    }
    Ok(())
}

// =========================================================
// alias list / confirm / reject
// =========================================================

/// 列出待确认别名冲突（alias → 建议规范词）。
async fn run_alias_list(pool: &SqlitePool, json: bool) -> anyhow::Result<()> {
    let pending = kw_repo::list_pending_aliases(pool)
        .await
        .context("查询待确认别名失败")?;

    if json {
        let aliases: Vec<serde_json::Value> = pending
            .iter()
            .map(|p| {
                serde_json::json!({
                    "alias": p.alias_keyword,
                    "canonical": p.canonical_keyword,
                    "created_at": crate::util::format_timestamp_iso(p.created_at),
                })
            })
            .collect();
        let data = serde_json::json!({
            "total": pending.len(),
            "aliases": aliases,
        });
        return json::emit_ok(&data);
    }

    if pending.is_empty() {
        crate::ui::info("暂无待确认别名冲突");
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("待确认别名", &pending.len().to_string());
    crate::ui::separator();
    for p in &pending {
        println!("{}  →  {}", p.alias_keyword, p.canonical_keyword);
    }
    Ok(())
}

/// 确认（confirm=true）/ 驳回（confirm=false）单个待确认别名。
///
/// 写操作确认规则: `--yes` 自动通过；非 TTY 且无 `--yes` 直接失败不挂起（exit 4）；
/// 用户主动取消不写库（ok:true + cancelled，exit 0）。
async fn run_alias_resolve(
    pool: &SqlitePool,
    alias: &str,
    json: bool,
    yes: bool,
    confirm: bool,
) -> anyhow::Result<()> {
    let token = parse_keyword(alias)?;
    let rowid = kw_repo::find_rowid(pool, token.as_str())
        .await
        .context("查询词条 rowid 失败")?
        .ok_or_else(|| {
            // 业务校验失败：词条不存在（exit 4）
            anyhow::anyhow!(RamariaError::validation(format!(
                "关键词 '{alias}' 不存在（keyword_pool 无此词条，无法{}）",
                if confirm {
                    "确认别名"
                } else {
                    "驳回别名"
                }
            )))
        })?;

    // 预取词条现状：非 pending 直接报业务错误；pending 则取规范词文本用于提示
    let entry = find_entry(pool, &token).await?.ok_or_else(|| {
        anyhow::anyhow!(RamariaError::validation(format!("关键词 '{alias}' 不存在")))
    })?;
    if status_of(&entry) != "pending" {
        return Err(anyhow::anyhow!(RamariaError::validation(format!(
            "关键词 '{alias}' 当前状态为 {}，不是待确认别名（pending），无法{}；请先用 alias list 查看待确认冲突",
            status_of(&entry),
            if confirm { "确认合并" } else { "驳回" }
        ))));
    }
    let canonical_text = entry
        .canonical_keyword
        .as_deref()
        .unwrap_or("（规范词缺失）")
        .to_string();

    let prompt = if confirm {
        format!("将别名 '{alias}' 合并到规范词 '{canonical_text}'？此操作不可撤销")
    } else {
        format!("将别名 '{alias}'（当前指向 '{canonical_text}'）驳回为独立规范词？此操作不可撤销")
    };
    let confirmed =
        crate::ui::confirm(&prompt, yes).map_err(|e| RamariaError::validation(e.to_string()))?;
    if !confirmed {
        if json {
            // 用户主动取消：非错误（ok:true + cancelled 标志，exit 0）
            let data = serde_json::json!({ "alias": alias, "cancelled": true });
            return json::emit_ok(&data);
        }
        crate::ui::info("已取消");
        return Ok(());
    }

    // 执行迁移（rowid 定位后调 repo 状态机；返回 false 表示现状已变化）
    let changed = if confirm {
        kw_repo::confirm_alias(pool, rowid)
            .await
            .context("确认别名失败")?
    } else {
        kw_repo::reject_alias(pool, rowid)
            .await
            .context("驳回别名失败")?
    };
    if !changed {
        return Err(anyhow::anyhow!(RamariaError::validation(format!(
            "词条 '{alias}' 状态已变化（非待确认别名），操作未执行，请重新查看"
        ))));
    }

    let new_status = if confirm { "alias" } else { "canonical" };
    if json {
        // canonical_text 仅在 confirm 后仍指向规范词；reject 后该词条已晋升，规范词置 null
        let canonical_json = if confirm {
            serde_json::Value::String(canonical_text)
        } else {
            serde_json::Value::Null
        };
        let data = serde_json::json!({
            "alias": alias,
            "canonical_keyword": canonical_json,
            "status": new_status,
        });
        return json::emit_ok(&data);
    }
    if confirm {
        crate::ui::success(&format!(
            "别名 '{alias}' 已确认合并到规范词 '{canonical_text}'"
        ));
    } else {
        crate::ui::success(&format!("别名 '{alias}' 已驳回，晋升为独立规范词"));
    }
    Ok(())
}
