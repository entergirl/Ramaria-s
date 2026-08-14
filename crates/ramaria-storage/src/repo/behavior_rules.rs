//! crates/ramaria-storage/src/repo/behavior_rules.rs - 行为规则 CRUD（v1.5 M5 D1）
//!
//! 设计特点:
//! - 管理 `behavior_rules` 表（算法说明书 v3.1 §4.1）
//! - situation / params / avoid / evidence 为 JSON 列，行映射时反序列化为强类型
//! - source 枚举解析失败时回退 Auto 并记录 WARNING（parse_enum_fallback）
//! - reaction 列可为 NULL（候选规则，仅参数注入）
//! - evidence 只存事件 id + 权重，原文不落规则表（隐私红线）

use crate::repo::StorageResultExt;
use ramaria_core::behavior::{
    BehaviorEvidence, BehaviorParams, BehaviorRule, BehaviorSituation, RuleSource,
};
use ramaria_core::error::{RamariaError, RamariaResult};
use sqlx::SqlitePool;

parse_enum_fallback!(
    parse_rule_source, RuleSource, RuleSource::Auto, "behavior_rules", "source",
    "auto"   => Auto,
    "manual" => Manual,
);

/// `behavior_rules` 表的一行（JSON 列原样读取，`into_rule` 解析）。
#[derive(sqlx::FromRow)]
struct RuleRow {
    id: i64,
    persona_uid: String,
    situation: String,
    reaction: Option<String>,
    params: String,
    avoid: String,
    evidence: String,
    confidence: f64,
    stability: f64,
    source: String,
    enabled: bool,
    created_at: i64,
    updated_at: i64,
}

impl RuleRow {
    /// 解析 JSON 列并组装为 `BehaviorRule`。
    ///
    /// 返回:
    /// - `Ok(rule)`: 解析成功。
    /// - `Err(Validation)`: situation/params 等必需 JSON 损坏（记录 warn 并传播错误，
    ///   由上层决定重试或跳过——不静默吞掉损坏数据）。
    fn into_rule(self) -> RamariaResult<BehaviorRule> {
        let situation: BehaviorSituation = serde_json::from_str(&self.situation).map_err(|e| {
            RamariaError::validation(format!(
                "behavior_rules 行 {} situation JSON 损坏: {}",
                self.id, e
            ))
        })?;
        let params: BehaviorParams = serde_json::from_str(&self.params).map_err(|e| {
            RamariaError::validation(format!(
                "behavior_rules 行 {} params JSON 损坏: {}",
                self.id, e
            ))
        })?;
        let avoid: Vec<String> = serde_json::from_str(&self.avoid).unwrap_or_else(|e| {
            tracing::warn!(
                rule_id = self.id,
                "behavior_rules.avoid JSON 损坏，回退空列表: {e}"
            );
            Vec::new()
        });
        let evidence: Vec<BehaviorEvidence> =
            serde_json::from_str(&self.evidence).unwrap_or_else(|e| {
                tracing::warn!(
                    rule_id = self.id,
                    "behavior_rules.evidence JSON 损坏，回退空列表: {e}"
                );
                Vec::new()
            });

        Ok(BehaviorRule {
            id: self.id,
            persona_uid: self.persona_uid,
            situation,
            reaction: self.reaction,
            params,
            avoid,
            evidence,
            confidence: self.confidence,
            stability: self.stability,
            source: parse_rule_source(&self.source),
            enabled: self.enabled,
            created_at: self.created_at,
            updated_at: self.updated_at,
        })
    }
}

// =========================================================
// 查询 / 写入
// =========================================================

/// 插入一条行为规则，返回自增 id。
pub async fn save(pool: &SqlitePool, rule: &BehaviorRule) -> RamariaResult<i64> {
    let situation = serde_json::to_string(&rule.situation)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior situation 失败: {e}")))?;
    let params = serde_json::to_string(&rule.params)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior params 失败: {e}")))?;
    let avoid = serde_json::to_string(&rule.avoid)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior avoid 失败: {e}")))?;
    let evidence = serde_json::to_string(&rule.evidence)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior evidence 失败: {e}")))?;

    sqlx::query_scalar::<_, i64>(
        "INSERT INTO behavior_rules
         (persona_uid, situation, reaction, params, avoid, evidence, confidence, stability, source, enabled, created_at, updated_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&rule.persona_uid)
    .bind(situation)
    .bind(&rule.reaction)
    .bind(params)
    .bind(avoid)
    .bind(evidence)
    .bind(rule.confidence)
    .bind(rule.stability)
    .bind(rule.source.as_str())
    .bind(rule.enabled)
    .bind(rule.created_at)
    .bind(rule.updated_at)
    .fetch_one(pool)
    .await
    .storage_err("保存行为规则失败")
}

/// 按 id 查询行为规则。
///
/// 返回:
/// - `Ok(Some(rule))`: 命中。
/// - `Ok(None)`: 未命中（不视为错误）。
pub async fn get(pool: &SqlitePool, id: i64) -> RamariaResult<Option<BehaviorRule>> {
    let row = sqlx::query_as::<_, RuleRow>(
        "SELECT id, persona_uid, situation, reaction, params, avoid, evidence,
                confidence, stability, source, enabled, created_at, updated_at
         FROM behavior_rules WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .storage_err("查询行为规则失败")?;

    row.map(RuleRow::into_rule).transpose()
}

/// 按 persona 查询全部行为规则（含 disabled，管理端需要展示禁用项）。
///
/// 返回:
/// - 按创建时间升序（规则生成顺序，便于展示与证据链对齐）。
pub async fn list_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<BehaviorRule>> {
    let rows = sqlx::query_as::<_, RuleRow>(
        "SELECT id, persona_uid, situation, reaction, params, avoid, evidence,
                confidence, stability, source, enabled, created_at, updated_at
         FROM behavior_rules WHERE persona_uid = ? ORDER BY created_at ASC, id ASC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询行为规则列表失败")?;

    rows.into_iter()
        .map(RuleRow::into_rule)
        .collect::<RamariaResult<Vec<_>>>()
}

/// 整体更新一条行为规则（edit 命令：reaction/params/avoid/situation/evidence 全量覆盖）。
///
/// 说明:
/// - 以 `id` 定位，`created_at` 保持不变，`updated_at` 由调用方刷新。
/// - 行不存在时返回 `Validation` 错误（id 无效）。
pub async fn update(pool: &SqlitePool, rule: &BehaviorRule) -> RamariaResult<()> {
    let situation = serde_json::to_string(&rule.situation)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior situation 失败: {e}")))?;
    let params = serde_json::to_string(&rule.params)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior params 失败: {e}")))?;
    let avoid = serde_json::to_string(&rule.avoid)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior avoid 失败: {e}")))?;
    let evidence = serde_json::to_string(&rule.evidence)
        .map_err(|e| RamariaError::serialization(format!("序列化 behavior evidence 失败: {e}")))?;

    let result = sqlx::query(
        "UPDATE behavior_rules SET
            persona_uid = ?, situation = ?, reaction = ?, params = ?, avoid = ?,
            evidence = ?, confidence = ?, stability = ?, source = ?, enabled = ?,
            updated_at = ?
         WHERE id = ?",
    )
    .bind(&rule.persona_uid)
    .bind(situation)
    .bind(&rule.reaction)
    .bind(params)
    .bind(avoid)
    .bind(evidence)
    .bind(rule.confidence)
    .bind(rule.stability)
    .bind(rule.source.as_str())
    .bind(rule.enabled)
    .bind(rule.updated_at)
    .bind(rule.id)
    .execute(pool)
    .await
    .storage_err("更新行为规则失败")?;

    if result.rows_affected() == 0 {
        return Err(RamariaError::validation(format!(
            "行为规则 {} 不存在，无法更新",
            rule.id
        )));
    }
    Ok(())
}

/// 删除一条行为规则。
///
/// 返回:
/// - `Ok(())`: 删除成功（行不存在时也视为成功——幂等删除）。
pub async fn delete(pool: &SqlitePool, id: i64) -> RamariaResult<()> {
    sqlx::query("DELETE FROM behavior_rules WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await
        .storage_err("删除行为规则失败")?;
    Ok(())
}

/// 启用/禁用一条行为规则（disable/enable 命令）。
///
/// 说明:
/// - 同时刷新 `updated_at`，供审计展示最近变更时间。
/// - 行不存在时返回 `Validation` 错误（id 无效）。
pub async fn set_enabled(pool: &SqlitePool, id: i64, enabled: bool) -> RamariaResult<()> {
    let updated_at = ramaria_core::types::now_ms();
    let result = sqlx::query("UPDATE behavior_rules SET enabled = ?, updated_at = ? WHERE id = ?")
        .bind(enabled)
        .bind(updated_at)
        .bind(id)
        .execute(pool)
        .await
        .storage_err("切换行为规则 enabled 失败")?;

    if result.rows_affected() == 0 {
        return Err(RamariaError::validation(format!(
            "行为规则 {} 不存在，无法切换启用状态",
            id
        )));
    }
    Ok(())
}

// =========================================================
// 单元测试（内存数据库，migration 自动应用）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;
    use ramaria_core::behavior::{BehaviorParams, BehaviorSituation, RuleSource};

    fn make_rule(persona: &str, reaction: Option<&str>) -> BehaviorRule {
        let mut rule = BehaviorRule::new(
            persona,
            BehaviorSituation {
                keywords: vec!["加班".into(), "累".into()],
                centroid: Some(vec![0.1, 0.2, 0.3]),
                response_centroid: None,
                valence_mean: -0.4,
                valence_std: 0.2,
                sample_count: 6,
                presentation_dist: Vec::new(),
                situation_strength_mean: 3.5,
                time_span_days: 20.0,
                trait_refs: Vec::new(),
            },
            reaction.map(String::from),
            BehaviorParams::default(),
            RuleSource::Auto,
        );
        rule.confidence = 0.8;
        rule.stability = 0.7;
        rule
    }

    #[tokio::test]
    async fn save_and_get_roundtrip() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let rule = make_rule("char-0001", Some("当聊到加班时，倾向表达疲惫并安慰对方。"));
        let id = save(&pool, &rule).await.expect("保存成功");
        let got = get(&pool, id).await.expect("查询成功").expect("应命中");
        assert_eq!(got.id, id);
        assert_eq!(got.persona_uid, "char-0001");
        assert_eq!(got.situation.keywords, vec!["加班", "累"]);
        assert_eq!(
            got.reaction.as_deref(),
            Some("当聊到加班时，倾向表达疲惫并安慰对方。")
        );
        assert_eq!(got.source, RuleSource::Auto);
        assert!(got.enabled);
        assert_eq!(got.confidence, 0.8);
        assert_eq!(got.stability, 0.7);
    }

    #[tokio::test]
    async fn get_missing_returns_none() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        assert!(get(&pool, 999).await.expect("查询成功").is_none());
    }

    #[tokio::test]
    async fn candidate_rule_reaction_is_null() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let rule = make_rule("char-0001", None);
        let id = save(&pool, &rule).await.expect("保存成功");
        let got = get(&pool, id).await.expect("查询成功").expect("应命中");
        assert!(got.is_candidate());
        assert!(got.reaction.is_none());
    }

    #[tokio::test]
    async fn list_filters_by_persona() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let a = save(&pool, &make_rule("char-0001", Some("规则 A")))
            .await
            .expect("保存 A");
        let b = save(&pool, &make_rule("char-0001", Some("规则 B")))
            .await
            .expect("保存 B");
        save(&pool, &make_rule("char-0002", Some("规则 C")))
            .await
            .expect("保存 C");

        let list = list_by_persona(&pool, "char-0001").await.expect("查询成功");
        assert_eq!(list.len(), 2, "跨 persona 隔离");
        assert_eq!(list[0].id, a);
        assert_eq!(list[1].id, b);
    }

    #[tokio::test]
    async fn update_overwrites_fields() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let mut rule = make_rule("char-0001", Some("旧文本"));
        let id = save(&pool, &rule).await.expect("保存成功");

        rule.id = id;
        rule.reaction = Some("新文本".into());
        rule.enabled = false;
        rule.updated_at += 1;
        update(&pool, &rule).await.expect("更新成功");

        let got = get(&pool, id).await.expect("查询成功").expect("应命中");
        assert_eq!(got.reaction.as_deref(), Some("新文本"));
        assert!(!got.enabled);
        // created_at 保持不变（update 只刷新 updated_at 与业务字段）
        assert_eq!(got.created_at, rule.created_at);
        assert_eq!(got.updated_at, rule.updated_at);
    }

    #[tokio::test]
    async fn update_missing_rule_is_validation_error() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let mut rule = make_rule("char-0001", Some("文本"));
        rule.id = 999;
        let err = update(&pool, &rule).await.expect_err("应返回错误");
        assert!(matches!(err, RamariaError::Validation { .. }));
    }

    #[tokio::test]
    async fn set_enabled_switches_and_missing_is_error() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let id = save(&pool, &make_rule("char-0001", Some("文本")))
            .await
            .expect("保存成功");

        set_enabled(&pool, id, false).await.expect("禁用成功");
        assert!(!get(&pool, id).await.expect("查询成功").unwrap().enabled);

        set_enabled(&pool, id, true).await.expect("启用成功");
        assert!(get(&pool, id).await.expect("查询成功").unwrap().enabled);

        let err = set_enabled(&pool, 999, true)
            .await
            .expect_err("不存在应报错");
        assert!(matches!(err, RamariaError::Validation { .. }));
    }

    #[tokio::test]
    async fn delete_removes_rule() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let id = save(&pool, &make_rule("char-0001", Some("文本")))
            .await
            .expect("保存成功");
        delete(&pool, id).await.expect("删除成功");
        assert!(get(&pool, id).await.expect("查询成功").is_none());
        // 幂等：重复删除不报错
        delete(&pool, id).await.expect("重复删除幂等");
    }

    #[tokio::test]
    async fn manual_source_roundtrip() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let mut rule = make_rule("char-0001", Some("手工规则"));
        rule.source = RuleSource::Manual;
        let id = save(&pool, &rule).await.expect("保存成功");
        let got = get(&pool, id).await.expect("查询成功").expect("应命中");
        assert_eq!(got.source, RuleSource::Manual);
        assert_eq!(got.source.as_str(), "manual");
    }
}
