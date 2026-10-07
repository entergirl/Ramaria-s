//! crates/ramaria-service/src/import/tests.rs - Ramaria QQ 聊天记录导入用例单元测试
//!
//! 设计特点:
//! - 覆盖导入管线五段：格式探测 / 解析预览 / L0 写入 / L1 批量生成 / 深度触发
//! - 真实临时 SQLite 库 + mock LLM（嵌入 provider 注入 None），不依赖外部服务
//! - 进度回调经记录型 sink 断言事件序列与 ETA 口径；完成摘要以桌面分支文案为准
//! - 边界与降级：空消息文件 / 非 .json 扩展名 / 未附着连接池 / 画像名回读回退
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网、不使用真实用户数据。

use super::l0::resolve_persona_name;
use super::*;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::{StorageBackend, StoreCrud};
use ramaria_core::types::{Persona, PersonaKind};
use ramaria_importer::qq::ImportSide;
use ramaria_storage::SqliteStorage;
use sqlx::SqlitePool;

use crate::engine::Engine;
use crate::test_support::{L1_JSON_REPLY, MockLlm};

// ---- 测试脚手架 ----

/// 最小 qq-chat-exporter v6.x 导出（5 分钟切割线下两条 session）。
///
/// 时间戳分布:
/// - session 1: base 与 base+60s（双方各一条）；
/// - session 2: base+3600s 与 base+3660s（双方各一条）。
fn qq_export_json() -> String {
    let base = 1_700_000_000_000i64;
    let messages = [
        (base, "u_self", "小明", "早上好"),
        (base + 60_000, "u_peer", "小红", "早上好呀"),
        (base + 3_600_000, "u_self", "小明", "中午吃什么"),
        (base + 3_660_000, "u_peer", "小红", "吃面吧"),
    ];
    let body = messages
        .iter()
        .enumerate()
        .map(|(i, (ts, uid, name, text))| {
            format!(
                r#"{{"id":"m_{i}","timestamp":{ts},"type":"text","recalled":false,"system":false,"content":{{"text":"{text}","elements":[]}},"sender":{{"uid":"{uid}","name":"{name}"}}}}"#
            )
        })
        .collect::<Vec<_>>()
        .join(",");
    format!(
        r#"{{"chatInfo":{{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"}},"messages":[{body}]}}"#
    )
}

/// 构造 L0 导入请求（默认双方、无画像覆盖、切割间隔 10 分钟）。
fn import_request(file_path: &Path) -> ImportRequest {
    ImportRequest {
        file_path: file_path.to_path_buf(),
        mode: ImportMode::Fast,
        gap_minutes: 10,
        side: ImportSide::Both,
        persona_name: None,
        self_persona_uid: None,
        other_persona_name: None,
        other_persona_uid: None,
    }
}

/// 装配导入用例测试引擎（真实临时库 + mock LLM + 已附着连接池）。
///
/// 说明:
/// - 使用 `init_pool` 建库（含 migration），不经 `Engine::open_with`
///   （避免装配真实 provider）；
/// - 嵌入 provider 注入 None（导入用例不依赖向量通道）。
async fn import_engine(tag: &str) -> (Engine, SqlitePool, Arc<SqliteStorage>, PathBuf) {
    let dir = crate::test_support::temp_dir(tag);
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("测试库初始化应成功");
    let storage = Arc::new(SqliteStorage::new(pool.clone()));
    let engine = Engine::from_parts(
        storage.clone() as Arc<dyn StorageBackend>,
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY)),
        None,
        RamariaConfig::default(),
    );
    engine.attach_sqlite_pool(pool.clone());
    (engine, pool, storage, dir)
}

/// 记录进度事件的测试 sink。
struct RecordingSink {
    events: Mutex<Vec<ImportL1Progress>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<ImportL1Progress> {
        self.events.lock().expect("进度事件锁不应中毒").clone()
    }
}

impl ImportProgressSink for RecordingSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        self.events
            .lock()
            .expect("进度事件锁不应中毒")
            .push(p.clone());
    }

    fn on_done(&self, _summary: &ImportDoneSummary) {}
}

// ---- 格式探测 ----

/// QQ JSON 匹配；无特征普通 JSON 不匹配；文件不存在返回 Io 错误。
#[tokio::test]
async fn detect_format_distinguishes_qq_json() {
    let (engine, _pool, _storage, dir) = import_engine("import-detect").await;

    let qq_path = dir.join("qq_export.json");
    std::fs::write(&qq_path, qq_export_json()).expect("写入导出文件应成功");
    assert!(
        engine.detect_qq_format(&qq_path).await.expect("探测应成功"),
        "含 chatInfo/messages 的 JSON 应判定为 QQ 格式"
    );

    let plain_path = dir.join("plain.json");
    std::fs::write(&plain_path, r#"{"hello":"world"}"#).expect("写入普通 JSON 应成功");
    assert!(
        !engine
            .detect_qq_format(&plain_path)
            .await
            .expect("探测应成功"),
        "无 QQ 特征的 JSON 不应判定为匹配"
    );

    let err = engine
        .detect_qq_format(&dir.join("missing.json"))
        .await
        .expect_err("文件不存在应返回错误");
    assert_eq!(err.category(), "io", "读取失败应为 Io 错误: {err}");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 解析 ----

#[tokio::test]
async fn analyze_reports_statistics_and_names() {
    let (engine, _pool, _storage, dir) = import_engine("import-analyze").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let report = engine
        .analyze_qq_import(AnalyzeRequest {
            file_path: file_path.clone(),
            gap_minutes: 10,
        })
        .await
        .expect("解析用例应成功");

    assert_eq!(report.file_path, file_path);
    assert_eq!(report.self_name, "小明");
    assert_eq!(report.chat_name, "小红");
    assert_eq!(report.other_name, "小红");
    assert_eq!(report.total_raw, 4);
    assert_eq!(report.total_success, 4, "4 条 text 消息应全部成功解析");
    assert_eq!(report.total_skipped, 0);
    assert_eq!(
        report.session_count, 2,
        "同 session 内 60 秒间隔、跨 session 1 小时间隔"
    );
    assert_eq!(report.gap_minutes, 10);
    assert!(!report.time_range.is_empty(), "时间范围应非空");

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- L0 写入 ----

#[tokio::test]
async fn write_l0_creates_sessions_messages_and_personas() {
    let (engine, pool, storage, dir) = import_engine("import-l0").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("L0 导入应成功");

    assert_eq!(outcome.mode, ImportMode::Fast);
    assert_eq!(outcome.sessions_written, 2);
    assert_eq!(outcome.messages_written, 4);
    assert_eq!(outcome.messages_dropped, 0);
    assert_eq!(outcome.skipped_count, 0);
    assert_eq!(outcome.session_ids.len(), 2);
    assert_eq!(outcome.persona_uid.as_deref(), Some("user-10001"));
    assert_eq!(outcome.other_persona_uid.as_deref(), Some("char-90002"));
    assert_eq!(outcome.self_name, "小明");
    assert_eq!(outcome.chat_name, "小红");
    assert!(!outcome.report_summary.is_empty());

    // 两个 source="qq" persona 各创建一次（导出者 user- / 对方 char-）
    let personas = ramaria_storage::repo::personas::list_all(&pool)
        .await
        .expect("读取 persona 列表应成功");
    let qq_personas: Vec<_> = personas.iter().filter(|p| p.source == "qq").collect();
    assert_eq!(qq_personas.len(), 2, "双画像应各创建一个 qq persona");
    assert!(qq_personas.iter().any(|p| p.uid == "user-10001"));
    assert!(qq_personas.iter().any(|p| p.uid == "char-90002"));

    // 画像名上报库内实际注册名（新建路径与请求名 / 文件解析名一致）
    let self_persona = ramaria_storage::repo::personas::get_by_uid(&pool, "user-10001")
        .await
        .expect("读取导出者 persona 应成功")
        .expect("导出者 persona 应存在");
    assert_eq!(outcome.persona_name, self_persona.name);
    assert_eq!(outcome.persona_name, "小明");
    let other_persona = ramaria_storage::repo::personas::get_by_uid(&pool, "char-90002")
        .await
        .expect("读取对方 persona 应成功")
        .expect("对方 persona 应存在");
    assert_eq!(outcome.other_persona_name, other_persona.name);
    assert_eq!(outcome.other_persona_name, "小红");

    // 消息按 session 落库（每个 session 2 条）
    let mut total_messages = 0usize;
    for session_id in &outcome.session_ids {
        total_messages += storage
            .list_messages(*session_id)
            .await
            .expect("读取会话消息应成功")
            .len();
    }
    assert_eq!(total_messages, 4);

    let _ = std::fs::remove_dir_all(&dir);
}

/// 复用既有 persona：结果上报库内实际名（请求覆盖名不落库也不上报）。
#[tokio::test]
async fn write_l0_reports_stored_persona_name_when_reusing_existing() {
    let (engine, pool, _storage, dir) = import_engine("import-reuse-name").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    // 预置既有 persona：uid 与导入解析一致，名称与请求覆盖名 / 文件解析名均不同
    let existing = Persona::new(
        "user-10001".to_string(),
        "旧名".to_string(),
        PersonaKind::User,
        1,
        "qq".to_string(),
    );
    ramaria_storage::repo::personas::create(&pool, &existing)
        .await
        .expect("预置 persona 应成功");

    let mut req = import_request(&file_path);
    req.persona_name = Some("新名".to_string());
    let outcome = engine.import_qq_l0(req).await.expect("L0 导入应成功");

    assert_eq!(
        outcome.persona_name, "旧名",
        "复用既有 persona 时应上报库内实际名"
    );
    let stored = ramaria_storage::repo::personas::get_by_uid(&pool, "user-10001")
        .await
        .expect("读取 persona 应成功")
        .expect("既有 persona 应仍存在");
    assert_eq!(stored.name, "旧名", "请求覆盖名不应改写既有 persona 名称");
    assert_eq!(outcome.self_name, "小明", "self_name 保持文件解析名口径");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 新建 persona：请求覆盖名即实际创建名，结果按库内回读上报（双方）。
#[tokio::test]
async fn write_l0_reports_created_name_with_overrides() {
    let (engine, pool, _storage, dir) = import_engine("import-create-name").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let mut req = import_request(&file_path);
    req.persona_name = Some("导出者备注名".to_string());
    req.other_persona_name = Some("对方备注名".to_string());
    let outcome = engine.import_qq_l0(req).await.expect("L0 导入应成功");

    assert_eq!(outcome.persona_name, "导出者备注名");
    assert_eq!(outcome.other_persona_name, "对方备注名");
    assert_eq!(outcome.self_name, "小明", "self_name 仍为文件解析名");

    let self_persona = ramaria_storage::repo::personas::get_by_uid(&pool, "user-10001")
        .await
        .expect("读取导出者 persona 应成功")
        .expect("导出者 persona 应存在");
    assert_eq!(self_persona.name, "导出者备注名");
    let other_persona = ramaria_storage::repo::personas::get_by_uid(&pool, "char-90002")
        .await
        .expect("读取对方 persona 应成功")
        .expect("对方 persona 应存在");
    assert_eq!(other_persona.name, "对方备注名");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 同文件二次导入：指纹去重（跨批次查重命中 → 新增消息为 0）。
///
/// 依据:
/// - 消息指纹由 `ramaria-importer` 生成，写入 `messages.import_fingerprint`（全局 UNIQUE）；
/// - `ImportWriter::write_l0` 写入前经 `find_by_fingerprint` 查重跳过，因此第二次导入
///   的消息新增数为 0；会话容器仍会创建（去重只跳过消息，不回滚会话）。
#[tokio::test]
async fn write_l0_second_import_deduplicates_fingerprints() {
    let (engine, pool, _storage, dir) = import_engine("import-dedup").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let first = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("首次导入应成功");
    assert_eq!(first.messages_written, 4);

    let second = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("二次导入应成功");
    assert_eq!(second.messages_written, 0, "同指纹消息应被跨批次查重跳过");
    assert_eq!(second.messages_dropped, 0);
    assert_eq!(
        second.sessions_written, 2,
        "会话容器仍创建（去重只作用于消息）"
    );
    assert_eq!(second.session_ids.len(), 2);

    // 库内消息总数保持 4；persona 仍是 2 个（按 uid 命中复用）
    let message_count: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM messages")
        .fetch_one(&pool)
        .await
        .expect("统计消息应成功");
    assert_eq!(message_count, 4);
    let personas = ramaria_storage::repo::personas::list_all(&pool)
        .await
        .expect("读取 persona 列表应成功");
    assert_eq!(personas.iter().filter(|p| p.source == "qq").count(), 2);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- L1 批量生成 ----

/// L0 导入写入发送者身份两列与会话成员行（消息字段与成员行一一对应）。
#[tokio::test]
async fn write_l0_persists_sender_identity_and_members() {
    let (engine, pool, storage, dir) = import_engine("import-sender-identity").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("L0 导入应成功");
    assert_eq!(outcome.sessions_written, 2);

    // 每个会话：消息 sender 两列与自身身份一致（u_self ↔ 小明、u_peer ↔ 小红）
    for session_id in &outcome.session_ids {
        let messages = storage
            .list_messages(*session_id)
            .await
            .expect("读取会话消息应成功");
        assert_eq!(messages.len(), 2, "每会话双方各一条消息");
        for msg in &messages {
            let (expected_ref, expected_name) = match msg.persona_uid.as_deref() {
                Some("user-10001") => ("u_self", "小明"),
                Some("char-90002") => ("u_peer", "小红"),
                other => panic!("意外的 persona_uid: {other:?}"),
            };
            assert_eq!(msg.sender_ref.as_deref(), Some(expected_ref));
            assert_eq!(msg.sender_name.as_deref(), Some(expected_name));
        }

        // 成员行：两行、platform_ref 与名称对应、按首见升序
        let members = ramaria_storage::repo::session_members::list_by_session(&pool, *session_id)
            .await
            .expect("读取会话成员应成功");
        let pairs: Vec<(String, String)> = members
            .iter()
            .map(|m| (m.platform_ref.clone(), m.name.clone()))
            .collect();
        assert_eq!(
            pairs,
            vec![
                ("u_self".to_string(), "小明".to_string()),
                ("u_peer".to_string(), "小红".to_string()),
            ],
            "成员行应按首见升序且名称对应"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn generate_l1_generates_for_both_personas_with_eta_progress() {
    let (engine, _pool, storage, dir) = import_engine("import-l1").await;
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect("L0 导入应成功");
    let self_uid = outcome
        .persona_uid
        .clone()
        .expect("导出者 persona 应已创建");
    let other_uid = outcome
        .other_persona_uid
        .clone()
        .expect("对方 persona 应已创建");

    let sink = RecordingSink::new();
    let plan = ImportL1Plan {
        targets: vec![Some(self_uid), Some(other_uid)],
        cascade: false,
        throttle_ms: 0,
    };
    let l1 = engine
        .generate_import_l1(&outcome.session_ids, plan, Some(&sink))
        .await
        .expect("L1 批量生成应成功");

    assert_eq!(l1.l1_total, 4, "每 session 双方 persona 各一次");
    assert_eq!(l1.l1_success, 4);
    assert_eq!(l1.l1_failed, 0);
    assert_eq!(l1.l1_skipped, 0);
    assert_eq!(l1.l1_processed, 4);
    assert_eq!(l1.session_ids.len(), 2);

    // 库内 L1 行数与调用数一致（每 session 两份：self / other 各一）
    let mut l1_rows = 0usize;
    for session_id in &outcome.session_ids {
        l1_rows += storage
            .list_memory_l1(*session_id)
            .await
            .expect("读取 L1 应成功")
            .len();
    }
    assert_eq!(l1_rows, 4, "每 session 应落两份 L1（双方 persona）");

    // ETA 进度回调：起始一条（0/4）+ 每 session 一条，阶段均为 l1，末条为完成态
    let events = sink.events();
    assert_eq!(events.len(), 3, "起始进度 + 每 session 一条");
    assert!(events.iter().all(|e| e.phase == "l1"));
    assert_eq!(events[0].current, 0);
    assert_eq!(events[0].total, 4);
    assert_eq!(events[0].l1_total, Some(4));
    let last = events.last().expect("应有进度事件");
    assert_eq!(last.current, 4);
    assert_eq!(last.total, 4);

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 边界与降级 ----

#[tokio::test]
async fn write_l0_rejects_empty_messages_file() {
    let (engine, _pool, _storage, dir) = import_engine("import-empty").await;
    let file_path = dir.join("empty.json");
    let empty = r#"{"chatInfo":{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"},"messages":[]}"#;
    std::fs::write(&file_path, empty).expect("写入空导出文件应成功");

    let err = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect_err("空消息文件应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.context().contains("没有可导入的消息"),
        "错误应说明没有可导入消息: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn write_l0_rejects_non_json_extension() {
    let (engine, _pool, _storage, dir) = import_engine("import-ext").await;
    let file_path = dir.join("export.txt");
    std::fs::write(&file_path, qq_export_json()).expect("写入非 .json 文件应成功");

    let err = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect_err("非 .json 扩展名应报错");
    assert_eq!(err.category(), "validation");
    assert!(
        err.context().contains("不支持的文件类型"),
        "错误应说明扩展名不支持: {err}"
    );

    // 解析用例同样执行扩展名校验
    let err = engine
        .analyze_qq_import(AnalyzeRequest {
            file_path,
            gap_minutes: 10,
        })
        .await
        .expect_err("非 .json 扩展名应报错");
    assert_eq!(err.category(), "validation");

    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn write_l0_requires_attached_pool() {
    let dir = crate::test_support::temp_dir("import-no-pool");
    let db_path = dir.join("assistant.db");
    let pool = ramaria_storage::database::init_pool(Some(db_path))
        .await
        .expect("测试库初始化应成功");
    let storage: Arc<dyn StorageBackend> = Arc::new(SqliteStorage::new(pool));
    // 注入构造不携带连接池：导入用例应显式报错而不是 panic
    let engine = Engine::from_parts(
        storage,
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY)),
        None,
        RamariaConfig::default(),
    );
    let file_path = dir.join("export.json");
    std::fs::write(&file_path, qq_export_json()).expect("写入导出文件应成功");

    let err = engine
        .import_qq_l0(import_request(&file_path))
        .await
        .expect_err("未附着连接池应显式报错");
    assert_eq!(err.category(), "unsupported");
    assert!(
        err.context().contains("attach_sqlite_pool"),
        "错误应指向连接池附着入口: {err}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 画像名回读降级：未指定 uid / 未命中 / 查询报错均回退展示名（不阻塞导入）。
#[tokio::test]
async fn resolve_persona_name_falls_back_to_display_name() {
    let (_engine, pool, _storage, dir) = import_engine("import-name-fallback").await;

    assert_eq!(
        resolve_persona_name(&pool, None, "回退名").await,
        "回退名",
        "导入侧过滤跳过时应回退展示名"
    );
    assert_eq!(
        resolve_persona_name(&pool, Some("user-9999"), "回退名").await,
        "回退名",
        "uid 未命中应回退展示名"
    );

    // 连接池关闭 → 查询报错 → 回退展示名（warn 日志，不向外抛错）
    pool.close().await;
    assert_eq!(
        resolve_persona_name(&pool, Some("user-10001"), "回退名").await,
        "回退名",
        "查询报错应回退展示名"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- 深度触发与完成摘要 ----

#[tokio::test]
async fn trigger_deep_emits_l2_then_l3_progress() {
    let (engine, _pool, _storage, dir) = import_engine("import-deep").await;
    let sink = RecordingSink::new();

    engine
        .trigger_import_deep(Some(4), Some(&sink))
        .await
        .expect("深度触发应成功");

    let events = sink.events();
    assert_eq!(events.len(), 2, "应发送 L2 / L3 两条阶段进度");
    assert_eq!(events[0].phase, "l2");
    assert_eq!(events[0].current, 0);
    assert_eq!(events[0].total, 2);
    assert_eq!(events[0].l1_total, Some(4), "应回填调用方传入的 L1 总量");
    assert_eq!(events[0].l2_total, Some(2));
    assert_eq!(events[1].phase, "l3");
    assert_eq!(events[1].current, 0);
    assert_eq!(events[1].total, 2);
    assert_eq!(events[1].l1_total, Some(4));
    assert_eq!(events[1].l2_total, Some(2));
    assert_eq!(events[1].l3_total, Some(2));

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn done_summary_matches_desktop_branches() {
    let success = l1_outcome(4, 0, 0, 4);
    let ok = done_summary(&success, true, true, 2);
    assert_eq!(ok.message, "深度处理完成: L1 全部成功 (4/4)");
    assert!(ok.l2_triggered && ok.l3_triggered);
    assert_eq!(ok.total_sessions, 2);
    assert_eq!(ok.l1_success, 4);

    let partial = l1_outcome(2, 1, 1, 4);
    let failed = done_summary(&partial, true, true, 2);
    assert_eq!(
        failed.message,
        "深度处理完成: L1 成功 2/4, 失败 1。请确认 LLM 已连接后重试。"
    );
    assert_eq!(failed.l1_failed, 1);
}

/// 构造 L1 批量生成结果（完成摘要用例的最小输入）。
fn l1_outcome(success: usize, failed: usize, skipped: usize, processed: usize) -> ImportL1Outcome {
    ImportL1Outcome {
        l1_success: success,
        l1_failed: failed,
        l1_skipped: skipped,
        l1_processed: processed,
        l1_total: processed,
        session_ids: Vec::new(),
    }
}
