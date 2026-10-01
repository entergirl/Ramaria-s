//! crates/ramaria-mcp/tests/protocol_e2e.rs - MCP 协议层端到端用例（内存 stdio 传输）
//!
//! 定位:
//! - 覆盖"协议壳真实往返"链路：JSON-RPC 帧 → rmcp 编解码与路由 → 参数 schema 反序列化
//!   → 门禁（`enabled` / `allow_ingest` / `allow_seal`）→ 服务层用例 → 结果包装
//!   （`structuredContent` / `isError`）。本 crate 的单元测试只覆盖 schema 与注解（静态面）。
//! - 传输使用 `tokio::io::duplex` 内存管道：不启动子进程、不依赖网络与 LLM，
//!   等价于并发手测（`docs/dev-2.1/test/m4-concurrency.md`）中"客户端 → stdio → 工具调用
//!   → 回执"的链路；多服务端实例共享同一库文件，对应"两个 MCP 客户端 + 同一数据库"并存。
//! - 只使用公开 API（`RamariaMcpServer` / `Engine`），不触碰 crate 内部实现。
//!
//! 边界:
//! - 不覆盖需要真实 LLM 的路径（`chat_send` 生成、`finalize = true` 的摘要生成），
//!   该部分保留在真机验证；
//! - 每次协议往返都有等待上限（[`RPC_TIMEOUT`]）：帧丢失或服务端异常退出会给出明确
//!   panic 信息，而不是让测试挂死。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use ramaria_core::config::McpConfig;
use ramaria_core::types::{Persona, PersonaKind};
use ramaria_mcp::RamariaMcpServer;
use ramaria_service::{DEFAULT_PERSONA_UID, Engine};
use rmcp::ServiceExt;
use serde_json::{Value, json};
use tokio::io::{
    AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, Lines, ReadHalf, WriteHalf,
};
use tokio::task::JoinHandle;

/// 单次协议往返的等待上限（正常路径在毫秒级完成，此处仅用于防挂死）。
const RPC_TIMEOUT: Duration = Duration::from_secs(30);

/// 客户端协商的协议版本：取仍走标准握手流程（`initialize` → `notifications/initialized`）
/// 的版本，避免受更新规范的生命周期差异影响。
const CLIENT_PROTOCOL_VERSION: &str = "2025-06-18";

/// 内存管道容量：协议帧为小 JSON，64 KiB 足够并避免写侧阻塞。
const PIPE_CAPACITY: usize = 64 * 1024;

// =========================================================
// 协议客户端（内存 stdio）
// =========================================================

/// 工具调用回执（解析后的视图）。
struct ToolReply {
    /// `isError`：true = 工具以可操作错误返回（不是协议级错误）。
    is_error: bool,
    /// 结构化载荷（`structuredContent`；缺失时回退解析文本块内的 JSON）。
    payload: Value,
}

impl ToolReply {
    /// 从 `tools/call` 的 result 构造（兼容只带文本块的实现）。
    fn from_result(result: &Value) -> Self {
        let is_error = result
            .get("isError")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let payload = result
            .get("structuredContent")
            .cloned()
            .or_else(|| text_payload(result))
            .unwrap_or(Value::Null);
        Self { is_error, payload }
    }

    /// 读取载荷字段（缺失时为 Null）。
    fn field(&self, name: &str) -> &Value {
        self.payload.get(name).unwrap_or(&Value::Null)
    }

    /// 断言回执为成功（非错误）并返回载荷。
    fn expect_ok(&self, context: &str) -> &Value {
        assert!(
            !self.is_error,
            "{context} 不应以错误返回，结构化载荷：{}",
            self.payload
        );
        &self.payload
    }

    /// 断言回执为工具级错误并返回错误文案。
    fn expect_error(&self, context: &str) -> String {
        assert!(
            self.is_error,
            "{context} 应返回 isError=true，结构化载荷：{}",
            self.payload
        );
        self.field("error").as_str().unwrap_or_default().to_string()
    }
}

/// 回退路径：成功结果的文本块内容本身是 JSON（与结构化内容同源）。
fn text_payload(result: &Value) -> Option<Value> {
    result
        .get("content")?
        .as_array()?
        .first()?
        .get("text")?
        .as_str()
        .and_then(|text| serde_json::from_str(text).ok())
}

/// 内存 stdio 客户端：写入行分隔 JSON-RPC 帧，按 id 读取响应。
struct StdioClient {
    writer: WriteHalf<DuplexStream>,
    reader: Lines<BufReader<ReadHalf<DuplexStream>>>,
    next_id: u64,
}

impl StdioClient {
    /// 以管道一端构造客户端。
    fn new(stream: DuplexStream) -> Self {
        let (read_half, write_half) = tokio::io::split(stream);
        Self {
            writer: write_half,
            reader: BufReader::new(read_half).lines(),
            next_id: 0,
        }
    }

    /// 握手（标准顺序）：`initialize` 请求 + `notifications/initialized` 通知。
    ///
    /// 返回:
    /// - `initialize` 的 result（服务端信息 / 能力 / 使用建议）。
    async fn handshake(&mut self, client_name: &str) -> Value {
        let result = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": CLIENT_PROTOCOL_VERSION,
                    "capabilities": {},
                    "clientInfo": { "name": client_name, "version": "0.1.0" },
                }),
            )
            .await;
        self.notify("notifications/initialized").await;
        result
    }

    /// 发送请求并等待同 id 的响应 result（协议级错误直接 panic）。
    async fn request(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id();
        self.send(&json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        }))
        .await;

        let response = self.read_until_response(id, method).await;
        match response.get("result") {
            Some(result) => result.clone(),
            None => panic!(
                "{method} 返回协议级错误：{}",
                response.get("error").cloned().unwrap_or(Value::Null)
            ),
        }
    }

    /// 调用工具并解析回执（`isError` + 结构化载荷）。
    async fn call_tool(&mut self, name: &str, arguments: Value) -> ToolReply {
        let result = self
            .request(
                "tools/call",
                json!({ "name": name, "arguments": arguments }),
            )
            .await;
        ToolReply::from_result(&result)
    }

    /// 发送通知（无 id，不等待响应）。
    async fn notify(&mut self, method: &str) {
        self.send(&json!({ "jsonrpc": "2.0", "method": method }))
            .await;
    }

    /// 递增并返回下一个请求 id。
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }

    /// 写入一帧（行分隔 JSON）。
    async fn send(&mut self, message: &Value) {
        let mut frame = serde_json::to_string(message).expect("协议帧应可序列化");
        frame.push('\n');
        self.writer
            .write_all(frame.as_bytes())
            .await
            .expect("写入协议帧应成功（内存管道）");
        self.writer
            .flush()
            .await
            .expect("刷新协议帧应成功（内存管道）");
    }

    /// 读取到指定 id 的响应（跳过通知与空行；超时 / 断流给出明确 panic）。
    async fn read_until_response(&mut self, id: u64, method: &str) -> Value {
        let deadline = tokio::time::Instant::now() + RPC_TIMEOUT;
        loop {
            let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
            let line = match tokio::time::timeout(remaining, self.reader.next_line()).await {
                Ok(Ok(Some(line))) => line,
                Ok(Ok(None)) => {
                    panic!("协议流在收到 {method}（id={id}）响应前关闭（服务端可能已退出）")
                }
                Ok(Err(e)) => panic!("读取 {method}（id={id}）的协议行失败：{e}"),
                Err(_) => panic!("等待 {method}（id={id}）响应超时（{RPC_TIMEOUT:?}）"),
            };

            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            let value: Value = match serde_json::from_str(trimmed) {
                Ok(value) => value,
                Err(e) => panic!("服务端输出不是 JSON 协议帧：{e}；原始行：{trimmed}"),
            };
            // 串行请求模型：只关心同 id 的响应，其余（通知 / 交叉响应）跳过
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                return value;
            }
        }
    }
}

// =========================================================
// 临时库与服务端辅助
// =========================================================

/// 临时库目录（drop 时尽力清理；句柄未释放导致删除失败不视为测试失败）。
struct TempDb {
    dir: PathBuf,
    db_path: PathBuf,
}

impl TempDb {
    /// 在系统临时目录下创建唯一库目录（`tag` 仅用于人工定位残留目录）。
    fn new(tag: &str) -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应可读取")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-mcp-e2e-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("临时目录应可创建");
        Self {
            db_path: dir.join("assistant.db"),
            dir,
        }
    }

    /// 库文件路径。
    fn path(&self) -> &Path {
        &self.db_path
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        // Windows 下 SQLite 连接池句柄可能延迟释放：清理失败忽略即可
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// 默认配置 + 总开关开启（其余键沿用默认：允许写入 / 允许封存 / 白名单 `*`）。
fn enabled_config() -> McpConfig {
    McpConfig {
        enabled: true,
        ..McpConfig::default()
    }
}

/// 装配服务层引擎（首次打开自动建库并执行 migration）。
async fn open_engine(db_path: &Path) -> Arc<Engine> {
    Arc::new(
        Engine::open(db_path)
            .await
            .expect("服务层引擎应可装配（含建库与 migration）"),
    )
}

/// 在库内种一个可用人格（回流写入的归属校验与消息外键都依赖它）。
async fn seed_persona(engine: &Engine, uid: &str) {
    let persona = Persona::new(
        uid.to_string(),
        "协议测试人格".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    engine
        .storage()
        .create_persona(&persona)
        .await
        .expect("插入人格应成功");
}

/// 起一台 MCP 服务端（内存 stdio 传输），返回未握手的客户端与服务端任务句柄。
///
/// 说明:
/// - `serve` 会等待客户端的首个 `initialize` 请求后才返回，因此服务端放入后台任务：
///   调用方随后用 [`StdioClient::handshake`] 发起握手，双方才不会互相等待；
/// - 返回的 [`JoinHandle`] 只需持有到测试结束：客户端断开（管道 EOF）后服务循环自行退出。
///
/// 返回:
/// - `StdioClient`: 客户端（需调用方自行握手）；
/// - `JoinHandle<()>`: 服务端任务（drop 即 detach，不阻塞测试退出）。
async fn start_server(engine: Arc<Engine>, config: McpConfig) -> (StdioClient, JoinHandle<()>) {
    let (server_io, client_io) = tokio::io::duplex(PIPE_CAPACITY);
    let (server_read, server_write) = tokio::io::split(server_io);

    // 与宿主装配同口径：门禁先注入服务层再构造协议壳（本文件直接构造服务端，
    // 不经过 `serve_stdio`，需自行补上宿主步骤；否则封存门禁默认放行）。
    engine.set_seal_allowed(config.allow_seal);
    let server = RamariaMcpServer::new(engine, config);
    let server_task = tokio::spawn(async move {
        match server.serve((server_read, server_write)).await {
            Ok(running) => {
                // 持有运行句柄直到传输关闭（客户端 drop 管道端）
                let _ = running.waiting().await;
            }
            Err(e) => panic!("MCP 服务端初始化失败：{e}"),
        }
    });

    (StdioClient::new(client_io), server_task)
}

// =========================================================
// 用例：握手与工具清单
// =========================================================

/// 握手后可正常调用，且工具清单与契约一致（六个工具）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_and_tools_list_follow_contract() {
    let db = TempDb::new("handshake");
    let engine = open_engine(db.path()).await;
    let (mut client, _server) = start_server(engine, enabled_config()).await;

    // ---- 握手：服务端信息 / 能力 / 使用建议 ----
    let init = client.handshake("protocol-test").await;
    assert_eq!(
        init.get("serverInfo")
            .and_then(|info| info.get("name"))
            .and_then(Value::as_str),
        Some("ramaria"),
        "initialize 应返回服务端标识：{init}"
    );
    assert!(
        init.get("capabilities").is_some(),
        "initialize 应声明能力：{init}"
    );
    let instructions = init
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    assert!(
        instructions.contains("memory_recall"),
        "使用建议应指引工具调用时机：{instructions}"
    );

    // ---- 工具清单：名称集合与数量 ----
    let listed = client.request("tools/list", json!({})).await;
    let names: Vec<String> = listed
        .get("tools")
        .and_then(Value::as_array)
        .expect("tools/list 应返回工具数组")
        .iter()
        .filter_map(|tool| tool.get("name").and_then(Value::as_str).map(str::to_string))
        .collect();
    for expected in [
        "memory_recall",
        "chat_send",
        "chat_ingest",
        "persona_list",
        "persona_get",
        "chat_history",
    ] {
        assert!(
            names.contains(&expected.to_string()),
            "工具清单缺少 {expected}：{names:?}"
        );
    }
    assert_eq!(names.len(), 6, "工具数量应与契约一致：{names:?}");
}

// =========================================================
// 用例：回流写入与历史回看（协议往返）
// =========================================================

/// 回流写入 → 历史回看：消息经协议写入后可按会话读回（回流可见的协议层证据）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn ingest_then_history_roundtrip() {
    let db = TempDb::new("ingest-history");
    let engine = open_engine(db.path()).await;
    seed_persona(&engine, DEFAULT_PERSONA_UID).await;
    let (mut client, _server) = start_server(engine, enabled_config()).await;
    client.handshake("client-a").await;

    // ---- 回流写入 ----
    let reply = client
        .call_tool(
            "chat_ingest",
            json!({
                "messages": [
                    { "role": "user", "content": "我最近在学 Rust" },
                    { "role": "assistant", "content": "记得你说过想系统学一遍" },
                ],
                "conversation_id": "conv-roundtrip",
            }),
        )
        .await;
    reply.expect_ok("chat_ingest");
    assert_eq!(reply.field("written").as_u64(), Some(2), "两条消息都应写入");
    assert_eq!(
        reply.field("deduplicated").as_u64(),
        Some(0),
        "首次提交不应有去重"
    );
    let session_id = reply
        .field("session_id")
        .as_str()
        .expect("回执应带 session_id")
        .to_string();

    // ---- 历史回看：角色与内容原样可见 ----
    let history = client
        .call_tool(
            "chat_history",
            json!({ "session_id": session_id, "limit": 10 }),
        )
        .await;
    history.expect_ok("chat_history");
    assert_eq!(history.field("total").as_u64(), Some(2), "应有两条消息");
    let messages = history
        .field("messages")
        .as_array()
        .expect("history 应返回消息数组");
    assert_eq!(messages.len(), 2, "页内应返回两条消息");
    assert_eq!(
        messages[0].get("role").and_then(Value::as_str),
        Some("user"),
        "首条应为用户消息：{messages:?}"
    );
    assert_eq!(
        messages[0].get("content").and_then(Value::as_str),
        Some("我最近在学 Rust")
    );
    assert_eq!(
        messages[1].get("content").and_then(Value::as_str),
        Some("记得你说过想系统学一遍")
    );
}

/// 重复提交同一段对话：写入 0 条、全部计入去重（幂等语义经协议层验证）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn resubmit_is_idempotent() {
    let db = TempDb::new("resubmit");
    let engine = open_engine(db.path()).await;
    seed_persona(&engine, DEFAULT_PERSONA_UID).await;
    let (mut client, _server) = start_server(engine, enabled_config()).await;
    client.handshake("client-a").await;

    let payload = json!({
        "messages": [
            { "role": "user", "content": "第一句" },
            { "role": "assistant", "content": "第一句回复" },
        ],
        "conversation_id": "conv-resubmit",
    });
    client
        .call_tool("chat_ingest", payload.clone())
        .await
        .expect_ok("首次 chat_ingest");

    let again = client.call_tool("chat_ingest", payload).await;
    again.expect_ok("重发 chat_ingest");
    assert_eq!(again.field("written").as_u64(), Some(0), "重发不应新增消息");
    assert_eq!(
        again.field("deduplicated").as_u64(),
        Some(2),
        "重发两条都应计入去重"
    );
}

// =========================================================
// 用例：两个服务端实例共享同一库（多客户端并存）
// =========================================================

/// 两个服务端实例（各自连接池）并发回流同一库：均成功、各自成会话（无写锁失败）。
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn two_servers_share_one_db_without_write_conflict() {
    let db = TempDb::new("two-servers");
    let engine_a = open_engine(db.path()).await;
    seed_persona(&engine_a, DEFAULT_PERSONA_UID).await;
    // 第二台引擎：同一库文件、独立连接池与内存状态（模拟第二个客户端进程）
    let engine_b = open_engine(db.path()).await;

    let (mut client_a, _server_a) = start_server(engine_a, enabled_config()).await;
    let (mut client_b, _server_b) = start_server(engine_b, enabled_config()).await;
    client_a.handshake("client-a").await;
    client_b.handshake("client-b").await;

    // 并发写入：写锁竞争由连接池等待 + 忙重试覆盖，两侧都不应失败
    let (reply_a, reply_b) = tokio::join!(
        client_a.call_tool(
            "chat_ingest",
            json!({
                "messages": [{ "role": "user", "content": "客户端 A 的第一句" }],
                "conversation_id": "conv-a",
            }),
        ),
        client_b.call_tool(
            "chat_ingest",
            json!({
                "messages": [{ "role": "user", "content": "客户端 B 的第一句" }],
                "conversation_id": "conv-b",
            }),
        ),
    );
    reply_a.expect_ok("客户端 A chat_ingest");
    reply_b.expect_ok("客户端 B chat_ingest");
    assert_eq!(reply_a.field("written").as_u64(), Some(1));
    assert_eq!(reply_b.field("written").as_u64(), Some(1));
    assert_ne!(
        reply_a.field("session_id"),
        reply_b.field("session_id"),
        "不同外部对话标识应各自成会话"
    );

    // 交叉回看：B 写入的内容在 A 侧服务端也可读（同一库、无内存态依赖）
    let session_b = reply_b
        .field("session_id")
        .as_str()
        .expect("回执应带 session_id")
        .to_string();
    let history = client_a
        .call_tool("chat_history", json!({ "session_id": session_b }))
        .await;
    history.expect_ok("跨客户端 chat_history");
    assert_eq!(
        history.field("total").as_u64(),
        Some(1),
        "A 侧应能读到 B 侧写入的消息"
    );
}

// =========================================================
// 用例：门禁与降级（开关关闭时的可操作错误）
// =========================================================

/// 总开关关闭：全部工具返回可操作错误（指引去桌面设置开启）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disabled_switch_blocks_all_tools() {
    let db = TempDb::new("gate-disabled");
    let engine = open_engine(db.path()).await;
    // `McpConfig::default()` 即总开关关闭（读侧保守、写侧放开但不生效）
    let (mut client, _server) = start_server(engine, McpConfig::default()).await;
    client.handshake("client-a").await;

    let recall = client
        .call_tool("memory_recall", json!({ "messages": [] }))
        .await;
    let message = recall.expect_error("memory_recall（总开关关闭）");
    assert!(
        message.contains("设置"),
        "错误应指引用户去面板开启：{message}"
    );

    let ingest = client
        .call_tool(
            "chat_ingest",
            json!({ "messages": [{ "role": "user", "content": "hi" }] }),
        )
        .await;
    ingest.expect_error("chat_ingest（总开关关闭）");
}

/// 写侧治理开关：`allow_ingest = false` 拒绝写入但读工具可用；
/// `allow_seal = false` 时 `finalize = true` 降级为"只写不封存"并在回执中说明。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn write_gates_are_enforced_per_switch() {
    let db = TempDb::new("gate-write");
    let engine = open_engine(db.path()).await;
    seed_persona(&engine, DEFAULT_PERSONA_UID).await;

    // ---- allow_ingest = false：写工具拒绝，读工具照常 ----
    let mut no_ingest = enabled_config();
    no_ingest.allow_ingest = false;
    let (mut client, _server) = start_server(Arc::clone(&engine), no_ingest).await;
    client.handshake("client-a").await;

    let ingest = client
        .call_tool(
            "chat_ingest",
            json!({ "messages": [{ "role": "user", "content": "不应写入" }] }),
        )
        .await;
    let message = ingest.expect_error("chat_ingest（写入开关关闭）");
    assert!(message.contains("写入"), "错误应说明写入被禁用：{message}");
    client
        .call_tool("memory_recall", json!({ "messages": [] }))
        .await
        .expect_ok("memory_recall（写入开关关闭不影响读）");

    // ---- allow_seal = false：finalize 降级为只写不封存 ----
    let mut no_seal = enabled_config();
    no_seal.allow_seal = false;
    let (mut client, _server) = start_server(engine, no_seal).await;
    client.handshake("client-b").await;

    let reply = client
        .call_tool(
            "chat_ingest",
            json!({
                "messages": [{ "role": "user", "content": "只写不封存" }],
                "conversation_id": "conv-seal-off",
                "finalize": true,
            }),
        )
        .await;
    reply.expect_ok("chat_ingest（封存开关关闭）");
    assert_eq!(
        reply.field("finalized").as_bool(),
        Some(false),
        "封存开关关闭时不应触发封存"
    );
    assert_eq!(reply.field("written").as_u64(), Some(1), "写入仍应完成");
    let note = reply.field("note").as_str().unwrap_or_default();
    assert!(note.contains("封存"), "回执应说明未封存的原因：{note}");
}

// =========================================================
// 用例：召回概览（读链路经协议可达）
// =========================================================

/// 空记忆概览：不报错、进入 overview 模式、条目为空（协议 → 召回用例 → 结果包装全链路）。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn recall_overview_on_empty_db() {
    let db = TempDb::new("recall-overview");
    let engine = open_engine(db.path()).await;
    seed_persona(&engine, DEFAULT_PERSONA_UID).await;
    let (mut client, _server) = start_server(engine, enabled_config()).await;
    client.handshake("client-a").await;

    let reply = client
        .call_tool("memory_recall", json!({ "messages": [] }))
        .await;
    reply.expect_ok("memory_recall（空库概览）");
    assert_eq!(
        reply.field("stats").get("mode").and_then(Value::as_str),
        Some("overview"),
        "无 query 且无对话片段时应进入概览模式"
    );
    assert!(
        reply
            .field("items")
            .as_array()
            .expect("items 应为数组")
            .is_empty(),
        "空库不应返回条目"
    );
    assert!(
        reply.field("context").is_string(),
        "context 应为可拼接的文本字段"
    );
}

// =========================================================
// 用例：工具级错误文案（业务错误原文直出）
// =========================================================

/// 服务层业务错误原样作为工具错误文案（不叠加类别前缀），并保留可操作后缀。
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn persona_get_missing_uid_reports_business_message() {
    let db = TempDb::new("persona-missing");
    let engine = open_engine(db.path()).await;
    seed_persona(&engine, DEFAULT_PERSONA_UID).await;
    let (mut client, _server) = start_server(engine, enabled_config()).await;
    client.handshake("client-a").await;

    let reply = client
        .call_tool("persona_get", json!({ "uid": "char-missing" }))
        .await;
    let message = reply.expect_error("persona_get（人格不存在）");
    assert!(
        message.starts_with("人格不存在: char-missing"),
        "业务错误应原文直出: {message}"
    );
    assert!(
        !message.contains("validation error"),
        "不应包含英文类别串: {message}"
    );
    assert!(
        message.contains("persona_list"),
        "应保留可操作后缀: {message}"
    );
}
