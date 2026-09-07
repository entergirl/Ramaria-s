//! crates/ramaria-app/tests/ablation_gate_snapshot.rs - 消融净增量档 prompt 结构快照
//!
//! 职责:
//! - 端到端核对四层净增量档（I_* = B1 RAG 基座 + 单专属层）在真实
//!   `send_message_with_config` 链路上是否精确成立：捕获 `ChatRequest`
//!   （system_prompt + memory_context），按段落标题 + 内容特征断言。
//! - 对照 B0/B1 锚点与 S_* 替代对照档，锁定三类关系：
//!   - I 组：memory_context 存在（RAG 基座真注入）；prompt 仅多目标专属层；
//!   - S 组：memory_context 恒 None（去 RAG）；其余单层开关与 I 组相同；
//!   - B1：仅 RAG 基座；B0：无记忆注入。
//! - 同时锁定"数据层闸门未误开/误关"：档位关闭某层时，该层数据不被收集、
//!   prompt 不出现对应段落。
//!
//! 档位 → prompt 结构对照（本测试锁定的事实，单 persona char-0001 fixture）:
//!
//! | 档位 | memory_context | 行为规则 | 知识卡片 | 表达层 | utt 原文 | 近期脉络 | 桥接 |
//! |------|:---:|:---:|:---:|:---:|:---:|:---:|:---:|
//! | B0 | None | - | - | - | - | - | - |
//! | B1 | Some | - | - | - | - | - | - |
//! | F0（两轮） | Some | ✓ | ✓ | ✓ | ✓ | ✓ | - |
//! | I_behavior | Some | ✓ | - | - | - | - | - |
//! | I_knowledge | Some | - | ✓ | - | - | - | - |
//! | I_expression | Some | - | - | ✓ | ✓ | - | - |
//! | I_narrative | Some | - | - | - | - | ✓ | ✓ |
//! | S_behavior | None | ✓ | - | - | - | - | - |
//! | S_knowledge | None | - | ✓ | - | - | - | - |
//! | S_expression | None | - | - | ✓(+示例) | ✓ | - | - |
//! | S_narrative | None | - | - | - | - | ✓ | ✓ |
//!
//! 说明（测试锁定的既有边界语义，见各用例注释）:
//! - RAG 摘要真体经 `ChatRequest.memory_context` 注入；system_prompt 内
//!   `## 相关历史记忆` 子段仅渲染占位（app 运行路径 PromptContext.memory_context
//!   恒 None）。I_* 与 B1 的 memory_context 均应为 Some。
//! - 表达层对话示例（`## 对话示例`）在"记忆检索命中"时被既有 v1.4 业务规则跳过
//!   （示例是记忆未命中的风格兜底）：I_expression（RAG 命中）无示例、S_expression
//!   （去 RAG）注入示例。该差异源于 examples 的命中互斥语义，不构成层错位。
//! - 行为层情境路由需要对话历史（messages 非空）才可能命中：行为开档位
//!   （F0/I_behavior/S_behavior）以两轮复用会话运行，断言第二轮请求。
//! - 知识层卡片渲染无 `# 知识（知识层，按需）` 段落标题（既有边界，登记于
//!   M4 完成记录），故用内容特征 `关于兴趣爱好：…` 断言。
//!
//! 安全约束:
//! - 全部使用 mock（MockStorage + MockLlm），零真实数据库/LLM/embedding。
//! - 断言只含确定性 fixture 内容与段落标题，不含真实用户原文。

mod mock_backend;

use futures::StreamExt;
use ramaria_app::App;
use ramaria_core::config::{InjectionGate, RamariaConfig};
use ramaria_core::traits::{ChatRequest, StorageBackend, StoreCrud};
use ramaria_core::types::{
    AppState, BackendConfig, FactSource, FactStatus, FactTier, MemoryL1, Persona, PersonaExample,
    PersonaFact, PersonaKind, ProfileField, UttBlock,
};
use ramaria_llm::keychain::Keychain;
use std::sync::Arc;
use uuid::Uuid;

use mock_backend::{MockLlm, MockStorage};

// =========================================================
// 确定性 fixture 文本特征（各层独有，避免跨层误命中）
// =========================================================

/// 角色层手工说话风格（表达层手工子段）。
const PERSONA_STYLE: &str = "热情活泼，喜欢用emoji";
/// 角色层已知事实（BasicInfo，固定骨架区，任何档位都保留）。
const ROLE_FACT_TEXT: &str = "出生于上海";
/// RAG L1 摘要特征（memory_context 内容）。
const RAG_L1_SUMMARY: &str = "用户最近迷上了科幻电影，几乎每周都会看一两次";
/// 知识层 active 事实内容（知识卡片）。
const KNOWLEDGE_FACT_TEXT: &str = "喜欢科幻电影";
/// utt 原文块 / 桥接共用的上一会话原文特征。
const PREV_SESSION_UTT: &str = "上次我们聊到科幻电影";
/// 脉络层有主 L1 摘要特征（仅脉络档出现）。
const NARRATIVE_L1_SUMMARY: &str = "用户聊了科幻电影与假期安排，倾向短途旅行";
/// 表达层示例回复特征（仅记忆未命中时注入）。
const EXAMPLE_REPLY: &str = "热情简短地回答科幻问题";
/// 行为规则 reaction 特征（仅行为命中时注入）。
const BEHAVIOR_REACTION: &str = "用热情兴奋的语气回应科幻话题";

/// 非行为档单轮查询（RAG/utt/知识判定/脉络命中共用）。
const QUERY_ROUND1: &str = "你喜欢科幻电影吗？";
/// 行为档次轮查询（复用会话；RAG/知识/utt 仍基于次轮命中）。
const QUERY_ROUND2: &str = "继续聊聊科幻电影吧";
/// 行为档首轮种子消息：双字词使查询侧 bigram 恰为一个 token，与规则关键词
/// 完全一致（查询侧 Jaccard = 1.0）。行为路由只消费历史消息（次轮复用会话）。
const BEHAVIOR_SEED_QUERY: &str = "科幻";

// =========================================================
// 档位闸门应用（与 ramaria-cli AblationProfile::apply_to 映射一致，
// 避免 app 集成测试反向依赖 cli crate）
// =========================================================

/// 把消融档位名称映射为 `InjectionGate` 覆盖集。
///
/// 映射关系（M0 冻结，见 `docs/dev-2.0/ablation-profile-mapping.md`）:
/// - `B0`: 全关（纯角色 + 当前对话）。
/// - `B1`: 仅 `memory_rag`（压缩摘要基座）。
/// - `F0`: 全开（与 ablation=None 等价）。
/// - `I_*`: 净增量对照 = B1 基座 + 仅目标专属层。
/// - `S_*`: 替代对照 = 去 RAG（memory_rag=false）+ 仅目标专属层。
fn apply_profile(config: &mut RamariaConfig, name: &str) {
    let gate = match name {
        "B0" => InjectionGate::all_off(),
        "B1" => {
            let mut g = InjectionGate::all_off();
            g.memory_rag = true;
            g
        }
        "F0" => InjectionGate::all_on(),
        "I_behavior" => {
            let mut g = InjectionGate::all_off();
            g.memory_rag = true;
            g.behavior = true;
            g
        }
        "I_knowledge" => {
            let mut g = InjectionGate::all_off();
            g.memory_rag = true;
            g.knowledge = true;
            g
        }
        "I_expression" => {
            let mut g = InjectionGate::all_off();
            g.memory_rag = true;
            g.speaking_style = true;
            g.examples = true;
            g.utt = true;
            g
        }
        "I_narrative" => {
            let mut g = InjectionGate::all_off();
            g.memory_rag = true;
            g.narrative = true;
            g.bridge = true;
            g
        }
        "S_behavior" => {
            let mut g = InjectionGate::all_off();
            g.behavior = true;
            g
        }
        "S_knowledge" => {
            let mut g = InjectionGate::all_off();
            g.knowledge = true;
            g
        }
        "S_expression" => {
            let mut g = InjectionGate::all_off();
            g.speaking_style = true;
            g.examples = true;
            g.utt = true;
            g
        }
        "S_narrative" => {
            let mut g = InjectionGate::all_off();
            g.narrative = true;
            g.bridge = true;
            g
        }
        other => panic!("未知档位名称: {other}"),
    };
    config.injection = gate;
}

// =========================================================
// 测试辅助
// =========================================================

/// 构造指定配置的 App（MockStorage + MockLlm，无 embedding）。
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

/// 按档位配置发送消息并消费完整个事件流，返回 Done 事件的 session_id。
async fn send_drain(
    app: &App,
    config: &RamariaConfig,
    text: &str,
    persona: Option<&str>,
    session: Option<Uuid>,
) -> Option<Uuid> {
    let mut stream = app
        .send_message_with_config(text, persona, session, config)
        .await
        .expect("发送成功");
    let mut sid = None;
    while let Some(ev) = stream.next().await {
        if let Ok(ramaria_app::stream_event::StreamEvent::Done { session_id, .. }) = ev {
            sid = session_id;
        }
    }
    sid
}

/// 注册角色类 persona（char-0001，含手工 speaking_style）。
fn add_char_persona(storage: &MockStorage) {
    let mut p = Persona::new(
        "char-0001".to_string(),
        "小夏".to_string(),
        PersonaKind::Char,
        1,
        "local".to_string(),
    );
    p.config = Some(format!(
        r#"{{"description":"测试角色","speaking_style":"{PERSONA_STYLE}"}}"#
    ));
    storage.add_persona(p);
}

/// 构造 active 事实。
fn active_fact(
    persona_uid: &str,
    field: ProfileField,
    content: &str,
    keyword_hint: &str,
    ref_l1: Option<Uuid>,
) -> PersonaFact {
    let mut f = PersonaFact::new(
        persona_uid.to_string(),
        field,
        content.to_string(),
        FactSource::Event,
    );
    f.status = FactStatus::Active;
    f.tier = FactTier::Stable;
    f.keyword_hint = Some(keyword_hint.to_string());
    f.confidence = 0.9;
    f.ref_l1_id = ref_l1;
    f
}

/// 构造角色层 BasicInfo 事实（固定骨架已知事实区，避免 facts 为空触发
/// persona.toml 冷启动分支）。
fn add_role_fact(storage: &MockStorage) {
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::BasicInfo,
        ROLE_FACT_TEXT,
        "上海",
        None,
    ));
}

/// 构造知识层 Interests active 事实（无 RAG ref，兜底不因 RAG 覆盖被去重）。
fn add_knowledge_fact(storage: &MockStorage) {
    storage.add_fact(active_fact(
        "char-0001",
        ProfileField::Interests,
        KNOWLEDGE_FACT_TEXT,
        "电影,科幻",
        None,
    ));
}

/// 保存一条无主 L1 摘要（persona_uid=None），供 rebuild_retriever 经 unbound
/// 通道加载 → RAG 检索命中（memory_context 真体）。
async fn add_unbound_rag_l1(storage: &MockStorage, summary: &str) -> Uuid {
    let mut l1 = MemoryL1::new(Uuid::new_v4(), summary.to_string(), None);
    l1.keywords = Some("科幻,电影".to_string());
    l1.salience = 1.0;
    let id = l1.id;
    storage.save_memory_l1(&l1).await.unwrap();
    id
}

/// 保存一条有主 L1（char-0001），供脉络层经 `list_recent_l1_by_persona`
/// 回退路径加载（mock 的 rebuild 不加载有主 L1，避免污染 RAG 检索结果）。
fn add_bound_narrative_l1(storage: &MockStorage) {
    let mut l1 = MemoryL1::new(
        Uuid::new_v4(),
        NARRATIVE_L1_SUMMARY.to_string(),
        Some("char-0001".to_string()),
    );
    l1.time_period = Some("下午".to_string());
    l1.salience = 1.0;
    storage.add_l1_summaries("char-0001", vec![l1]);
}

/// 构造已关闭上一会话 + utt 块（utt 检索与桥接共用数据源）。
fn add_prev_session_with_utt(storage: &MockStorage) {
    let prev = Uuid::new_v4();
    storage.add_closed_session(prev);
    storage.add_utt_block(UttBlock {
        id: 0,
        persona_uid: "char-0001".to_string(),
        session_id: prev,
        start_msg_id: Uuid::new_v4(),
        end_msg_id: Uuid::new_v4(),
        block_text: format!(
            "[2026-08-01 20:00] 小夏: {PREV_SESSION_UTT}\n[2026-08-01 20:01] 用户: 我也喜欢"
        ),
        msg_count: 2,
        time_span_ms: 60_000,
        embedding: None,
        created_at: 1_700_000_000_000,
    });
}

/// 构造表达层示例候选（未选中；记忆未命中时评分轮换兜底注入）。
fn add_example_candidates(storage: &MockStorage) {
    for (partner, reply) in [
        ("你最近看什么", EXAMPLE_REPLY),
        ("推荐一部电影", "推荐一部轻松温暖的科幻片"),
    ] {
        let mut e = PersonaExample::new(
            "char-0001".to_string(),
            partner.to_string(),
            reply.to_string(),
        );
        e.tags = Some("科幻,电影".to_string());
        e.selected = false;
        storage.add_example("char-0001", e);
    }
}

/// 向存储导入一条手工行为规则（关键词「科幻电影」，行为命中源）。
async fn import_behavior_rule(app: &App) {
    ramaria_app::commands::behavior::behavior_import_rule(
        app,
        "char-0001",
        r#"{
            "situation": {"keywords": ["科幻"], "valence_mean": 0.5, "valence_std": 0.1, "sample_count": 6},
            "reaction": "用热情兴奋的语气回应科幻话题",
            "params": {"emotional_intensity": 0.6, "proactiveness": 0.7, "detail_level": 0.5, "formality": 0.2},
            "avoid": ["剧透"]
        }"#,
    )
    .await
    .expect("行为规则导入成功");
}

/// 构建"四层均有确定性内容"的共享 fixture（每档独立 storage/app，防止跨档污染）。
async fn build_rich_app(storage: Arc<MockStorage>, llm: Arc<MockLlm>) -> App {
    let mut config = RamariaConfig::default();
    // 打开知识层自动抽取判定总开关（默认关闭）：让知识层档位有注入内容。
    config.knowledge.auto_fact_detect = true;
    let app = make_app(Arc::clone(&storage), Arc::clone(&llm), config);

    add_char_persona(&storage);
    add_role_fact(&storage);
    add_knowledge_fact(&storage);
    add_unbound_rag_l1(&storage, RAG_L1_SUMMARY).await;
    add_bound_narrative_l1(&storage);
    add_prev_session_with_utt(&storage);
    add_example_candidates(&storage);
    import_behavior_rule(&app).await;

    setup_ready(&app, storage.as_ref()).await;
    app.rebuild_retriever().await.expect("检索器重建成功");
    app
}

/// 按档位运行并捕获最后一次 ChatRequest。
///
/// - 行为开档位（F0/I_behavior/S_behavior）以两轮复用会话运行（行为路由需历史），
///   断言第二轮；其余档位单轮新会话（新会话才会触发桥接加载）。
async fn capture_profile_request(name: &str) -> (Arc<MockStorage>, Arc<MockLlm>, ChatRequest) {
    let storage = Arc::new(MockStorage::new());
    // 空回复：assistant 消息不落库，避免污染行为路由查询侧（与既有行为路由测试一致）
    let llm = Arc::new(MockLlm::new(""));
    let app = build_rich_app(Arc::clone(&storage), Arc::clone(&llm)).await;

    let mut cfg = app.config().clone();
    apply_profile(&mut cfg, name);

    let behavior_two_round = matches!(name, "F0" | "I_behavior" | "S_behavior");
    if behavior_two_round {
        // 首轮用简短种子消息入历史（行为路由命中源），次轮复用会话发完整查询
        let sid = send_drain(&app, &cfg, BEHAVIOR_SEED_QUERY, Some("char-0001"), None).await;
        let sid = sid.expect("首轮应返回 session_id");
        send_drain(&app, &cfg, QUERY_ROUND2, Some("char-0001"), Some(sid)).await;
    } else {
        send_drain(&app, &cfg, QUERY_ROUND1, Some("char-0001"), None).await;
    }

    let request = llm.last_request().expect("应捕获最后一次 ChatRequest");
    (storage, llm, request)
}

/// 断言 prompt 全部包含给定子串（段落标题 + 内容特征）。
fn assert_present(prompt: &str, expected: &[&str], label: &str) {
    for needle in expected {
        assert!(
            prompt.contains(needle),
            "[{label}] prompt 应含「{needle}」:\n{prompt}"
        );
    }
}

/// 断言 prompt 全部不包含给定子串（对应层未注入）。
fn assert_absent(prompt: &str, forbidden: &[&str], label: &str) {
    for needle in forbidden {
        assert!(
            !prompt.contains(needle),
            "[{label}] prompt 不应含「{needle}」:\n{prompt}"
        );
    }
}

/// 固定骨架子串（所有档位 persona 存在且 facts 非空时均在）。
const FIXED_SKELETON: &[&str] = &[
    "# 能力边界",
    "# 角色（行为层）",
    "## 已知事实",
    "## 回复规范",
    "# 当前时间",
];

/// 专属层段落标题/内容特征（缺席断言用）。
const LAYER_FEATURES: &[(&str, &str)] = &[
    ("行为", "## 行为规则"),
    ("知识", "关于兴趣爱好："),
    ("表达", "# 说话风格（表达层）"),
    ("示例", "## 对话示例"),
    ("utt", "## 原文片段"),
    ("脉络", "## 近期对话脉络"),
    ("桥接", "## 桥接（上一会话尾部）"),
];

// =========================================================
// 档位快照断言
// =========================================================

// 行为规则注入需命中（I_behavior/S_behavior/F0 第二轮含「科幻电影」历史）。
// 行为开档位在第二轮断言：history=[用户 Q1 + 助手回复]，
// 查询侧含「科幻电影」bigram → 与规则关键词「科幻电影」Jaccard 命中。

#[tokio::test]
async fn b0_anchor_has_no_memory_layers() {
    let (_, _, request) = capture_profile_request("B0").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "B0");
    assert!(request.memory_context.is_none(), "B0 无 RAG 基座");
    for (_, feature) in LAYER_FEATURES {
        assert!(
            !prompt.contains(feature),
            "[B0] 不应含「{feature}」:\n{prompt}"
        );
    }
    // B0 无记忆块（四个记忆子段全关 → build_memory 返回空）
    assert!(
        !prompt.contains("# 记忆（脉络层）"),
        "[B0] 不应含记忆块:\n{prompt}"
    );
}

#[tokio::test]
async fn b1_anchor_has_rag_base_only() {
    let (_, _, request) = capture_profile_request("B1").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "B1");
    // RAG 基座真体存在
    let rag = request
        .memory_context
        .as_deref()
        .expect("B1 memory_rag=true → memory_context 应有内容");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // system_prompt 内相关历史记忆子段只渲染占位标题
    assert!(prompt.contains("# 记忆（脉络层）"), "B1 记忆块存在");
    assert!(prompt.contains("## 相关历史记忆"), "B1 渲染记忆子段标题");
    // 无专属层
    for (_, feature) in LAYER_FEATURES {
        assert!(
            !prompt.contains(feature),
            "[B1] 不应含专属层「{feature}」:\n{prompt}"
        );
    }
}

#[tokio::test]
async fn f0_full_system_injects_all_layers_on_route_hit() {
    let (_, _, request) = capture_profile_request("F0").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "F0");
    let rag = request
        .memory_context
        .as_deref()
        .expect("F0 memory_context 应有内容");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // 四层：行为（两轮命中）、知识卡片、表达手工风格、utt 原文、近期脉络
    assert_present(
        prompt,
        &[
            "## 行为规则",
            BEHAVIOR_REACTION,
            "关于兴趣爱好：喜欢科幻电影",
            "# 说话风格（表达层）",
            "## 说话风格",
            "## 原文片段",
            PREV_SESSION_UTT,
            "## 近期对话脉络",
            NARRATIVE_L1_SUMMARY,
        ],
        "F0",
    );
}

// =========================================================
// I 组（净增量：B1 基座 + 单专属层）
// =========================================================

#[tokio::test]
async fn i_behavior_is_rag_base_plus_behavior_only() {
    let (_, _, request) = capture_profile_request("I_behavior").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "I_behavior");
    let rag = request
        .memory_context
        .as_deref()
        .expect("I_behavior 保留 RAG 基座");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // 仅多行为层
    assert_present(prompt, &["## 行为规则", BEHAVIOR_REACTION], "I_behavior");
    assert_absent(
        prompt,
        &[
            "关于兴趣爱好：",
            "# 说话风格（表达层）",
            "## 原文片段",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "I_behavior",
    );
}

#[tokio::test]
async fn i_knowledge_is_rag_base_plus_knowledge_only() {
    let (_, _, request) = capture_profile_request("I_knowledge").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "I_knowledge");
    let rag = request
        .memory_context
        .as_deref()
        .expect("I_knowledge 保留 RAG 基座");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // 仅多知识卡片
    assert_present(prompt, &["关于兴趣爱好：喜欢科幻电影"], "I_knowledge");
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "# 说话风格（表达层）",
            "## 原文片段",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "I_knowledge",
    );
}

#[tokio::test]
async fn i_expression_is_rag_base_plus_style_and_utt() {
    let (_, _, request) = capture_profile_request("I_expression").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "I_expression");
    let rag = request
        .memory_context
        .as_deref()
        .expect("I_expression 保留 RAG 基座");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // 表达层 = 手工说话风格 + utt 原文（示例因 RAG 命中被既有兜底语义跳过）
    assert_present(
        prompt,
        &[
            "# 说话风格（表达层）",
            "## 说话风格",
            PERSONA_STYLE,
            "## 原文片段",
            PREV_SESSION_UTT,
        ],
        "I_expression",
    );
    assert!(
        !prompt.contains("## 对话示例"),
        "[I_expression] 记忆命中时示例兜底被跳过:\n{prompt}"
    );
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "关于兴趣爱好：",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "I_expression",
    );
}

#[tokio::test]
async fn i_narrative_is_rag_base_plus_narrative_and_bridge() {
    let (_, _, request) = capture_profile_request("I_narrative").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "I_narrative");
    let rag = request
        .memory_context
        .as_deref()
        .expect("I_narrative 保留 RAG 基座");
    assert!(rag.contains("科幻电影"), "RAG 摘要含事实: {rag}");
    // 仅多脉络层（近期脉络 + 桥接）
    assert_present(
        prompt,
        &[
            "## 近期对话脉络",
            NARRATIVE_L1_SUMMARY,
            "## 桥接（上一会话尾部）",
            PREV_SESSION_UTT,
        ],
        "I_narrative",
    );
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "关于兴趣爱好：",
            "# 说话风格（表达层）",
            "## 原文片段",
        ],
        "I_narrative",
    );
}

// =========================================================
// S 组（替代对照：去 RAG 摘要 + 单专属层）
// =========================================================

#[tokio::test]
async fn s_behavior_removes_rag_keeps_behavior() {
    let (_, _, request) = capture_profile_request("S_behavior").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "S_behavior");
    assert!(
        request.memory_context.is_none(),
        "S_behavior 去 RAG → memory_context 应为 None"
    );
    // 行为层仍在（与 I_behavior 对照：I/S 唯一差异 = RAG 基座有无）
    assert_present(prompt, &["## 行为规则", BEHAVIOR_REACTION], "S_behavior");
    assert_absent(
        prompt,
        &[
            "关于兴趣爱好：",
            "# 说话风格（表达层）",
            "## 原文片段",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "S_behavior",
    );
}

#[tokio::test]
async fn s_knowledge_removes_rag_keeps_knowledge() {
    let (_, _, request) = capture_profile_request("S_knowledge").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "S_knowledge");
    assert!(
        request.memory_context.is_none(),
        "S_knowledge 去 RAG → memory_context 应为 None"
    );
    assert_present(prompt, &["关于兴趣爱好：喜欢科幻电影"], "S_knowledge");
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "# 说话风格（表达层）",
            "## 原文片段",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "S_knowledge",
    );
}

#[tokio::test]
async fn s_expression_removes_rag_keeps_expression_with_examples() {
    let (_, _, request) = capture_profile_request("S_expression").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "S_expression");
    assert!(
        request.memory_context.is_none(),
        "S_expression 去 RAG → memory_context 应为 None"
    );
    // 表达层 = 手工风格 + 示例（去 RAG → 记忆未命中 → 示例兜底注入）+ utt 原文
    assert_present(
        prompt,
        &[
            "# 说话风格（表达层）",
            "## 说话风格",
            PERSONA_STYLE,
            "## 对话示例",
            EXAMPLE_REPLY,
            "## 原文片段",
            PREV_SESSION_UTT,
        ],
        "S_expression",
    );
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "关于兴趣爱好：",
            "## 近期对话脉络",
            "## 桥接（上一会话尾部）",
        ],
        "S_expression",
    );
}

#[tokio::test]
async fn s_narrative_removes_rag_keeps_narrative_and_bridge() {
    let (_, _, request) = capture_profile_request("S_narrative").await;
    let prompt = &request.system_prompt;
    assert_present(prompt, FIXED_SKELETON, "S_narrative");
    assert!(
        request.memory_context.is_none(),
        "S_narrative 去 RAG → memory_context 应为 None"
    );
    assert_present(
        prompt,
        &[
            "## 近期对话脉络",
            NARRATIVE_L1_SUMMARY,
            "## 桥接（上一会话尾部）",
            PREV_SESSION_UTT,
        ],
        "S_narrative",
    );
    assert_absent(
        prompt,
        &[
            "## 行为规则",
            "关于兴趣爱好：",
            "# 说话风格（表达层）",
            "## 原文片段",
        ],
        "S_narrative",
    );
}

// =========================================================
// 净增量语义矩阵锁定：I 与 S 同层差异 = RAG 基座有无
// =========================================================

/// I_* 与对应 S_* 的唯一差异是 memory_context 有无（其余层开关位相同，
/// prompt 的专属层段落均出现），由各用例已逐层断言。此处再补一个显式对照：
/// I_knowledge vs S_knowledge 的 memory_context 差异 + 知识卡片内容完全一致。
#[tokio::test]
async fn increment_vs_substitution_diff_is_rag_base_only() {
    let (_, _, i_req) = capture_profile_request("I_knowledge").await;
    let (_, _, s_req) = capture_profile_request("S_knowledge").await;

    assert!(
        i_req.memory_context.is_some() && s_req.memory_context.is_none(),
        "I 档保留 RAG 基座、S 档去 RAG"
    );
    // 同层注入内容一致（知识卡片文本相同）
    assert!(
        i_req.system_prompt.contains("关于兴趣爱好：喜欢科幻电影")
            && s_req.system_prompt.contains("关于兴趣爱好：喜欢科幻电影"),
        "I/S 同层知识卡片内容应一致"
    );
    // I 档多出 RAG 摘要内容（memory_context 真体）
    let rag = i_req.memory_context.as_deref().unwrap();
    assert!(rag.contains(RAG_L1_SUMMARY), "I 档 RAG 摘要真体: {rag}");
}
