//! crates/ramaria-storage/src/repo/feedback_log.rs - 反馈日志存取（v1.5 M5，算法说明书 v3.1 §9.4）
//!
//! 设计特点:
//! - 管理 `feedback_log` 表（v3.1 §9.4），S1 强信号（edit/disable，weight=1.0）写入
//! - S2/S3 弱信号（correction/continue）v1.7 复用同表，本版本只写入不消费
//! - detail 列只存"编辑前后快照" JSON，不存对话原文（隐私红线）
//! - 枚举解析失败时回退默认并记录 WARNING（parse_enum_fallback）

use crate::repo::StorageResultExt;
use ramaria_core::behavior::{FeedbackLog, SignalType, TargetType};
use ramaria_core::error::RamariaResult;
use sqlx::SqlitePool;

parse_enum_fallback!(
    parse_target_type, TargetType, TargetType::BehaviorRule, "feedback_log", "target_type",
    "behavior_rule"    => BehaviorRule,
    "persona_fact"     => PersonaFact,
    "personality_trait" => PersonalityTrait,
);

parse_enum_fallback!(
    parse_signal_type, SignalType, SignalType::Edit, "feedback_log", "signal_type",
    "edit"       => Edit,
    "disable"    => Disable,
    "correction" => Correction,
    "continue"   => Continue,
);

/// `feedback_log` 表的一行。
#[derive(sqlx::FromRow)]
struct LogRow {
    id: i64,
    persona_uid: String,
    target_type: String,
    target_id: String,
    signal_type: String,
    weight: f64,
    session_id: Option<String>,
    detail: Option<String>,
    created_at: i64,
}

impl LogRow {
    fn into_log(self) -> FeedbackLog {
        FeedbackLog {
            id: self.id,
            persona_uid: self.persona_uid,
            target_type: parse_target_type(&self.target_type),
            target_id: self.target_id,
            signal_type: parse_signal_type(&self.signal_type),
            weight: self.weight,
            session_id: self.session_id,
            detail: self.detail,
            created_at: self.created_at,
        }
    }
}

// =========================================================
// 查询 / 写入
// =========================================================

/// 写入一条反馈日志，返回自增 id。
pub async fn save(pool: &SqlitePool, log: &FeedbackLog) -> RamariaResult<i64> {
    sqlx::query_scalar::<_, i64>(
        "INSERT INTO feedback_log
         (persona_uid, target_type, target_id, signal_type, weight, session_id, detail, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?) RETURNING id",
    )
    .bind(&log.persona_uid)
    .bind(match log.target_type {
        TargetType::BehaviorRule => "behavior_rule",
        TargetType::PersonaFact => "persona_fact",
        TargetType::PersonalityTrait => "personality_trait",
    })
    .bind(&log.target_id)
    .bind(match log.signal_type {
        SignalType::Edit => "edit",
        SignalType::Disable => "disable",
        SignalType::Correction => "correction",
        SignalType::Continue => "continue",
    })
    .bind(log.weight)
    .bind(&log.session_id)
    .bind(&log.detail)
    .bind(log.created_at)
    .fetch_one(pool)
    .await
    .storage_err("写入反馈日志失败")
}

/// 按 persona 查询反馈日志（审计/证据链展示）。
///
/// 返回:
/// - 按创建时间降序（最近干预在前）。
pub async fn list_by_persona(
    pool: &SqlitePool,
    persona_uid: &str,
) -> RamariaResult<Vec<FeedbackLog>> {
    let rows = sqlx::query_as::<_, LogRow>(
        "SELECT id, persona_uid, target_type, target_id, signal_type, weight,
                session_id, detail, created_at
         FROM feedback_log WHERE persona_uid = ?
         ORDER BY created_at DESC, id DESC",
    )
    .bind(persona_uid)
    .fetch_all(pool)
    .await
    .storage_err("查询反馈日志失败")?;

    Ok(rows.into_iter().map(LogRow::into_log).collect())
}

// =========================================================
// 单元测试（内存数据库，migration 自动应用）
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::init_test_pool;
    use ramaria_core::behavior::TargetType;

    #[tokio::test]
    async fn save_and_list_roundtrip() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let log = FeedbackLog::new(
            "char-0001",
            TargetType::BehaviorRule,
            "3",
            SignalType::Disable,
            Some("sess-1".into()),
            Some(r#"{"before":{}}"#.to_string()),
        );
        let id = save(&pool, &log).await.expect("写入成功");
        assert!(id > 0);

        let list = list_by_persona(&pool, "char-0001").await.expect("查询成功");
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].id, id);
        assert_eq!(list[0].target_type, TargetType::BehaviorRule);
        assert_eq!(list[0].signal_type, SignalType::Disable);
        assert_eq!(list[0].weight, 1.0, "S1 强信号 weight=1.0");
        assert_eq!(list[0].session_id.as_deref(), Some("sess-1"));
    }

    #[tokio::test]
    async fn list_isolated_by_persona() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        save(
            &pool,
            &FeedbackLog::new(
                "p1",
                TargetType::BehaviorRule,
                "1",
                SignalType::Edit,
                None,
                None,
            ),
        )
        .await
        .expect("写入 p1");
        save(
            &pool,
            &FeedbackLog::new(
                "p2",
                TargetType::BehaviorRule,
                "1",
                SignalType::Edit,
                None,
                None,
            ),
        )
        .await
        .expect("写入 p2");

        assert_eq!(
            list_by_persona(&pool, "p1").await.expect("查询成功").len(),
            1
        );
        assert_eq!(
            list_by_persona(&pool, "p2").await.expect("查询成功").len(),
            1
        );
    }

    #[tokio::test]
    async fn reserved_target_types_roundtrip() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        for (ty, id) in [
            (TargetType::BehaviorRule, "b1"),
            (TargetType::PersonaFact, "f1"),
            (TargetType::PersonalityTrait, "t1"),
        ] {
            save(
                &pool,
                &FeedbackLog::new("char-0001", ty, id, SignalType::Edit, None, None),
            )
            .await
            .expect("写入成功");
        }
        let list = list_by_persona(&pool, "char-0001").await.expect("查询成功");
        assert_eq!(list.len(), 3);
        // 枚举 roundtrip（v1.7 预留类型不丢精度）
        let types: Vec<TargetType> = list.iter().map(|l| l.target_type).collect();
        assert!(types.contains(&TargetType::PersonaFact));
        assert!(types.contains(&TargetType::PersonalityTrait));
    }

    #[tokio::test]
    async fn detail_snapshot_preserved() {
        let pool = init_test_pool().await.expect("测试库初始化成功");
        let detail = r#"{"before":{"reaction":"旧"},"after":{"reaction":"新"}}"#;
        save(
            &pool,
            &FeedbackLog::new(
                "char-0001",
                TargetType::BehaviorRule,
                "9",
                SignalType::Edit,
                None,
                Some(detail.into()),
            ),
        )
        .await
        .expect("写入成功");
        let list = list_by_persona(&pool, "char-0001").await.expect("查询成功");
        assert_eq!(list[0].detail.as_deref(), Some(detail));
    }
}
