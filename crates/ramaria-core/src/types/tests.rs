//! crates/ramaria-core/src/types/tests.rs - Ramaria 核心业务数据类型单元测试
//!
//! 设计特点:
//! - 覆盖 ID 双轨制转换与时间戳工具函数
//! - 校验核心领域对象的构造与状态辅助方法
//! - 验证 Persona 体系枚举与结构体的 serde 往返
//! - 依赖父模块 re-export 与 uuid 导入，保持断言零改写

use super::*;

use uuid::Uuid;

// ---- EvidenceNote（v1.4 结构化证据线索）----

#[test]
fn evidence_note_new_creates_text_only() {
    let note = EvidenceNote::new("用户表示最近一个月每天加班到10点以后");
    assert_eq!(note.text, "用户表示最近一个月每天加班到10点以后");
    assert!(note.time.is_none());
    assert!(note.who.is_none());
    assert!(note.cause.is_none());
}

#[test]
fn evidence_note_with_slots() {
    let note = EvidenceNote::with_slots(
        "用户提到项目延期",
        Some("上周三晚上".to_string()),
        Some("用户".to_string()),
        Some("因为需求变更".to_string()),
    );
    assert_eq!(note.time.as_deref(), Some("上周三晚上"));
    assert_eq!(note.who.as_deref(), Some("用户"));
    assert_eq!(note.cause.as_deref(), Some("因为需求变更"));
}

#[test]
fn evidence_note_serde_roundtrip_full() {
    let note = EvidenceNote::with_slots(
        "用户表示压力很大",
        Some("最近".to_string()),
        Some("用户".to_string()),
        Some("工作量大".to_string()),
    );
    let json = serde_json::to_string(&note).unwrap();
    let back: EvidenceNote = serde_json::from_str(&json).unwrap();
    assert_eq!(note, back);
}

#[test]
fn evidence_note_serde_roundtrip_text_only() {
    let note = EvidenceNote::new("仅文本证据");
    let json = serde_json::to_string(&note).unwrap();
    let back: EvidenceNote = serde_json::from_str(&json).unwrap();
    assert_eq!(note, back);
    // 文本-only 序列化应包含 text 键且无多余槽位
    let value: serde_json::Value = serde_json::from_str(&json).unwrap();
    assert_eq!(value["text"], "仅文本证据");
    assert!(value.get("time").is_none(), "None 槽位不应序列化");
}

#[test]
fn evidence_note_parses_missing_slots() {
    // 兼容：缺失槽位的 JSON 反序列化为 None（而非报错）
    let json = r#"{"text": "只有文本的证据"}"#;
    let note: EvidenceNote = serde_json::from_str(json).unwrap();
    assert_eq!(note.text, "只有文本的证据");
    assert!(note.time.is_none());
    assert!(note.who.is_none());
    assert!(note.cause.is_none());
}

#[test]
fn evidence_note_parses_null_slots() {
    // LLM 可能输出 "time": null，应解析为 None
    let json = r#"{"text": "证据", "time": null, "who": null, "cause": null}"#;
    let note: EvidenceNote = serde_json::from_str(json).unwrap();
    assert_eq!(note.text, "证据");
    assert!(note.time.is_none());
}

#[test]
fn memory_l1_evidence_notes_upgraded_to_structured() {
    // MemoryL1.evidence_notes 已是 Vec<EvidenceNote>（结构化新格式）
    let mut l1 = MemoryL1::new(Uuid::new_v4(), "摘要".to_string(), None);
    l1.evidence_notes = Some(vec![
        EvidenceNote::with_slots(
            "用户提到项目延期",
            Some("上周".to_string()),
            Some("用户".to_string()),
            Some("需求变更".to_string()),
        ),
        EvidenceNote::new("用户表示压力很大"),
    ]);
    let json = serde_json::to_string(&l1).unwrap();
    let back: MemoryL1 = serde_json::from_str(&json).unwrap();
    let notes = back.evidence_notes.expect("evidence_notes 不应为 None");
    assert_eq!(notes.len(), 2);
    assert_eq!(notes[0].who.as_deref(), Some("用户"));
    assert_eq!(notes[1].text, "用户表示压力很大");
    assert!(notes[1].cause.is_none());
}

// ---- ID 与时间约定 ----

#[test]
fn new_id_is_unique() {
    let a = new_id();
    let b = new_id();
    assert_ne!(a, b);
}

#[test]
fn new_id_is_valid_uuid_v4() {
    let id = new_id();
    assert_eq!(id.get_version_num(), 4);
}

#[test]
fn uuid_to_db_and_back() {
    let id = new_id();
    let s = uuid_to_db(id);
    let back = uuid_from_db(&s).expect("合法 UUID 应解析成功");
    assert_eq!(id, back);
}

#[test]
fn uuid_from_db_invalid_returns_error() {
    let result = uuid_from_db("not-a-valid-uuid");
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert_eq!(err.category(), "validation");
    assert!(err.context().contains("not-a-valid-uuid"));
}

#[test]
fn now_ms_is_reasonable() {
    let t = now_ms();
    assert!(t > 1_700_000_000_000, "timestamp too old: {t}");
    assert!(t < 2_600_000_000_000, "timestamp too far: {t}");
}

// ---- MessageRole ----

/// MessageRole serde 序列化（小写字符串）与往返验证。
#[test]
fn message_role_serde_cases() {
    let cases = [
        (MessageRole::User, r#""user""#),
        (MessageRole::Assistant, r#""assistant""#),
        (MessageRole::System, r#""system""#),
        (MessageRole::Tool, r#""tool""#),
    ];
    for (role, expected) in cases {
        let json = serde_json::to_string(&role).unwrap();
        assert_eq!(json, expected, "{role:?} 应序列化为小写");
        let back: MessageRole = serde_json::from_str(&json).unwrap();
        assert_eq!(role, back);
    }
}

// ---- Session / Message ----

#[test]
fn session_lifecycle() {
    let mut session = Session::new();
    assert!(session.is_active());
    assert!(session.ended_at.is_none());
    session.close();
    assert!(!session.is_active());
    assert!(session.ended_at.is_some());
}

#[test]
fn session_close_is_idempotent() {
    let mut session = Session::new();
    let first_close = now_ms();
    session.ended_at = Some(first_close);
    session.close();
    assert_eq!(session.ended_at, Some(first_close));
}

#[test]
fn session_defaults_to_local_channel() {
    let session = Session::new();
    assert_eq!(session.channel, CHANNEL_LOCAL);
    assert!(session.external_ref.is_none());

    let session = Session::with_persona(Some("char-0001".into()));
    assert_eq!(session.channel, CHANNEL_LOCAL);
    assert_eq!(session.persona_uid.as_deref(), Some("char-0001"));
}

#[test]
fn session_new_in_channel_sets_channel_and_ref() {
    let session = Session::new_in_channel(
        Some("rama-0001".to_string()),
        "mcp",
        Some("client-A".to_string()),
    );
    assert_eq!(session.channel, "mcp");
    assert_eq!(session.external_ref.as_deref(), Some("client-A"));
    assert_eq!(session.persona_uid.as_deref(), Some("rama-0001"));
    assert!(session.is_active());
    assert!(session.started_at > 0);
}

#[test]
fn message_creation() {
    let sid = new_id();
    let msg = Message::new(sid, MessageRole::User, "你好".into(), MessageSource::Local);
    assert_eq!(msg.session_id, sid);
    assert_eq!(msg.role, MessageRole::User);
    assert_eq!(msg.content, "你好");
    assert_eq!(msg.source, MessageSource::Local);
    assert!(msg.fingerprint.is_none());
    assert!(msg.persona_uid.is_none());
}

#[test]
fn message_with_persona_uid() {
    let sid = new_id();
    let mut msg = Message::new(sid, MessageRole::User, "你好".into(), MessageSource::Local);
    msg.persona_uid = Some("user-0001".into());
    assert_eq!(msg.persona_uid.as_deref(), Some("user-0001"));
}

#[test]
fn session_message_serde_roundtrip() {
    let mut session = Session::new();
    session.channel = "mcp".to_string();
    session.external_ref = Some("client-A".to_string());
    let json = serde_json::to_string(&session).unwrap();
    let back: Session = serde_json::from_str(&json).unwrap();
    assert_eq!(session.id, back.id);
    assert_eq!(session.started_at, back.started_at);
    assert_eq!(back.channel, "mcp");
    assert_eq!(back.external_ref.as_deref(), Some("client-A"));

    let mut msg = Message::new(
        session.id,
        MessageRole::Assistant,
        "回复".into(),
        MessageSource::Online,
    );
    msg.persona_uid = Some("rama-0001".into());
    let json = serde_json::to_string(&msg).unwrap();
    let back: Message = serde_json::from_str(&json).unwrap();
    assert_eq!(msg.session_id, back.session_id);
    assert_eq!(msg.role, back.role);
    assert_eq!(msg.content, back.content);
    assert_eq!(back.persona_uid.as_deref(), Some("rama-0001"));
}

// ---- MemoryL1 ----

#[test]
fn memory_l1_lifecycle() {
    let sid = new_id();
    let mut l1 = MemoryL1::new(sid, "摘要内容".into(), Some("上午".into()));
    assert!(!l1.absorbed);
    assert_eq!(l1.valence, 0.0);
    assert_eq!(l1.salience, 0.5);
    assert!(l1.last_accessed_at.is_none());
    assert!(l1.persona_uid.is_none());
    assert!(l1.context_json.is_none());

    l1.mark_absorbed();
    assert!(l1.absorbed);
    l1.touch();
    assert!(l1.last_accessed_at.is_some());
}

#[test]
fn memory_l1_with_persona_context() {
    let sid = new_id();
    let mut l1 = MemoryL1::new(sid, "摘要".into(), None);
    l1.persona_uid = Some("char-0003".into());
    l1.context_json = Some(r#"{"chat_partners":["user-0001","char-0003"]}"#.into());
    let json = serde_json::to_string(&l1).unwrap();
    let back: MemoryL1 = serde_json::from_str(&json).unwrap();
    assert_eq!(back.persona_uid.as_deref(), Some("char-0003"));
    assert!(
        back.context_json
            .as_deref()
            .unwrap()
            .contains("chat_partners")
    );
}

// ---- Persona 枚举 serde ----

#[test]
fn persona_kind_serde() {
    assert_eq!(
        serde_json::to_string(&PersonaKind::User).unwrap(),
        r#""user""#
    );
    assert_eq!(
        serde_json::to_string(&PersonaKind::Rama).unwrap(),
        r#""rama""#
    );
    assert_eq!(
        serde_json::to_string(&PersonaKind::Char).unwrap(),
        r#""char""#
    );
    let back: PersonaKind = serde_json::from_str(r#""anim""#).unwrap();
    assert_eq!(back, PersonaKind::Anim);
}

#[test]
fn trait_layer_serde() {
    assert_eq!(
        serde_json::to_string(&TraitLayer::Base).unwrap(),
        r#""base""#
    );
    assert_eq!(
        serde_json::to_string(&TraitLayer::Primary).unwrap(),
        r#""primary""#
    );
    assert_eq!(
        serde_json::to_string(&TraitLayer::Accent).unwrap(),
        r#""accent""#
    );
}

#[test]
fn event_relation_kind_serde() {
    // PascalCase 序列化
    assert_eq!(
        serde_json::to_string(&EventRelationKind::CausedBy).unwrap(),
        r#""CausedBy""#
    );
    assert_eq!(
        serde_json::to_string(&EventRelationKind::Contradicts).unwrap(),
        r#""Contradicts""#
    );
    let back: EventRelationKind = serde_json::from_str(r#""Timeline""#).unwrap();
    assert_eq!(back, EventRelationKind::Timeline);
}

#[test]
fn presentation_serde() {
    assert_eq!(
        serde_json::to_string(&Presentation::Objective).unwrap(),
        r#""objective""#
    );
    assert_eq!(
        serde_json::to_string(&Presentation::Subjective).unwrap(),
        r#""subjective""#
    );
    assert_eq!(
        serde_json::to_string(&Presentation::Mixed).unwrap(),
        r#""mixed""#
    );
}

#[test]
fn trait_status_serde() {
    assert_eq!(
        serde_json::to_string(&TraitStatus::Active).unwrap(),
        r#""active""#
    );
    assert_eq!(
        serde_json::to_string(&TraitStatus::Deprecated).unwrap(),
        r#""deprecated""#
    );
}

#[test]
fn profile_field_includes_speaking_style() {
    assert_eq!(ProfileField::SpeakingStyle.label(), "说话风格");
    assert_eq!(ProfileField::SpeakingStyle.as_str(), "speaking_style");
    // 确保原有字段不变
    assert_eq!(ProfileField::BasicInfo.label(), "基础信息");
    assert_eq!(ProfileField::PersonalStatus.label(), "近期状态");
}

// ---- Persona 结构体创建（id 初始为 0） ----

#[test]
fn persona_creation() {
    let p = Persona::new(
        "user-0001".into(),
        "用户".into(),
        PersonaKind::User,
        1,
        "local".into(),
    );
    assert_eq!(p.uid, "user-0001");
    assert_eq!(p.kind, PersonaKind::User);
    assert!(p.active);
    assert_eq!(p.id, 0); // 存储层回填前为 0
}

#[test]
fn persona_fact_creation() {
    let f = PersonaFact::new(
        "user-0001".into(),
        ProfileField::BasicInfo,
        "姓名：小明".into(),
        FactSource::L1,
    );
    assert_eq!(f.persona_uid, "user-0001");
    assert_eq!(f.field, ProfileField::BasicInfo);
    assert_eq!(f.id, 0);
}

#[test]
fn memory_event_creation() {
    let now = now_ms();
    let ev = MemoryEvent::new(
        "user-0001".into(),
        "跳槽".into(),
        "换了新工作".into(),
        now - 86_400_000,
        now,
    );
    assert_eq!(ev.persona_uid, "user-0001");
    assert_eq!(ev.confidence, 0.5);
    assert_eq!(ev.salience, 0.5);
    assert_eq!(ev.presentation, Presentation::Mixed);
    assert_eq!(ev.id, 0);
}

#[test]
fn event_relation_creation() {
    let rel = EventRelation::new(1, 2, EventRelationKind::CausedBy);
    assert_eq!(rel.from_id, 1);
    assert_eq!(rel.to_id, 2);
    assert_eq!(rel.kind, EventRelationKind::CausedBy);
    assert_eq!(rel.weight, 0.5);
    assert_eq!(rel.id, 0);
}

#[test]
fn event_source_creation() {
    let l1 = new_id();
    let src = EventSource::new(5, l1);
    assert_eq!(src.event_id, 5);
    assert_eq!(src.l1_id, l1);
    assert_eq!(src.weight, 1.0);
    assert_eq!(src.id, 0);
}

#[test]
fn trait_evidence_creation() {
    let ev = TraitEvidence::new(1, 10, EvidenceDirection::Support, 0.85);
    assert_eq!(ev.trait_id, 1);
    assert_eq!(ev.event_id, 10);
    assert_eq!(ev.direction, EvidenceDirection::Support);
    assert!((ev.score - 0.85).abs() < f64::EPSILON);
    assert_eq!(ev.id, 0);
}

#[test]
fn personality_trait_creation() {
    let pt = PersonalityTrait::new(
        "user-0001".into(),
        TraitLayer::Primary,
        "幽默".into(),
        "喜欢用自嘲化解尴尬".into(),
        TraitSource::Inferred,
        1,
    );
    assert_eq!(pt.trait_label, "幽默");
    assert_eq!(pt.layer, TraitLayer::Primary);
    assert_eq!(pt.status, TraitStatus::Active);
    assert_eq!(pt.confidence, 0.0);
    assert_eq!(pt.id, 0);
}

#[test]
fn persona_example_creation() {
    let ex = PersonaExample::new(
        "char-0003".into(),
        "今天怎么样？".into(),
        "还行，刚跑完步".into(),
    );
    assert_eq!(ex.persona_uid, "char-0003");
    assert_eq!(ex.length, 7); // "还行，刚跑完步" = 7 个字符
    assert!(!ex.selected);
    assert_eq!(ex.id, 0);
}

#[test]
fn cluster_snapshot_creation() {
    let cs = ClusterSnapshot::new("user-0001".into(), "工作".into(), "对挑战的兴奋感".into());
    assert_eq!(cs.persona_uid, "user-0001");
    assert_eq!(cs.category, "工作");
    assert!(cs.is_current);
    assert_eq!(cs.id, 0);
}

// ---- Persona 类型 serde 往返 ----

#[test]
fn persona_serde_roundtrip() {
    let mut p = Persona::new(
        "rama-0001".into(),
        "Ramaria".into(),
        PersonaKind::Rama,
        1,
        "local".into(),
    );
    p.id = 42;
    let json = serde_json::to_string(&p).unwrap();
    let back: Persona = serde_json::from_str(&json).unwrap();
    assert_eq!(back.uid, "rama-0001");
    assert_eq!(back.kind, PersonaKind::Rama);
    assert_eq!(back.id, 42);
}

#[test]
fn memory_event_serde_roundtrip() {
    let mut ev = MemoryEvent::new(
        "user-0001".into(),
        "事件".into(),
        "描述".into(),
        now_ms() - 1000,
        now_ms(),
    );
    ev.id = 7;
    let json = serde_json::to_string(&ev).unwrap();
    let back: MemoryEvent = serde_json::from_str(&json).unwrap();
    assert_eq!(back.title, "事件");
    assert_eq!(back.persona_uid, "user-0001");
    assert_eq!(back.id, 7);
}

#[test]
fn personality_trait_serde_roundtrip() {
    let mut pt = PersonalityTrait::new(
        "user-0001".into(),
        TraitLayer::Base,
        "温和".into(),
        "待人接物温和".into(),
        TraitSource::Inferred,
        0,
    );
    pt.id = 3;
    let json = serde_json::to_string(&pt).unwrap();
    let back: PersonalityTrait = serde_json::from_str(&json).unwrap();
    assert_eq!(back.trait_label, "温和");
    assert_eq!(back.layer, TraitLayer::Base);
    assert_eq!(back.id, 3);
}

// ---- BackendConfig / PrivacyConsent ----

#[test]
fn llm_provider_serde() {
    for (provider, expected) in [
        (LlmProvider::LmStudio, r#""lm_studio""#),
        (LlmProvider::DeepSeek, r#""deepseek""#),
        (LlmProvider::OpenAI, r#""openai""#),
    ] {
        assert_eq!(serde_json::to_string(&provider).unwrap(), expected);
        let back: LlmProvider = serde_json::from_str(expected).unwrap();
        assert_eq!(back, provider);
    }
}

#[test]
fn llm_provider_accepts_legacy_kebab_form() {
    // 兼容旧 config.toml 模板的连字符写法 `lm-studio`：
    // 反序列化必须兼容（否则真实用户配置文件会被误判为损坏而回退默认值）。
    // TOML 通道的完整场景（[backend] 表内 provider = "lm-studio"）由
    // config.rs::v14_config_groups_missing_fields_fallback_to_defaults 覆盖。
    let back: LlmProvider = serde_json::from_str(r#""lm-studio""#).unwrap();
    assert_eq!(back, LlmProvider::LmStudio);
}

#[test]
fn llm_provider_is_online() {
    assert!(!LlmProvider::LmStudio.is_online());
    assert!(LlmProvider::DeepSeek.is_online());
    assert!(LlmProvider::OpenAI.is_online());
}

#[test]
fn backend_config_defaults() {
    let lm = BackendConfig::lm_studio_default();
    assert_eq!(lm.provider, LlmProvider::LmStudio);
    assert!(lm.base_url.contains("localhost"));

    let ds = BackendConfig::deepseek_default();
    assert_eq!(ds.provider, LlmProvider::DeepSeek);
    assert_eq!(ds.capability.model_id, "deepseek-chat");

    let oa = BackendConfig::openai_default();
    assert_eq!(oa.provider, LlmProvider::OpenAI);
}

#[test]
fn privacy_consent_creation() {
    let consent = PrivacyConsent::new(
        LlmProvider::DeepSeek,
        "https://api.deepseek.com/v1".into(),
        true,
    );
    assert_eq!(consent.provider, LlmProvider::DeepSeek);
    assert!(consent.persistent);
    assert!(consent.timestamp > 0);
}

// ---- AppState ----

#[test]
fn app_state_serde() {
    for state in [
        AppState::NeedsSetup,
        AppState::DownloadingModel,
        AppState::Indexing,
        AppState::Ready,
        AppState::Degraded,
        AppState::FatalError,
    ] {
        let json = serde_json::to_string(&state).unwrap();
        let back: AppState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, back);
    }
}

#[test]
fn app_state_ready_allows_conversation() {
    assert!(matches!(AppState::Ready, AppState::Ready));
    assert!(!matches!(AppState::NeedsSetup, AppState::Ready));
    assert!(!matches!(AppState::FatalError, AppState::Ready));
}

#[test]
fn message_source_default() {
    assert_eq!(MessageSource::default(), MessageSource::Local);
}

#[test]
fn message_source_serde() {
    assert_eq!(
        serde_json::to_string(&MessageSource::Local).unwrap(),
        r#""local""#
    );
    assert_eq!(
        serde_json::to_string(&MessageSource::Online).unwrap(),
        r#""online""#
    );
}
