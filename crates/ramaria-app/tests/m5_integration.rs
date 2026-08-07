//! rust/crates/ramaria-app/tests/m5_integration.rs - v1.4 M5 集成测试
//!
//! 覆盖:
//! - T-V14-5-002/003：桥接链路（新会话加载上一会话尾部 → 注入 system prompt）
//!   - 角色类 persona 新会话：prompt 含【桥接（上一会话尾部）】段落
//!   - rama 自身（助手类）：不注入桥接（原文白名单回归红线）
//! - T-V14-5-005：单边合并封存链路（真实消息序列 → 封存 → utt 块结构）
//!
//! 安全约束:
//! - 全部使用 mock（MockStorage + MockLlm），不触碰真实数据库/LLM。

#![allow(unused_must_use)]

mod mock_backend;

use futures::StreamExt;
use ramaria_app::App;
use ramaria_core::config::RamariaConfig;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{
    AppState, BackendConfig, Message, MessageRole, MessageSource, Persona, PersonaKind, UttBlock,
};
use ramaria_llm::keychain::Keychain;
use std::sync::Arc;
use uuid::Uuid;

use mock_backend::{MockLlm, MockStorage};

// =========================================================
// 辅助函数
// =========================================================

/// 构造 Ready 状态 App（MockStorage + MockLlm）。
fn make_app() -> (Arc<MockStorage>, Arc<MockLlm>, App) {
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new("好的，我记住了。"));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    (storage, llm, app)
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

/// 发送消息并消费完整个事件流。
async fn send_and_drain(app: &App, text: &str, persona: Option<&str>, session: Option<Uuid>) {
    let mut stream = app.send_message(text, persona, session).await.unwrap();
    while stream.next().await.is_some() {}
}

/// 构造一条消息（created_at 可控）。
fn make_msg(
    session: Uuid,
    role: MessageRole,
    content: &str,
    persona: Option<&str>,
    t: i64,
) -> Message {
    let mut m = Message::new(session, role, content.to_string(), MessageSource::Local)
        .with_persona_uid(persona.map(|s| s.to_string()));
    m.created_at = t;
    m
}

// =========================================================
// T-V14-5-002/003：桥接活跃路径
// =========================================================

/// 角色类 persona 新会话：桥接内容注入 system prompt（utt 块来源）。
///
/// 链路: 已关闭会话 + utt 块 → 新会话创建（resolve_session 加载）→
/// app_chat 组装 → MockLlm 收到含【桥接（上一会话尾部）】段落的 user_message。
#[tokio::test]
async fn bridge_injected_into_new_session_prompt_from_utt_block() {
    let (storage, llm, app) = make_app();
    setup_ready(&app, storage.as_ref()).await;

    // 上一会话：已关闭 + 有 utt 块（角色类 persona 在白名单内）
    let prev_session = Uuid::new_v4();
    storage.add_closed_session(prev_session);
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));
    storage.add_utt_block(UttBlock {
        id: 1,
        persona_uid: "char-0001".to_string(),
        session_id: prev_session,
        start_msg_id: Uuid::new_v4(),
        end_msg_id: Uuid::new_v4(),
        block_text: "[2026-08-01 20:00] 小夏: 上次我们聊到去海边\n[2026-08-01 20:01] 用户: 嗯嗯"
            .to_string(),
        msg_count: 2,
        time_span_ms: 60_000,
        embedding: None,
        created_at: 1_700_000_000_000,
    });

    // 新会话发送消息（无 session_id → resolve_session 创建 + 加载桥接）
    send_and_drain(&app, "继续上次的话题吧", Some("char-0001"), None).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    assert!(
        request.system_prompt.contains("桥接（上一会话尾部）"),
        "prompt 应含桥接段落标题"
    );
    assert!(
        request.system_prompt.contains("上次我们聊到去海边"),
        "prompt 应含桥接原文（utt 块内容）"
    );
}

/// 降级路径：无 utt 块 → 末 N 条原文消息注入桥接。
#[tokio::test]
async fn bridge_injected_into_new_session_prompt_from_recent_messages() {
    let (storage, llm, app) = make_app();
    setup_ready(&app, storage.as_ref()).await;

    let prev_session = Uuid::new_v4();
    storage.add_closed_session(prev_session);
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));
    // 无 utt 块，仅 2 条原文消息
    storage.add_messages(
        prev_session,
        vec![
            make_msg(
                prev_session,
                MessageRole::Assistant,
                "明天记得带伞",
                Some("char-0001"),
                1_700_000_000_000,
            ),
            make_msg(
                prev_session,
                MessageRole::User,
                "好的，晚安",
                None,
                1_700_000_060_000,
            ),
        ],
    );

    send_and_drain(&app, "早上好", Some("char-0001"), None).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    assert!(
        request.system_prompt.contains("桥接（上一会话尾部）"),
        "降级路径也应注入桥接段落"
    );
    assert!(
        request.system_prompt.contains("明天记得带伞"),
        "应含末 N 条原文"
    );
    assert!(
        request.system_prompt.contains("用户: 好的，晚安"),
        "应含用户消息行"
    );
}

/// 回归红线：rama 自身（助手类，不在原文白名单）→ 不注入桥接（与 v1.3 语义等价）。
#[tokio::test]
async fn bridge_not_injected_for_rama_persona() {
    let (storage, llm, app) = make_app();
    setup_ready(&app, storage.as_ref()).await;

    let prev_session = Uuid::new_v4();
    storage.add_closed_session(prev_session);
    storage.add_utt_block(UttBlock {
        id: 1,
        persona_uid: "rama-0001".to_string(),
        session_id: prev_session,
        start_msg_id: Uuid::new_v4(),
        end_msg_id: Uuid::new_v4(),
        block_text: "[2026-08-01 20:00] rama: 你好".to_string(),
        msg_count: 1,
        time_span_ms: 0,
        embedding: None,
        created_at: 1_700_000_000_000,
    });

    // persona_uid = None → rama 自身
    send_and_drain(&app, "你好", None, None).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    assert!(
        !request.system_prompt.contains("桥接（上一会话尾部）"),
        "助手类 persona 不应注入桥接原文"
    );
}

/// 桥接开关关闭 → 即使有上一会话也不注入。
#[tokio::test]
async fn bridge_disabled_skips_injection() {
    let (storage, llm, _app) = make_app();
    // 关闭桥接：bridge.enabled = false
    let mut cfg = RamariaConfig::default();
    cfg.bridge.enabled = false;
    let keychain = Arc::new(Keychain::new());
    let app2 = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        cfg,
        keychain,
    );
    setup_ready(&app2, storage.as_ref()).await;

    let prev_session = Uuid::new_v4();
    storage.add_closed_session(prev_session);
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));
    storage.add_utt_block(UttBlock {
        id: 1,
        persona_uid: "char-0001".to_string(),
        session_id: prev_session,
        start_msg_id: Uuid::new_v4(),
        end_msg_id: Uuid::new_v4(),
        block_text: "[2026-08-01 20:00] 小夏: 上次的内容".to_string(),
        msg_count: 1,
        time_span_ms: 0,
        embedding: None,
        created_at: 1_700_000_000_000,
    });

    send_and_drain(&app2, "你好", Some("char-0001"), None).await;

    let request = llm.last_request().expect("应记录最后一次请求");
    assert!(
        !request.system_prompt.contains("桥接（上一会话尾部）"),
        "bridge.enabled=false 不应注入桥接"
    );
}

// =========================================================
// T-V14-5-005：单边合并封存链路端到端
// =========================================================

/// 封存链路：真实消息序列（双边 → 间隙 → 单边 → 间隙 → 双边）经
/// save_and_close_session（含 utt 构建）后，单边块正确并入相邻块。
#[tokio::test]
async fn save_and_close_single_side_block_merges_in_pipeline() {
    // MockLlm 需返回合法 L1 JSON：封存链路中 utt 构建位于 L1 生成成功之后
    //（Step 2.5），L1 失败则 utt 构建被跳过（既有降级语义）。
    const L1_JSON: &str = r#"{"summary":"用户讨论了项目安排","keywords":"项目,排期","time_period":"下午","atmosphere":"紧张","valence":0.0,"salience":0.5}"#;
    let storage = Arc::new(MockStorage::new());
    let llm = Arc::new(MockLlm::new(L1_JSON));
    let config = RamariaConfig::default();
    let keychain = Arc::new(Keychain::new());
    let app = App::new_without_embedding(
        Arc::clone(&storage) as Arc<dyn StorageBackend>,
        Arc::clone(&llm) as Arc<dyn ramaria_core::traits::LlmProvider>,
        config,
        keychain,
    );
    setup_ready(&app, storage.as_ref()).await;

    // 角色类 persona（utt 白名单内）
    storage.add_persona(Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    ));

    // 先走一次 send_message 创建活跃会话（与封存链路衔接）
    send_and_drain(&app, "你好", Some("char-0001"), None).await;
    let session_id = app.get_active_session_id().expect("应有活跃会话");

    // 追加单边消息序列：交替（双边）→ 间隙 → 纯用户（单边）→ 间隙 → 交替（双边）
    let base = 1_700_000_000_000i64;
    let gap = 60 * 60 * 1000; // 1h > θ_gap 30min
    let mut msgs = vec![];
    let mut t = base;
    // 双边段（交替）
    for i in 0..4 {
        let role = if i % 2 == 0 {
            MessageRole::Assistant
        } else {
            MessageRole::User
        };
        let uid = if i % 2 == 0 { Some("char-0001") } else { None };
        msgs.push(make_msg(session_id, role, &format!("双边段{i}"), uid, t));
        t += 60_000;
    }
    // 单边段（纯用户）
    t += gap;
    for i in 4..6 {
        msgs.push(make_msg(
            session_id,
            MessageRole::User,
            &format!("单边段{i}"),
            None,
            t,
        ));
        t += 60_000;
    }
    // 双边段
    t += gap;
    for i in 6..10 {
        let role = if i % 2 == 0 {
            MessageRole::Assistant
        } else {
            MessageRole::User
        };
        let uid = if i % 2 == 0 { Some("char-0001") } else { None };
        msgs.push(make_msg(session_id, role, &format!("双边段2_{i}"), uid, t));
        t += 60_000;
    }
    for m in &msgs {
        storage.save_message(m).await.unwrap();
    }

    // 封存：关闭会话 + L1 生成 + utt 构建
    app.save_and_close_session(Some("char-0001")).await.unwrap();

    // 断言 utt 块结构：单边块已并入相邻块（优先并入前块）
    let blocks = storage
        .list_utt_blocks_by_persona("char-0001")
        .await
        .unwrap();
    // 首条 send_message 的"你好"回合可能形成额外块，取本会话全部块核对合并语义：
    // 单边段（4-5）不应独立成块——要么并入前块（0-5）要么并入后块（4-9）
    let merged = blocks
        .iter()
        .filter(|b| b.session_id == session_id)
        .collect::<Vec<_>>();
    assert!(!merged.is_empty(), "封存后应有 utt 块");
    for block in &merged {
        let text = &block.block_text;
        let has_left = text.contains("双边段0");
        let has_mid = text.contains("单边段4") || text.contains("单边段5");
        let has_right = text.contains("双边段2_6");
        if has_mid {
            // 单边内容必须与某侧双边内容同块（已并入）
            assert!(
                has_left || has_right,
                "单边消息应并入相邻块，实际独立成块: {text}"
            );
        }
        let _ = has_left;
    }
}
