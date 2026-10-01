//! tests/session_tests.rs - CLI `session show` / `session summarize` 输出口径测试
//!
//! 覆盖:
//! - `session show`：多消息会话的文本渲染（状态 / 创建时间 / 消息数 / 角色图标 / 200 字符截断）
//! - `session show --json`：负载字段集、字段值与消息顺序（固定夹具逐字段断言，全量加载）
//! - `session show` / `session summarize`：不存在会话的文案与退出码（业务校验失败）
//! - `session summarize`：无消息会话的 `--json` 空数据信封（`no_messages`）
//!
//! 安全约束:
//! - 真实 SQLite 临时库播种 + 真实 CLI 二进制（CARGO_BIN_EXE_ramaria）进程级断言
//! - 不调用真实 LLM、不连网、不触碰 keychain；临时目录随测试清理

use ramaria_core::traits::StoreCrud;
use ramaria_core::types::{Message, MessageRole, MessageSource, Persona, PersonaKind};
use ramaria_storage::SqliteStorage;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use uuid::Uuid;

// =========================================================
// 固定夹具
// =========================================================

/// 活跃会话起始时间（2024-06-10 08:00:00 UTC）。
const START_MS: i64 = 1_718_006_400_000;
/// 已结束会话的结束时间（2024-06-10 09:00:00 UTC）。
const END_MS: i64 = 1_718_010_000_000;

/// 播种结果（会话 id 与超长消息正文）。
struct Fixture {
    active: Uuid,
    ended: Uuid,
    empty: Uuid,
    long_content: String,
}

/// 构造固定 id / 时间戳的消息。
fn message(
    session_id: Uuid,
    id: &str,
    role: MessageRole,
    content: &str,
    created_at: i64,
    source: MessageSource,
) -> Message {
    Message {
        id: Uuid::parse_str(id).expect("固定 UUID 应合法"),
        session_id,
        role,
        content: content.to_string(),
        created_at,
        source,
        fingerprint: None,
        persona_uid: Some("char-0001".to_string()),
    }
}

/// 播种固定夹具：活跃会话（3 条消息，含超长消息）+ 已结束会话 + 空会话。
async fn seed_fixture(db: &Path) -> Fixture {
    let pool = ramaria_storage::database::init_pool(Some(db.to_path_buf()))
        .await
        .expect("初始化测试数据库失败");
    let storage = SqliteStorage::new(pool.clone());

    let persona = Persona::new(
        "char-0001".to_string(),
        "测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    storage
        .create_persona(&persona)
        .await
        .expect("写入 persona 应成功");

    let active = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let ended = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");
    let empty = storage
        .create_session(Some("char-0001"))
        .await
        .expect("创建会话应成功");

    let long_content = "很长的消息".chars().cycle().take(240).collect::<String>();
    let messages = vec![
        message(
            active.id,
            "aaaaaaaa-1111-1111-1111-111111111111",
            MessageRole::User,
            "你好，今天想聊聊项目进度",
            START_MS,
            MessageSource::Local,
        ),
        message(
            active.id,
            "bbbbbbbb-2222-2222-2222-222222222222",
            MessageRole::Assistant,
            "好的！请说说目前进展到哪里了？",
            START_MS + 60_000,
            MessageSource::Online,
        ),
        message(
            active.id,
            "cccccccc-3333-3333-3333-333333333333",
            MessageRole::User,
            &long_content,
            START_MS + 120_000,
            MessageSource::Local,
        ),
        message(
            ended.id,
            "dddddddd-4444-4444-4444-444444444444",
            MessageRole::User,
            "已结束会话的消息",
            START_MS + 60_000,
            MessageSource::Local,
        ),
    ];
    for m in &messages {
        storage.save_message(m).await.expect("写入消息应成功");
    }

    // 固定时间戳：创建接口使用当前时间，夹具按确定值回写（消息写入后置 ended_at）
    sqlx::query("UPDATE sessions SET started_at = ?, ended_at = NULL WHERE id = ?")
        .bind(START_MS)
        .bind(active.id.to_string())
        .execute(&pool)
        .await
        .expect("回写会话时间应成功");
    sqlx::query("UPDATE sessions SET started_at = ?, ended_at = ? WHERE id = ?")
        .bind(START_MS)
        .bind(END_MS)
        .bind(ended.id.to_string())
        .execute(&pool)
        .await
        .expect("回写会话时间应成功");
    sqlx::query("UPDATE sessions SET started_at = ? WHERE id = ?")
        .bind(START_MS)
        .bind(empty.id.to_string())
        .execute(&pool)
        .await
        .expect("回写会话时间应成功");

    pool.close().await;
    Fixture {
        active: active.id,
        ended: ended.id,
        empty: empty.id,
        long_content,
    }
}

// =========================================================
// 辅助函数
// =========================================================

/// 临时目录序号（并行测试线程安全：纳秒可能撞车，追加原子计数保证唯一）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建唯一临时测试目录（库文件位于其中，独立于仓库目录）。
fn temp_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("系统时间应晚于 Unix 纪元")
        .subsec_nanos();
    let dir = std::env::temp_dir().join(format!(
        "ramaria-cli-session-{tag}-{}-{seq}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 以真实 CLI 二进制运行（共享已播种的临时库）。
fn run_cli(db: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .args(args)
        .arg("--db")
        .arg(db)
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// 解析 stdout 单行 JSON 信封。
fn parse_envelope(out: &std::process::Output) -> serde_json::Value {
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.lines().count(),
        1,
        "stdout 应只含一行 JSON，实际: {stdout:?}"
    );
    serde_json::from_str(stdout.trim()).expect("stdout 必须是合法 JSON")
}

/// 取 JSON 对象键集合并升序返回（锁定字段集，杜绝多余 / 缺失字段）。
fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("应为 JSON 对象")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

// =========================================================
// session show — JSON 逐字段
// =========================================================

/// `session show --json`：字段集 / 字段值 / 消息顺序与夹具逐项一致（全量加载不截断）。
#[tokio::test]
async fn session_show_json_locks_fields_and_values() {
    let dir = temp_dir("show-json");
    let db = dir.join("session.db");
    let fx = seed_fixture(&db).await;
    let active = fx.active.to_string();
    let ended = fx.ended.to_string();

    // 活跃会话：ended_at 为 null
    let out = run_cli(&db, &["session", "show", &active, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = parse_envelope(&out);
    assert_eq!(parsed["ok"], true);
    let data = &parsed["data"];
    assert_eq!(sorted_keys(data), vec!["messages", "session"]);
    assert_eq!(
        sorted_keys(&data["session"]),
        vec!["ended_at", "id", "persona_uid", "started_at"]
    );
    assert_eq!(data["session"]["id"].as_str(), Some(active.as_str()));
    assert_eq!(
        data["session"]["started_at"].as_str(),
        Some("2024-06-10T08:00:00Z")
    );
    assert!(data["session"]["ended_at"].is_null());
    assert_eq!(data["session"]["persona_uid"].as_str(), Some("char-0001"));

    let messages = data["messages"].as_array().expect("messages 应为数组");
    assert_eq!(messages.len(), 3, "默认全量加载：条数不受分页上限影响");
    assert_eq!(
        sorted_keys(&messages[0]),
        vec!["content", "created_at", "id", "role", "source"]
    );
    assert_eq!(
        messages[0]["id"].as_str(),
        Some("aaaaaaaa-1111-1111-1111-111111111111")
    );
    assert_eq!(messages[0]["role"].as_str(), Some("user"));
    assert_eq!(
        messages[0]["content"].as_str(),
        Some("你好，今天想聊聊项目进度")
    );
    assert_eq!(
        messages[0]["created_at"].as_str(),
        Some("2024-06-10T08:00:00Z")
    );
    assert_eq!(messages[0]["source"].as_str(), Some("local"));
    assert_eq!(messages[1]["role"].as_str(), Some("assistant"));
    assert_eq!(
        messages[1]["created_at"].as_str(),
        Some("2024-06-10T08:01:00Z")
    );
    assert_eq!(messages[1]["source"].as_str(), Some("online"));
    assert_eq!(
        messages[2]["created_at"].as_str(),
        Some("2024-06-10T08:02:00Z")
    );
    assert_eq!(
        messages[2]["content"].as_str(),
        Some(fx.long_content.as_str()),
        "JSON 输出不截断正文"
    );

    // 已结束会话：ended_at 为 ISO 时间
    let out = run_cli(&db, &["session", "show", &ended, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = parse_envelope(&out);
    assert_eq!(
        parsed["data"]["session"]["ended_at"].as_str(),
        Some("2024-06-10T09:00:00Z")
    );
    assert_eq!(
        parsed["data"]["messages"]
            .as_array()
            .expect("messages 应为数组")
            .len(),
        1
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// session show — 文本逐字
// =========================================================

/// `session show` 文本：状态 / 创建时间 / 消息数 / 角色图标与 200 字符截断逐字一致。
#[tokio::test]
async fn session_show_text_locks_render_and_truncation() {
    let dir = temp_dir("show-text");
    let db = dir.join("session.db");
    let fx = seed_fixture(&db).await;
    let active = fx.active.to_string();
    let ended = fx.ended.to_string();

    let out = run_cli(&db, &["session", "show", &active]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains(&format!("  会话: {active}")),
        "会话行应含 id: {stdout}"
    );
    assert!(stdout.contains(&ramaria_cli::ui::labeled_line("状态", "进行中")));
    assert!(stdout.contains(&ramaria_cli::ui::labeled_line(
        "创建时间",
        "2024-06-10 08:00"
    )));
    assert!(stdout.contains(&ramaria_cli::ui::labeled_line("消息数", "3")));
    assert!(stdout.contains("\x1b[36m👤 用户\x1b[0m"));
    assert!(stdout.contains("\x1b[32m🤖 AI\x1b[0m"));

    // 超长消息按 200 字符截断（含省略号），完整正文不出现在文本输出
    let truncated = ramaria_cli::util::truncate(&fx.long_content, 200);
    assert!(truncated.ends_with('…'), "夹具应触发截断");
    assert!(stdout.contains(&truncated), "文本输出应含截断后的正文");
    assert!(!stdout.contains(&fx.long_content), "文本输出不应含完整正文");

    // 已结束会话：状态为"已结束"
    let out = run_cli(&db, &["session", "show", &ended]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains(&ramaria_cli::ui::labeled_line("状态", "已结束")));

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// session show / summarize — 不存在会话
// =========================================================

/// `session show`：不存在会话 → validation 文案 + exit 4（文本 / --json 两态）。
#[tokio::test]
async fn session_show_missing_session_reports_error() {
    let dir = temp_dir("show-missing");
    let db = dir.join("session.db");
    let _fx = seed_fixture(&db).await;
    let missing = "99999999-9999-9999-9999-999999999999";

    let out = run_cli(&db, &["session", "show", missing]);
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("会话不存在: {missing}")),
        "stderr: {stderr}"
    );
    assert!(out.stdout.is_empty(), "文本错误不应污染 stdout");

    let out = run_cli(&db, &["session", "show", missing, "--json"]);
    assert_eq!(out.status.code(), Some(4));
    let parsed = parse_envelope(&out);
    assert_eq!(parsed["ok"], false);
    assert_eq!(parsed["error"]["code"], 4);
    assert!(
        parsed["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("会话不存在: {missing}")),
        "错误信封: {parsed}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// `session summarize`：不存在会话 → validation 文案 + exit 4；空会话 → no_messages 信封。
#[tokio::test]
async fn session_summarize_missing_and_empty_sessions() {
    let dir = temp_dir("summarize");
    let db = dir.join("session.db");
    let fx = seed_fixture(&db).await;
    let empty = fx.empty.to_string();
    let missing = "99999999-9999-9999-9999-999999999999";

    // 不存在会话：文本与 JSON 两态均保持业务校验文案（不触发 LLM）
    let out = run_cli(&db, &["session", "summarize", missing]);
    assert_eq!(out.status.code(), Some(4));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(&format!("会话不存在: {missing}")),
        "stderr: {stderr}"
    );

    let out = run_cli(&db, &["session", "summarize", missing, "--json"]);
    assert_eq!(out.status.code(), Some(4));
    let parsed = parse_envelope(&out);
    assert_eq!(parsed["error"]["code"], 4);
    assert!(
        parsed["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains(&format!("会话不存在: {missing}")),
        "错误信封: {parsed}"
    );

    // 存在但无消息：成功 + 空数据信封（agent 可区分"成功但无数据"）
    let out = run_cli(&db, &["session", "summarize", &empty, "--json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let parsed = parse_envelope(&out);
    assert_eq!(parsed["ok"], true);
    assert_eq!(parsed["data"]["session_id"].as_str(), Some(empty.as_str()));
    assert_eq!(parsed["data"]["generated"], false);
    assert_eq!(parsed["data"]["reason"], "no_messages");

    let _ = std::fs::remove_dir_all(&dir);
}
