//! crates/ramaria-eval/src/baseline/artifacts/tests.rs - Ramaria 评测证据映射单元测试
//!
//! 设计特点:
//! - 会话对齐用纯结构夹具：不建库，只锁定条数 / 正文 / 顺序三条判定
//! - 映射往返落到临时目录：写读逐字段相等，末尾换行与顺序一并锁定
//! - 读取校验按类型 / 版本 / 档位 / 语料摘要逐项拒绝，各一条
//! - 不读写真实档位产物与用户库

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::NaiveDate;
use ramaria_core::error::RamariaError;
use ramaria_core::types::{MessageRole, MessageSource};
use ramaria_service::{ChatRole, SessionMessageView};
use uuid::Uuid;

use crate::corpus::{Message, Session};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    let dir = std::env::temp_dir().join(format!("ramaria-eval-artifacts-{tag}-{nanos}"));
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    dir
}

fn date(day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2025, 12, day).expect("日期应合法")
}

fn message(dia_id: &str, text: &str) -> Message {
    Message {
        role: ChatRole::User,
        speaker: "甲".to_string(),
        dia_id: dia_id.to_string(),
        text: text.to_string(),
    }
}

fn session(messages: Vec<Message>) -> Session {
    Session {
        index: 1,
        date: date(2),
        messages,
    }
}

fn stored(content: &str) -> SessionMessageView {
    SessionMessageView {
        id: Uuid::new_v4(),
        role: MessageRole::User,
        content: content.to_string(),
        created_at: 0,
        source: MessageSource::Local,
        persona_uid: Some("lifebench-user-1".to_string()),
        is_proactive: false,
        sender_ref: None,
        sender_name: None,
        recalled_at: None,
    }
}

fn facts(sha256: &str) -> CorpusFacts {
    CorpusFacts {
        name: "lifebench".to_string(),
        version: "2.0".to_string(),
        sha256: sha256.to_string(),
    }
}

fn sample_map() -> ArtifactMap {
    let mut artifacts = BTreeMap::new();
    artifacts.insert("chat1".to_string(), vec!["m1".to_string()]);
    let mut samples = BTreeMap::new();
    samples.insert("user-1".to_string(), artifacts);
    ArtifactMap {
        artifact_kind: ARTIFACT_KIND.to_string(),
        schema_version: ARTIFACT_SCHEMA_VERSION,
        tier_id: "u1-m1".to_string(),
        corpus: facts("sha-a"),
        samples,
    }
}

// =========================================================
// 会话对齐
// =========================================================

/// 条数不符直接拒绝，文案带日期。
#[test]
fn pair_session_rejects_length_mismatch() {
    let session = session(vec![message("2025-12-02_chat1", "甲")]);
    let error = pair_session(&session, &[]).expect_err("条数不符应报错");
    assert!(matches!(&error, RamariaError::Validation { .. }));
    assert!(error.to_string().contains("2025-12-02"));
}

/// 正文不符拒绝，文案带日期与序号。
#[test]
fn pair_session_rejects_content_mismatch() {
    let session = session(vec![
        message("2025-12-02_chat1", "甲"),
        message("2025-12-02_chat2", "乙"),
    ]);
    let messages = vec![stored("甲"), stored("丙")];
    let error = pair_session(&session, &messages).expect_err("正文不符应报错");
    assert!(matches!(&error, RamariaError::Validation { .. }));
    assert!(error.to_string().contains("2025-12-02"));
    assert!(error.to_string().contains("第 2 条"));
}

/// 正常对齐保持语料顺序，同一 artifact 的多条消息各自成对。
#[test]
fn pair_session_pairs_in_corpus_order() {
    let session = session(vec![
        message("2025-12-02_chat1", "甲 "),
        message("2025-12-02_chat1", "乙"),
        message("2025-12-02_chat2", "丙"),
    ]);
    // 语料带尾随空格、库内已裁掉，仍按同一条对齐。
    let messages = vec![stored("甲"), stored("乙"), stored("丙")];
    let ids: Vec<String> = messages.iter().map(|view| view.id.to_string()).collect();

    let pairs = pair_session(&session, &messages).expect("应可对齐");
    assert_eq!(
        pairs,
        vec![
            ("chat1".to_string(), ids[0].clone()),
            ("chat1".to_string(), ids[1].clone()),
            ("chat2".to_string(), ids[2].clone()),
        ]
    );
}

// =========================================================
// 读写
// =========================================================

/// 写出后读回逐字段相等，且文件以换行收尾。
#[test]
fn artifacts_round_trip() {
    let root = temp_dir("round-trip");
    let dir = root.join("u1-m1");
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    let map = sample_map();

    let path = write(&dir, &map).expect("应可写出");
    let text = std::fs::read_to_string(&path).expect("应可读回");
    assert!(text.ends_with('\n'), "写出的 JSON 应以换行收尾");

    let read_back = read(&dir, "sha-a").expect("应可读取");
    assert_eq!(read_back, map);

    let _ = std::fs::remove_dir_all(&root);
}

/// 档位目录名与映射标识不一致时拒绝。
#[test]
fn read_rejects_foreign_tier() {
    let root = temp_dir("foreign-tier");
    let dir = root.join("u5-y1");
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    write(&dir, &sample_map()).expect("应可写出");

    let error = read(&dir, "sha-a").expect_err("档位不符应报错");
    assert!(matches!(&error, RamariaError::Validation { .. }));
    assert!(error.to_string().contains("u5-y1"));

    let _ = std::fs::remove_dir_all(&root);
}

/// 语料摘要不符时拒绝，文案带路径。
#[test]
fn read_rejects_corpus_mismatch() {
    let root = temp_dir("corpus");
    let dir = root.join("u1-m1");
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    write(&dir, &sample_map()).expect("应可写出");

    let error = read(&dir, "sha-b").expect_err("摘要不符应报错");
    assert!(matches!(&error, RamariaError::Validation { .. }));
    assert!(error.to_string().contains(ARTIFACTS_FILE));

    let _ = std::fs::remove_dir_all(&root);
}

/// 映射不存在返回 None（可选附件语义）。
#[test]
fn load_returns_none_without_file() {
    let root = temp_dir("absent");
    let dir = root.join("u1-m1");
    std::fs::create_dir_all(&dir).expect("档位目录应可创建");
    assert_eq!(load(&dir).expect("缺文件不应报错"), None);
    assert!(read(&dir, "sha-a").is_err(), "严格读取要求文件存在");
    let _ = std::fs::remove_dir_all(Path::new(&root));
}

/// 采集窗口：覆盖夹具所在日期。
fn window() -> TierSpec {
    TierSpec {
        id: "u1-m1",
        users: 1,
        memory_start: date(1),
        memory_end: date(31),
        query_end: date(31),
    }
}

async fn close_engine(engine: ramaria_service::Engine) {
    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    drop(engine);
}

// =========================================================
// 采集
// =========================================================

/// 用真实迁移库插入人格 / 会话 / 消息，验证采集按语料顺序落到映射表。
#[tokio::test]
async fn collect_maps_artifacts_from_library() {
    let root = temp_dir("collect");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");
    let pool = engine.sqlite_pool().expect("评测引擎应带连接池");
    sqlx::query(
        "INSERT OR IGNORE INTO personas (uid, name, kind, seq, source, created_at, updated_at) VALUES (?1, ?2, 'char', 1, 'local', 0, 0)",
    )
    .bind("lifebench-user-1")
    .bind("lifebench-user-1")
    .execute(&pool)
    .await
    .expect("人格夹具应可写");

    let session_id = Uuid::new_v4().to_string();
    sqlx::query(
        "INSERT INTO sessions (id, started_at, channel, persona_uid, external_ref) VALUES (?1, 0, 'eval', ?2, ?3)",
    )
    .bind(&session_id)
    .bind("lifebench-user-1")
    .bind("lifebench:user-1:2025-12-02")
    .execute(&pool)
    .await
    .expect("会话夹具应可写");

    let texts = ["甲", "乙", "丙"];
    let mut ids = Vec::new();
    for (index, text) in texts.iter().enumerate() {
        let id = Uuid::new_v4().to_string();
        sqlx::query(
            "INSERT INTO messages (id, session_id, role, content, created_at, source) VALUES (?1, ?2, 'user', ?3, ?4, 'local')",
        )
        .bind(&id)
        .bind(&session_id)
        .bind(*text)
        .bind(index as i64)
        .execute(&pool)
        .await
        .expect("消息夹具应可写");
        ids.push(id);
    }

    let sample = crate::corpus::Sample {
        sample_id: "sample-1".to_string(),
        user_key: "user-1".to_string(),
        speaker_a: "甲".to_string(),
        speaker_b: "乙".to_string(),
        sessions: vec![session(vec![
            message("2025-12-02_chat1", "甲"),
            message("2025-12-02_chat1", "乙"),
            message("2025-12-02_chat2", "丙"),
        ])],
        qa: Vec::new(),
    };

    let map = collect(&engine, &[sample], &window(), "u1-m1", facts("sha-a"))
        .await
        .expect("采集应成功");
    let artifacts = map.samples.get("user-1").expect("样本应有一项映射");
    assert_eq!(
        artifacts.get("chat1"),
        Some(&vec![ids[0].clone(), ids[1].clone()])
    );
    assert_eq!(artifacts.get("chat2"), Some(&vec![ids[2].clone()]));
    assert_eq!(map.artifact_total(), 2);

    close_engine(engine).await;
    let _ = std::fs::remove_dir_all(&root);
}
