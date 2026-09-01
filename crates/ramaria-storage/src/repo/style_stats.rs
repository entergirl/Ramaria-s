//! crates/ramaria-storage/src/repo/style_stats.rs - persona_style_stats 存取模块
//!
//! 设计特点:
//! - 按 persona 单行 upsert（主键 persona_uid），幂等写入（INSERT OR REPLACE）
//! - stats_json 为五维统计参数 JSON（不含原文），本模块只做序列化透传
//! - rule_source / status 枚举解析失败时回退默认值并记录 WARNING
//! - 与 facts 等 repo 一致：所有可恢复错误统一转换为 RamariaError::Storage

use crate::repo::StorageResultExt;
use ramaria_core::error::RamariaResult;
use ramaria_core::types::{PersonaStyleStats, StyleRuleSource, StyleStatsStatus};
use sqlx::SqlitePool;

parse_enum_fallback!(
    parse_rule_source, StyleRuleSource, StyleRuleSource::None, "persona_style_stats",
    "rule_source",
    "none"      => None,
    "template"  => Template,
    "llm"       => Llm,
);
parse_enum_fallback!(
    parse_status, StyleStatsStatus, StyleStatsStatus::Insufficient, "persona_style_stats",
    "status",
    "insufficient"   => Insufficient,
    "ready"          => Ready,
    "no_significant" => NoSignificant,
);

/// 按 persona 单行 upsert 风格统计（幂等）。
///
/// 参数:
/// - `stats`: 风格统计记录（persona_uid 为主键，重复写入覆盖旧值）。
///
/// 返回:
/// - 成功时返回 `Ok(())`。
pub async fn upsert(pool: &SqlitePool, stats: &PersonaStyleStats) -> RamariaResult<()> {
    sqlx::query(
        "INSERT OR REPLACE INTO persona_style_stats \
             (persona_uid, sample_count, stats_json, baseline_version, rule_text, \
              rule_source, status, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&stats.persona_uid)
    .bind(stats.sample_count)
    .bind(&stats.stats_json)
    .bind(stats.baseline_version)
    .bind(stats.rule_text.as_deref())
    .bind(stats.rule_source.as_str())
    .bind(stats.status.as_str())
    .bind(stats.updated_at)
    .execute(pool)
    .await
    .storage_err("保存风格统计失败")?;
    Ok(())
}

/// 按 persona 查询风格统计（注入侧读取规则文本 / 状态判断）。
pub async fn get(pool: &SqlitePool, persona_uid: &str) -> RamariaResult<Option<PersonaStyleStats>> {
    let row = sqlx::query_as::<_, StyleStatsRow>(
        "SELECT persona_uid, sample_count, stats_json, baseline_version, rule_text, \
             rule_source, status, updated_at
         FROM persona_style_stats WHERE persona_uid = ?",
    )
    .bind(persona_uid)
    .fetch_optional(pool)
    .await
    .storage_err("查询风格统计失败")?;
    row.map(StyleStatsRow::into_stats).transpose()
}

/// 查询全部风格统计（基线池更新 / CLI 诊断）。
pub async fn list_all(pool: &SqlitePool) -> RamariaResult<Vec<PersonaStyleStats>> {
    let rows = sqlx::query_as::<_, StyleStatsRow>(
        "SELECT persona_uid, sample_count, stats_json, baseline_version, rule_text, \
             rule_source, status, updated_at
         FROM persona_style_stats ORDER BY updated_at DESC",
    )
    .fetch_all(pool)
    .await
    .storage_err("查询风格统计列表失败")?;
    rows.into_iter().map(StyleStatsRow::into_stats).collect()
}

#[derive(sqlx::FromRow)]
struct StyleStatsRow {
    persona_uid: String,
    sample_count: i64,
    stats_json: String,
    baseline_version: i64,
    rule_text: Option<String>,
    rule_source: String,
    status: String,
    updated_at: i64,
}

impl StyleStatsRow {
    fn into_stats(self) -> RamariaResult<PersonaStyleStats> {
        Ok(PersonaStyleStats {
            persona_uid: self.persona_uid,
            sample_count: u32::try_from(self.sample_count.max(0)).unwrap_or(u32::MAX),
            stats_json: self.stats_json,
            baseline_version: u32::try_from(self.baseline_version.max(0)).unwrap_or(u32::MAX),
            rule_text: self.rule_text,
            rule_source: parse_rule_source(&self.rule_source),
            status: parse_status(&self.status),
            updated_at: self.updated_at,
        })
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use ramaria_core::types::{StyleRuleSource, StyleStatsStatus};

    #[test]
    fn enum_parse_fallback_on_invalid_value() {
        // 非法 DB 值回退默认（None / Insufficient），不产生解析错误
        assert_eq!(parse_rule_source("unknown"), StyleRuleSource::None);
        assert_eq!(parse_status("unknown"), StyleStatsStatus::Insufficient);
    }

    #[test]
    fn enum_parse_known_values() {
        assert_eq!(parse_rule_source("template"), StyleRuleSource::Template);
        assert_eq!(parse_rule_source("llm"), StyleRuleSource::Llm);
        assert_eq!(parse_status("ready"), StyleStatsStatus::Ready);
        assert_eq!(
            parse_status("no_significant"),
            StyleStatsStatus::NoSignificant
        );
    }

    #[test]
    fn as_str_roundtrip_matches_db_values() {
        for src in [
            StyleRuleSource::None,
            StyleRuleSource::Template,
            StyleRuleSource::Llm,
        ] {
            assert_eq!(parse_rule_source(src.as_str()), src);
        }
        for st in [
            StyleStatsStatus::Insufficient,
            StyleStatsStatus::Ready,
            StyleStatsStatus::NoSignificant,
        ] {
            assert_eq!(parse_status(st.as_str()), st);
        }
    }

    #[test]
    fn new_stats_defaults_are_consistent() {
        let stats = PersonaStyleStats::new(
            "char-0001".to_string(),
            5,
            "{}".to_string(),
            0,
            None,
            StyleRuleSource::None,
            StyleStatsStatus::Insufficient,
        );
        assert_eq!(stats.persona_uid, "char-0001");
        assert_eq!(stats.sample_count, 5);
        assert_eq!(stats.status, StyleStatsStatus::Insufficient);
        assert!(stats.updated_at > 0);
    }
}
