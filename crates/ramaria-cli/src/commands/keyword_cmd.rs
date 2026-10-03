//! crates/ramaria-cli/src/commands/keyword_cmd.rs - 关键词词典管理命令
//!
//! 设计特点:
//! - 子命令词表：list / show / seed / alias list|confirm|reject
//! - list/show/alias list: 经服务层关键词用例查询 keyword_pool（三态视图）
//! - seed: 幂等手工注入规范词（use_count 从 0 起，重复执行不改 use_count/别名状态），
//!   供词典增强分词（keyword_pool 规范词 → 分词词典）消费
//! - alias list: 输出前 best-effort 触发建议生成（扫描使用量 → 相似词对落库），
//!   生成失败仅提示，不影响列表输出与 `--json` 信封结构
//! - alias confirm/reject: 写操作，遵循确认惯例（--yes 自动通过；非 TTY 无 --yes 直接失败）；
//!   不存在 / 非 pending 均报业务校验错误（exit 4）；已合并（alias）再次 confirm 幂等成功
//! - 全部支持全局 `--json` 信封；stdout 只输出数据；日志/提示不记录关键词原文（仅 rowid/count）

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::keyword::KeywordToken;
use ramaria_service::{
    AliasAction as ServiceAliasAction, AliasResolveRequest, Engine, KeywordEntryView,
};
use std::sync::Arc;

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
/// - `engine`: 服务层引擎引用。
/// - `cmd`: Keyword 子命令。
/// - `json`: JSON 信封输出。
/// - `yes`: 自动确认所有确认点（alias confirm/reject 等）。
pub async fn run(
    engine: &Arc<Engine>,
    cmd: KeywordCmd,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    match cmd {
        KeywordCmd::List => run_list(engine, json).await,
        KeywordCmd::Show { keyword } => run_show(engine, &keyword, json).await,
        KeywordCmd::Seed { keywords } => run_seed(engine, &keywords, json).await,
        KeywordCmd::Alias(AliasAction::List) => run_alias_list(engine, json).await,
        KeywordCmd::Alias(AliasAction::Confirm { alias }) => {
            run_alias_resolve(engine, &alias, json, yes, true).await
        }
        KeywordCmd::Alias(AliasAction::Reject { alias }) => {
            run_alias_resolve(engine, &alias, json, yes, false).await
        }
    }
}

// =========================================================
// 行视图辅助
// =========================================================

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

/// 从全量词条视图中定位单个词条（keyword 已标准化比较）。
fn find_entry<'a>(
    entries: &'a [KeywordEntryView],
    token: &KeywordToken,
) -> Option<&'a KeywordEntryView> {
    entries.iter().find(|e| e.keyword == token.as_str())
}

// =========================================================
// list
// =========================================================

/// 列出 keyword_pool 全部词条（含状态与指向的规范词文本）。
async fn run_list(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    let view = engine.keyword_list().await.context("查询关键词列表失败")?;
    let entries = &view.keywords;

    if json {
        let keywords: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "keyword": e.keyword,
                    "use_count": e.use_count,
                    "status": e.status,
                    "canonical_keyword": e.canonical_keyword,
                })
            })
            .collect();
        let data = serde_json::json!({
            "total": view.total,
            "keywords": keywords,
        });
        return json::emit_ok(&data);
    }

    if entries.is_empty() {
        crate::ui::info("keyword_pool 暂无词条（可用 ramaria keyword seed 手工注入）");
        return Ok(());
    }
    crate::ui::separator();
    crate::ui::labeled("词条数", &view.total.to_string());
    crate::ui::separator();
    for e in entries {
        let status = e.status.as_str();
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
async fn run_show(engine: &Arc<Engine>, keyword: &str, json: bool) -> anyhow::Result<()> {
    let token = parse_keyword(keyword)?;
    let view = engine.keyword_list().await.context("查询关键词词条失败")?;
    let entry = find_entry(&view.keywords, &token).ok_or_else(|| {
        // 业务校验失败：词条不存在（exit 4）
        anyhow::anyhow!(RamariaError::validation(format!(
            "关键词 '{keyword}' 不存在（keyword_pool 无此词条，可用 ramaria keyword seed 注入）"
        )))
    })?;

    if json {
        let data = serde_json::json!({
            "keyword": entry.keyword,
            "use_count": entry.use_count,
            "status": entry.status,
            "canonical_id": entry.canonical_id,
            "canonical_keyword": entry.canonical_keyword,
            "created_at": crate::util::format_timestamp_iso(entry.created_at),
        });
        return json::emit_ok(&data);
    }

    crate::ui::separator();
    crate::ui::labeled("关键词", &entry.keyword);
    crate::ui::labeled("使用次数", &entry.use_count.to_string());
    crate::ui::labeled("状态", &entry.status);
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
async fn run_seed(engine: &Arc<Engine>, keywords: &[String], json: bool) -> anyhow::Result<()> {
    // 预取词条视图：已存在词条的规范词指向用于保持现状提示
    let before = engine.keyword_list().await.context("查询关键词词条失败")?;

    // 整体校验（任一无效即报错、不部分写入）与去重由服务层用例执行
    let outcome = engine
        .keyword_seed(keywords)
        .await
        .context("种子注入关键词失败")?;

    for item in &outcome.results {
        if item.inserted {
            if !json {
                crate::ui::success(&format!(
                    "已注入规范词 '{}'（use_count 0，供词典增强分词）",
                    item.keyword
                ));
            }
            continue;
        }
        if !json {
            match item.status.as_str() {
                "alias" | "pending" => {
                    let canonical = before
                        .keywords
                        .iter()
                        .find(|e| e.keyword == item.keyword)
                        .and_then(|e| e.canonical_keyword.as_deref())
                        .unwrap_or("其他词");
                    crate::ui::info(&format!(
                        "词条 '{}' 已是{}（指向 {canonical}），保持现状；可用 ramaria keyword alias confirm/reject 处理",
                        item.keyword, item.status
                    ));
                }
                _ => crate::ui::info(&format!(
                    "词条 '{}' 已存在且为规范词，保持现状（幂等，不递增 use_count）",
                    item.keyword
                )),
            }
        }
    }

    if json {
        let data = serde_json::json!({
            "seeded": outcome.seeded,
            "skipped": outcome.skipped,
            "results": outcome.results,
        });
        return json::emit_ok(&data);
    }
    Ok(())
}

// =========================================================
// alias list / confirm / reject
// =========================================================

/// 列出待确认别名冲突（alias → 建议规范词）。
///
/// 说明:
/// - 列输出前先 best-effort 扫描词池与内存镜像使用量生成待确认项；
///   生成失败只提示，不影响列表输出。
async fn run_alias_list(engine: &Arc<Engine>, json: bool) -> anyhow::Result<()> {
    if let Err(e) = engine.keyword_suggest_pending_aliases(None).await {
        crate::ui::warn(&format!("待确认别名生成失败（忽略）：{e}"));
    }

    let pending = engine
        .keyword_pending_aliases()
        .await
        .context("查询待确认别名失败")?;

    if json {
        let aliases: Vec<serde_json::Value> = pending
            .iter()
            .map(|p| {
                serde_json::json!({
                    "alias": p.alias,
                    "canonical": p.canonical,
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
        println!("{}  →  {}", p.alias, p.canonical);
    }
    Ok(())
}

/// 确认（confirm=true）/ 驳回（confirm=false）单个待确认别名。
///
/// 写操作确认规则: `--yes` 自动通过；非 TTY 且无 `--yes` 直接失败不挂起（exit 4）；
/// 用户主动取消不写库（ok:true + cancelled，exit 0）。
/// confirm 且词条已是合并状态（alias）为幂等成功：不弹确认、不写库。
async fn run_alias_resolve(
    engine: &Arc<Engine>,
    alias: &str,
    json: bool,
    yes: bool,
    confirm: bool,
) -> anyhow::Result<()> {
    let token = parse_keyword(alias)?;

    // 预取词条现状：
    // - confirm 且已是 alias（已合并）→ 透传服务层做幂等成功（不写库、不弹确认）；
    // - 其余非 pending → 业务错误（exit 4）。
    let view = engine.keyword_list().await.context("查询关键词词条失败")?;
    let entry = find_entry(&view.keywords, &token).ok_or_else(|| {
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
    let status = entry.status.as_str();
    let already_merged = confirm && status == "alias";
    if status != "pending" && !already_merged {
        return Err(anyhow::anyhow!(RamariaError::validation(format!(
            "关键词 '{alias}' 当前状态为 {status}，不是待确认别名（pending），无法{}；请先用 alias list 查看待确认冲突",
            if confirm { "确认合并" } else { "驳回" }
        ))));
    }
    let canonical_text = entry
        .canonical_keyword
        .as_deref()
        .unwrap_or("（规范词缺失）")
        .to_string();

    if !already_merged {
        let prompt = if confirm {
            format!("将别名 '{alias}' 合并到规范词 '{canonical_text}'？此操作不可撤销")
        } else {
            format!(
                "将别名 '{alias}'（当前指向 '{canonical_text}'）驳回为独立规范词？此操作不可撤销"
            )
        };
        let confirmed = crate::ui::confirm(&prompt, yes)
            .map_err(|e| RamariaError::validation(e.to_string()))?;
        if !confirmed {
            if json {
                // 用户主动取消：非错误（ok:true + cancelled 标志，exit 0）
                let data = serde_json::json!({ "alias": alias, "cancelled": true });
                return json::emit_ok(&data);
            }
            crate::ui::info("已取消");
            return Ok(());
        }
    }

    // 执行状态迁移（服务层裁决用例；已合并再次确认为幂等成功，不写库）
    let outcome = engine
        .keyword_resolve_alias(AliasResolveRequest {
            alias: alias.to_string(),
            action: if confirm {
                ServiceAliasAction::Confirm
            } else {
                ServiceAliasAction::Reject
            },
        })
        .await
        .map_err(|e| map_resolve_error(e, alias))?;

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
            "already_applied": outcome.already_applied,
        });
        return json::emit_ok(&data);
    }
    if confirm {
        if outcome.already_applied {
            crate::ui::info(&format!(
                "别名 '{alias}' 已是合并状态（→ 规范词 '{canonical_text}'），无需重复确认"
            ));
        } else {
            crate::ui::success(&format!(
                "别名 '{alias}' 已确认合并到规范词 '{canonical_text}'"
            ));
        }
    } else {
        crate::ui::success(&format!("别名 '{alias}' 已驳回，晋升为独立规范词"));
    }
    Ok(())
}

/// 将别名裁决的竞态错误重映射为入口既有文案（条件更新未命中 = 状态已变化）。
fn map_resolve_error(err: RamariaError, alias: &str) -> anyhow::Error {
    if err.category() == "validation" && err.to_string().contains("状态已变化") {
        return anyhow::anyhow!(RamariaError::validation(format!(
            "词条 '{alias}' 状态已变化（非待确认别名），操作未执行，请重新查看"
        )));
    }
    anyhow::anyhow!(err)
}
