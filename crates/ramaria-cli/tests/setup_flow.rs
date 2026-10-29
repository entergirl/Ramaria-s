//! tests/setup_flow.rs - Ramaria 首次配置到对话的进程级回归
//!
//! 设计特点:
//! - 夹具自建：唯一临时目录 + 本地 mock HTTP 服务，不依赖外部网络与真实模型
//! - 同进程路径：真实 Engine 跑完 setup 后直接 ask，验证配置热替换后对话可用
//! - 新进程路径：真实 CLI 二进制分两次进程跑 setup 与 ask，验证落库配置被下次启动读到
//! - 重跑与标记：重跑向导保留 embedding_model_path，完成标记为毫秒时间戳
//! - 只用本地 LmStudio provider：不触碰 OS keychain，不写真实用户库
//!
//! 安全约束:
//! - mock 服务只监听 127.0.0.1，返回固定回复 "pong"，不记录请求正文
//! - 临时目录按进程 id + 原子序号隔离，用例结束删除

use futures::StreamExt;
use ramaria_core::traits::{StoreCrud, StoreInfrastructure};
use ramaria_core::types::{AppState, LlmProvider, Persona, PersonaKind, now_ms};
use ramaria_service::{ChatStreamRequest, Engine, EngineOptions, SetupRequest, StreamEvent};
use ramaria_storage::SqliteStorage;
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// 临时目录序号（同进程并行建目录时避免撞名）。
static TMP_SEQ: AtomicU32 = AtomicU32::new(0);

/// 创建本次用例独占的临时目录（名内带进程 id 与原子序号）。
fn temp_dir(tag: &str) -> PathBuf {
    let seq = TMP_SEQ.fetch_add(1, Ordering::Relaxed);
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "ramaria-setup-flow-{tag}-{}-{seq}-{millis}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("创建临时目录应成功");
    dir
}

/// 以真实 CLI 二进制运行（工作目录限定在临时目录，stdin 置空避免等待输入）。
fn run_cli(cwd: &Path, db: &Path, args: &[&str]) -> std::process::Output {
    std::process::Command::new(env!("CARGO_BIN_EXE_ramaria"))
        .current_dir(cwd)
        .args(args)
        .arg("--db")
        .arg(db)
        .stdin(std::process::Stdio::null())
        .output()
        .expect("运行 ramaria 二进制失败")
}

/// 断言进程成功退出，失败时带上 stderr。
fn assert_success(out: &std::process::Output, context: &str) {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{context} 应成功退出；stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// 在临时目录写入仓库样例人格文件，供 CLI 扫描 `config/personas/`。
fn seed_rama_persona_file(root: &Path) {
    let dir = root.join("config").join("personas");
    std::fs::create_dir_all(&dir).expect("创建 personas 目录应成功");
    std::fs::write(
        dir.join("rama-0001.toml"),
        include_str!("../../../config/personas/rama-0001.toml"),
    )
    .expect("写入样例人格文件应成功");
}

/// 打开真实 SQLite 库（自动 migration），返回存储句柄与连接池（用完关闭）。
async fn open_storage(db: &Path) -> (SqliteStorage, sqlx::SqlitePool) {
    let pool = ramaria_storage::database::init_pool(Some(db.to_path_buf()))
        .await
        .expect("初始化测试数据库失败");
    (SqliteStorage::new(pool.clone()), pool)
}

/// 播种 Ramaria 人格（供同进程用例直接对话）。
async fn seed_rama_persona(db: &Path) {
    let (storage, pool) = open_storage(db).await;
    let mut persona = Persona::new(
        "rama-0001".to_string(),
        "黎杋枫".to_string(),
        PersonaKind::Rama,
        1,
        "file".to_string(),
    );
    persona.config = Some(include_str!("../../../config/personas/rama-0001.toml").to_string());
    storage
        .create_persona(&persona)
        .await
        .expect("写入测试人格应成功");
    pool.close().await;
}

// =========================================================
// 本地 mock LLM（GET 健康探测 / POST 流式与非流式）
// =========================================================

/// 启动本地 mock LLM，返回 `http://127.0.0.1:{port}/v1`。
///
/// 说明:
/// - GET（健康探测）→ 200 `{}`；
/// - POST（chat）按请求体 `stream` 返回 SSE 或普通 JSON，回复固定为 `pong`；
/// - 线程常驻到测试进程结束，不 join。
fn spawn_mock_llm() -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("绑定 mock 端口应成功");
    let port = listener.local_addr().expect("读取 mock 地址应成功").port();
    std::thread::spawn(move || {
        for incoming in listener.incoming() {
            let Ok(stream) = incoming else { continue };
            std::thread::spawn(move || handle_connection(stream));
        }
    });
    format!("http://127.0.0.1:{port}/v1")
}

/// 处理单个连接：先按 header 判定方法，再按 Content-Length 读满 body。
fn handle_connection(mut stream: TcpStream) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(10)));
    let Some((head, body)) = read_request(&mut stream) else {
        return;
    };
    let method = head
        .lines()
        .next()
        .unwrap_or_default()
        .split_whitespace()
        .next()
        .unwrap_or_default();
    if method == "GET" {
        respond(&mut stream, "application/json", "{}");
        return;
    }

    let streaming = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|value| value.get("stream").and_then(|flag| flag.as_bool()))
        .unwrap_or(false);
    if streaming {
        respond(
            &mut stream,
            "text/event-stream",
            "data: {\"choices\":[{\"delta\":{\"content\":\"pong\"},\"finish_reason\":null}]}\n\ndata: [DONE]\n\n",
        );
    } else {
        respond(
            &mut stream,
            "application/json",
            "{\"choices\":[{\"message\":{\"content\":\"pong\"}}]}",
        );
    }
}

/// 读取请求头与 body；连接提前关闭时返回 None。
fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(pos) = buffer.windows(4).position(|w| w == b"\r\n\r\n") {
            break pos;
        }
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            return None;
        }
        buffer.extend_from_slice(&chunk[..read]);
    };

    let head = String::from_utf8_lossy(&buffer[..header_end]).to_string();
    let body_start = header_end + 4;
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            if name.eq_ignore_ascii_case("content-length") {
                value.trim().parse::<usize>().ok()
            } else {
                None
            }
        })
        .unwrap_or(0);

    let mut body = buffer[body_start..].to_vec();
    while body.len() < content_length {
        let read = stream.read(&mut chunk).ok()?;
        if read == 0 {
            break;
        }
        body.extend_from_slice(&chunk[..read]);
    }
    body.truncate(content_length);
    Some((head, body))
}

/// 写响应（带 Content-Length 与 Connection: close）并关闭连接。
fn respond(stream: &mut TcpStream, content_type: &str, body: &str) {
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    let _ = stream.write_all(response.as_bytes());
    let _ = stream.flush();
    let _ = stream.shutdown(Shutdown::Both);
}

// =========================================================
// 用例
// =========================================================

/// 同进程：配置完成后 ask 立即成功（流式事件含 pong 且无 Error）。
#[tokio::test]
async fn setup_then_ask_in_same_process() {
    let dir = temp_dir("same-process");
    let db = dir.join("ramaria.db");
    let base_url = spawn_mock_llm();
    let engine = Arc::new(
        Engine::open_with(EngineOptions::new(&db).with_config_path(dir.join("config.toml")))
            .await
            .expect("装配测试引擎应成功"),
    );
    seed_rama_persona(&db).await;

    let state = engine
        .run_setup(&SetupRequest {
            provider: LlmProvider::LmStudio,
            model_id: "local-model".to_string(),
            base_url,
            api_key: None,
            embedding_model_path: None,
            skip_validate: false,
        })
        .await
        .expect("首次配置应成功");
    assert_ne!(state, AppState::NeedsSetup, "配置后不应仍处于待配置状态");
    assert_ne!(
        engine.current_state(),
        AppState::NeedsSetup,
        "引擎状态应随配置推进"
    );

    let handle = engine
        .chat_stream(ChatStreamRequest {
            message: "你好".to_string(),
            persona: Some("rama-0001".to_string()),
            session_id: None,
            seed_history: Vec::new(),
            config_override: None,
        })
        .await
        .expect("发送消息应成功");

    let mut events = handle.events;
    let mut reply = String::new();
    while let Some(item) = events.next().await {
        match item {
            Ok(StreamEvent::Delta { content, .. }) => reply.push_str(&content),
            Ok(StreamEvent::Error { error, .. }) => panic!("对话不应返回错误事件: {error}"),
            Ok(_) => {}
            Err(e) => panic!("事件流不应中断: {e}"),
        }
    }
    assert!(reply.contains("pong"), "回复应来自 mock LLM: {reply}");

    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    let _ = std::fs::remove_dir_all(&dir);
}

/// 新进程：setup 落库后另起进程 ask 成功（配置被下次启动读到）。
#[test]
fn setup_then_new_process_ask_succeeds() {
    let dir = temp_dir("new-process");
    let db = dir.join("ramaria.db");
    seed_rama_persona_file(&dir);
    let base_url = spawn_mock_llm();

    let out = run_cli(
        &dir,
        &db,
        &[
            "setup",
            "--provider",
            "lm_studio",
            "--base-url",
            &base_url,
            "--no-validate",
        ],
    );
    assert_success(&out, "非交互首次配置");

    let out = run_cli(&dir, &db, &["ask", "你好", "--no-stream"]);
    assert_success(&out, "新进程对话");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("pong"), "对话输出应含 mock 回复: {stdout}");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 重跑：第二次不带 --embedding-path 时保留首次写入的嵌入模型路径。
#[tokio::test]
async fn rerun_setup_keeps_embedding_model_path() {
    let dir = temp_dir("rerun");
    let db = dir.join("ramaria.db");
    let model_dir = dir.join("embedding-model");
    std::fs::create_dir_all(&model_dir).expect("创建嵌入模型目录应成功");
    let model_path = model_dir
        .to_str()
        .expect("临时路径应为合法 UTF-8")
        .to_string();
    let base_url = spawn_mock_llm();

    let out = run_cli(
        &dir,
        &db,
        &[
            "setup",
            "--provider",
            "lm_studio",
            "--base-url",
            &base_url,
            "--embedding-path",
            &model_path,
            "--no-validate",
        ],
    );
    assert_success(&out, "首次配置（带嵌入路径）");

    let out = run_cli(
        &dir,
        &db,
        &[
            "setup",
            "--provider",
            "lm_studio",
            "--base-url",
            &base_url,
            "--no-validate",
        ],
    );
    assert_success(&out, "重跑配置（不带嵌入路径）");

    let (storage, pool) = open_storage(&db).await;
    let config = storage
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应存在");
    assert_eq!(
        config.embedding_model_path.as_deref(),
        Some(model_path.as_str()),
        "重跑不应丢失嵌入模型路径"
    );
    pool.close().await;

    let _ = std::fs::remove_dir_all(&dir);
}

/// 完成标记：settings.setup_completed_at 为 13 位毫秒时间戳。
#[tokio::test]
async fn setup_writes_millisecond_completion_marker() {
    let dir = temp_dir("marker");
    let db = dir.join("ramaria.db");
    let base_url = spawn_mock_llm();

    let out = run_cli(
        &dir,
        &db,
        &[
            "setup",
            "--provider",
            "lm_studio",
            "--base-url",
            &base_url,
            "--no-validate",
        ],
    );
    assert_success(&out, "首次配置");

    let (storage, pool) = open_storage(&db).await;
    let marker = storage
        .get_setting(ramaria_service::SETUP_COMPLETED_KEY)
        .await
        .expect("读取完成标记应成功")
        .expect("完成标记应存在");
    assert_eq!(marker.len(), 13, "完成标记应为 13 位毫秒时间戳: {marker}");
    let completed_at = marker.parse::<i64>().expect("完成标记应为整数");
    let now = now_ms();
    assert!(
        completed_at >= now - 60_000 && completed_at <= now + 60_000,
        "完成标记应落在当前时间附近: {completed_at} vs {now}"
    );
    pool.close().await;

    let _ = std::fs::remove_dir_all(&dir);
}
