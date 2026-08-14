//! rust/crates/ramaria-app/tests/m6_integration.rs - v1.4/v1.5 M6 集成测试
//!
//! 覆盖:
//! - T-V14-6-001/002/003：四层模板（活跃路径端到端）
//!   - 段落标题对齐 v3.1 §8.2（# 角色（行为层）/ # 说话风格（表达层）/
//!     # 记忆（脉络层）/ # 当前时间；# 知识（知识层，按需）槽位为空不产生段落）
//!   - 原文片段与桥接并存（utt 检索 + 桥接加载双通道）
//! - T-V14-6-004：配置传播（[utt]/[examples]/[bridge] 开关逐一断言 + 关闭回退 v1.3）
//!   - utt.enabled=false → 无原文片段（行为回退 v1.3）
//!   - bridge.enabled=false → 无桥接段落
//!   - examples.enabled=false → 回退静态 selected 查询（v1.3 行为）
//!   - examples.max_examples 传播（注入条数上限生效）
//!   - 三开关全关 → prompt 无任何 v1.4 新增段落（语义等价 v1.3）
//! - T-V15-6-001/002：驱动环接线（v1.5 M6，F 任务）
//!   - 情境路由命中 → `## 行为规则` 行为块注入 prompt（reaction + params + avoid）
//!   - [behavior].enabled=false → 无行为块（prompt 与 v1.4 语义等价——回归红线）
//!   - 无规则（助手 persona）→ 无行为块（无回归）
//!
//! 安全约束:
//! - 全部使用 mock（MockStorage + MockLlm），不触碰真实数据库/LLM。
//! - 原文内容不写日志（断言不涉及日志内容）。

#![allow(unused_must_use)]

mod mock_backend;

use futures::StreamExt;
use ramaria_app::App;
use ramaria_app::stream_event::StreamEvent;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    AppState, BackendConfig, Persona, PersonaExample, PersonaKind, UttBlock,
};
use ramaria_llm::keychain::Keychain;
use std::sync::Arc;
use uuid::Uuid;

use mock_backend::{MockLlm, MockStorage};

// =========================================================
// 辅助函数
// =========================================================

/// 构造指定配置的 App（MockStorage + MockLlm）。
fn make_app(storage: Arc<MockStorage>, llm: Arc<MockLlm>, config: RamariaConfig) -> App {
    let keychain = Arc::new(Keychain::new());
    App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    )
}

/// 使 App 进入对话可用状态（写入后端配置 + Ready）。
async fn setup_ready(app: &App, storage: &dyn StorageBackend) {
    storage
        .save_backend_config(&BackendConfig::lm_studio_default())
        .await
        .unwrap();
    app.refresh_setup_state().await.unwrap();
    app.set_state(AppState::Ready);
}

/// 发送消息并消费完整个事件流（无 session_id → 创建新会话）。
async fn send_and_drain(app: &App, text: &str, persona: Option<&str>) {
    let mut stream = app.send_message(text, persona, None).await.unwrap();
    while stream.next().await.is_some() {}
}

/// 注册角色类 persona（含 speaking_style，激活表达层段落）。
fn add_char_persona(storage: &MockStorage) {
    let mut p = Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    p.config =
        Some(r#"{"description":"测试角色","speaking_style":"热情活泼，喜欢用emoji"}"#.into());
    storage.add_persona(p);
}

/// 构造角色类 persona 的上一会话 + utt 块（桥接与 utt 检索共用数据源）。
fn add_prev_session_with_utt(storage: &MockStorage) {
    let prev = Uuid::new_v4();
    storage.add_closed_session(prev);
    storage.add_utt_block(UttBlock {
        id: 1,
        persona_uid: "char-0001".to_string(),
        session_id: prev,
        start_msg_id: Uuid::new_v4(),
        end_msg_id: Uuid::new_v4(),
        block_text: "[2026-08-01 20:00] 小夏: 上次我们聊到海边\n[2026-08-01 20:01] 用户: 嗯嗯"
            .to_string(),
        msg_count: 2,
        time_span_ms: 60_000,
        embedding: None,
        created_at: 1_700_000_000_000,
    });
}

/// 构造候选示例（tags 逗号分隔，selected 可控）。
fn make_example(partner: &str, reply: &str, tags: Option<&str>, selected: bool) -> PersonaExample {
    let mut e = PersonaExample::new(
        "char-0001".to_string(),
        partner.to_string(),
        reply.to_string(),
    );
    e.tags = tags.map(|s| s.to_string());
    e.selected = selected;
    e
}

// =========================================================
// T-V14-6-001/002/003：四层模板端到端
// =========================================================

/// 角色类 persona 全链路（utt 块 + 桥接 + 表达层）：四层段落齐全，
/// 知识槽位为空不产生段落。
#[tokio::test]
async fn four_layer_template_rendered_in_active_path() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(
        Arc::clone(&storage),
        Arc::clone(&llm),
        RamariaConfig::default(),
    );
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    add_prev_session_with_utt(&storage);
    // utt 块入检索索引（App 启动链路不自动重建，测试显式调用）
    app.rebuild_retriever().await.unwrap();

    send_and_drain(&app, "继续上次的话题吧", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;

    // ---- 四层段落标题（v3.1 §8.2） ----
    assert!(prompt.contains("# 能力边界"), "能力边界段缺失");
    assert!(prompt.contains("# 角色（行为层）"), "角色层缺失");
    assert!(prompt.contains("# 说话风格（表达层）"), "表达层缺失");
    assert!(prompt.contains("# 记忆（脉络层）"), "脉络层缺失");
    assert!(prompt.contains("# 当前时间"), "当前时间段缺失");

    // ---- 知识槽位为空 → 不产生段落（T-V14-6-003） ----
    assert!(
        !prompt.contains("# 知识（知识层，按需）"),
        "知识槽位为空不应产生段落"
    );

    // ---- 原文片段 + 桥接并存（双通道） ----
    assert!(prompt.contains("## 原文片段"), "utt 检索命中应注入原文片段");
    assert!(prompt.contains("## 桥接（上一会话尾部）"), "桥接应注入");
    assert!(prompt.contains("上次我们聊到海边"), "桥接内容保留");

    // ---- 表达层内容（speaking_style） ----
    assert!(prompt.contains("热情活泼"), "表达层应含 speaking_style");
}

// =========================================================
// T-V14-6-004：配置传播
// =========================================================

/// [utt].enabled=false → 不检索不注入原文片段（行为回退 v1.3）。
#[tokio::test]
async fn utt_disabled_falls_back_to_v13() {
    let mut cfg = RamariaConfig::default();
    cfg.utt.enabled = false;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    add_prev_session_with_utt(&storage);
    app.rebuild_retriever().await.unwrap();

    send_and_drain(&app, "继续上次的话题吧", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(
        !prompt.contains("## 原文片段"),
        "utt.enabled=false 不应注入原文片段"
    );
    // 桥接开关独立：仍应注入（互不干扰）
    assert!(
        prompt.contains("## 桥接（上一会话尾部）"),
        "桥接不受 utt 开关影响"
    );
}

/// [bridge].enabled=false → 不加载桥接（行为回退 v1.3）。
#[tokio::test]
async fn bridge_disabled_falls_back_to_v13() {
    let mut cfg = RamariaConfig::default();
    cfg.bridge.enabled = false;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    add_prev_session_with_utt(&storage);
    app.rebuild_retriever().await.unwrap();

    send_and_drain(&app, "继续上次的话题吧", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(
        !prompt.contains("## 桥接（上一会话尾部）"),
        "bridge.enabled=false 不应注入桥接"
    );
    // utt 检索不受影响
    assert!(
        prompt.contains("## 原文片段"),
        "utt 注入不受 bridge 开关影响"
    );
}

/// [examples].enabled=false → 回退 v1.3 静态 selected 查询：
/// 仅 selected 示例注入，候选池（未选中）不注入。
#[tokio::test]
async fn examples_disabled_uses_static_selected() {
    let mut cfg = RamariaConfig::default();
    cfg.examples.enabled = false;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    // 候选池：1 条 selected + 2 条未选中
    storage.add_example(
        "char-0001",
        make_example("你好", "静态选中示例回复", Some("问候"), true),
    );
    storage.add_example(
        "char-0001",
        make_example("在吗", "候选池未选中一", None, false),
    );
    storage.add_example(
        "char-0001",
        make_example("干嘛", "候选池未选中二", None, false),
    );

    // 记忆未命中（检索器空）→ v1.3 路径仍无条件注入 selected
    send_and_drain(&app, "你好", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(prompt.contains("静态选中示例回复"), "静态 selected 应注入");
    assert!(!prompt.contains("候选池未选中"), "未选中示例不应注入");
}

/// [examples].max_examples 传播：候选池 4 条 + max_examples=2 →
/// 注入示例 ≤ 2 条（`load_examples_for_input` 与装配层双闸门一致）。
#[tokio::test]
async fn examples_max_examples_propagated() {
    let mut cfg = RamariaConfig::default();
    cfg.examples.max_examples = 2;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    for i in 0..4 {
        storage.add_example(
            "char-0001",
            make_example(
                &format!("话题{i}"),
                &format!("示例回复内容{i}"),
                Some("话题,测试"),
                false,
            ),
        );
    }

    // 记忆未命中 → 候选池评分轮换（风格兜底）
    send_and_drain(&app, "话题", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    let example_count = prompt.matches("示例 ").count();
    assert!(
        example_count <= 2,
        "max_examples=2 应注入 ≤2 条，实际 {example_count} 条"
    );
    assert!(example_count >= 1, "候选池非空应至少注入 1 条");
}

/// 三开关全关 → prompt 无任何 v1.4 新增段落（语义等价 v1.3）。
#[tokio::test]
async fn all_v14_features_disabled_returns_v13_semantics() {
    let mut cfg = RamariaConfig::default();
    cfg.utt.enabled = false;
    cfg.bridge.enabled = false;
    cfg.examples.enabled = false;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    add_char_persona(&storage);
    add_prev_session_with_utt(&storage);
    storage.add_example(
        "char-0001",
        make_example("你好", "静态示例回复", Some("问候"), true),
    );
    app.rebuild_retriever().await.unwrap();

    send_and_drain(&app, "继续上次的话题吧", Some("char-0001")).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;

    // v1.4 新增段落全部不出现
    assert!(!prompt.contains("## 原文片段"), "utt 关闭：无原文片段");
    assert!(
        !prompt.contains("## 桥接（上一会话尾部）"),
        "bridge 关闭：无桥接"
    );
    // v1.3 既有内容保留（四层模板内）
    assert!(prompt.contains("# 角色（行为层）"));
    assert!(prompt.contains("# 记忆（脉络层）"));
    assert!(prompt.contains("## 近期对话脉络"), "v1.3 脉络保留");
    assert!(prompt.contains("## 回复规范"), "v1.3 回复规范保留");
    // examples 走静态 selected（v1.3 行为）
    assert!(
        prompt.contains("静态示例回复"),
        "examples 关闭回退静态 selected"
    );
}

// =========================================================
// T-V15-6-001/002：驱动环接线——行为控制块注入（v1.5 M6）
// =========================================================

/// 发送消息并返回 Done 事件的 session_id（供后续轮次复用会话）。
async fn send_and_get_session(
    app: &App,
    text: &str,
    persona: Option<&str>,
    session: Option<Uuid>,
) -> Option<Uuid> {
    let mut stream = app
        .send_message(text, persona, session)
        .await
        .expect("发送成功");
    let mut sid = None;
    while let Some(ev) = stream.next().await {
        if let Ok(StreamEvent::Done { session_id, .. }) = ev {
            sid = session_id;
        }
    }
    sid
}

/// 注册角色类 persona（行为规则学习/路由目标）。
async fn setup_behavior_persona(storage: &MockStorage) {
    storage
        .create_persona(&Persona::new(
            "char-0001".to_string(),
            "小夏".to_string(),
            PersonaKind::Char,
            0,
            "local".into(),
        ))
        .await
        .expect("persona 创建成功");
}

/// 行为命中：情境路由命中 → `## 行为规则` 块注入 prompt（reaction + params + avoid）。
///
/// 说明:
/// - 手工导入 Manual 规则（M5 D7）而非 learn：避免 mock LLM 翻译响应污染
///   对话历史（assistant 消息会进入路由查询窗口）；MockLlm 空回复 →
///   assistant 消息不落库，历史仅含用户消息，查询侧 Jaccard 纯净命中。
#[tokio::test]
async fn behavior_rule_injected_when_route_hits() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("")); // 空回复：assistant 消息不入历史
    let app = make_app(
        Arc::clone(&storage),
        Arc::clone(&llm),
        RamariaConfig::default(),
    );
    setup_ready(&app, storage.as_ref()).await;

    setup_behavior_persona(&storage).await;
    // 手工导入规则：关键词 [加班]，reaction + avoid（enabled 默认 true）
    ramaria_app::commands::behavior::behavior_import_rule(
        &app,
        "char-0001",
        r#"{
            "situation": {"keywords": ["加班"], "valence_mean": -0.5, "valence_std": 0.1, "sample_count": 6},
            "reaction": "先共情再给建议，语气疲惫但温和。",
            "params": {"emotional_intensity": -0.4, "proactiveness": 0.7, "detail_level": 0.6, "formality": 0.3},
            "avoid": ["深夜"]
        }"#,
    )
    .await
    .expect("导入成功");

    // 第一轮建立会话（history 为空 → 行为路由不命中）
    let sid = send_and_get_session(&app, "加班", Some("char-0001"), None)
        .await
        .expect("首轮应返回 session_id");
    // 第二轮：历史含"加班" → 查询侧 Jaccard=1.0 ≥ θ_route → 行为块注入
    send_and_get_session(&app, "继续聊聊", Some("char-0001"), Some(sid)).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(prompt.contains("## 行为规则"), "行为块缺失: {prompt}");
    assert!(
        prompt.contains("先共情再给建议"),
        "reaction 未注入: {prompt}"
    );
    assert!(prompt.contains("深夜"), "avoid 未注入: {prompt}");
    assert!(prompt.contains("表达倾向"), "params 未注入: {prompt}");
}

/// [behavior].enabled=false → 不路由不注入，prompt 与 v1.4 语义等价（回归红线）。
#[tokio::test]
async fn behavior_disabled_no_behavior_block() {
    let mut cfg = RamariaConfig::default();
    cfg.behavior.enabled = false;

    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(""));
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), cfg);
    setup_ready(&app, storage.as_ref()).await;

    setup_behavior_persona(&storage).await;
    // 存在可命中的规则（但行为关闭 → 不路由）
    ramaria_app::commands::behavior::behavior_import_rule(
        &app,
        "char-0001",
        r#"{
            "situation": {"keywords": ["加班"], "valence_mean": -0.5, "valence_std": 0.1, "sample_count": 6},
            "reaction": "先共情再给建议。",
            "avoid": ["深夜"]
        }"#,
    )
    .await
    .expect("导入成功");

    let sid = send_and_get_session(&app, "加班", Some("char-0001"), None)
        .await
        .expect("首轮 session_id");
    send_and_get_session(&app, "继续聊聊", Some("char-0001"), Some(sid)).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(!prompt.contains("## 行为规则"), "行为关闭不注入行为块");
    assert!(!prompt.contains("先共情再给建议"), "规则文本不泄漏");
}

/// 无规则（助手 persona 未学习）→ 无行为块（回归红线：助手类 persona 无回归）。
#[tokio::test]
async fn behavior_no_rule_no_block_for_assistant() {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的。"));
    let app = make_app(
        Arc::clone(&storage),
        Arc::clone(&llm),
        RamariaConfig::default(),
    );
    setup_ready(&app, storage.as_ref()).await;

    // 未注册规则：直接对话（不学习）
    let sid = send_and_get_session(&app, "你好", Some("rama-0001"), None)
        .await
        .expect("首轮 session_id");
    send_and_get_session(&app, "今天天气不错", Some("rama-0001"), Some(sid)).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    let prompt = &request.system_prompt;
    assert!(!prompt.contains("## 行为规则"), "无规则不产生行为块");
}
