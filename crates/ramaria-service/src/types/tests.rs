//! crates/ramaria-service/src/types/tests.rs - Ramaria 服务层用例数据结构单元测试
//!
//! 设计特点:
//! - 由 `types/mod.rs` 以 `#[cfg(test)] mod tests;` 收纳：覆盖类型定义与请求归一化两条路径
//! - serde 口径：小写枚举 / snake_case 字段 / ISO-8601 时间 / 可选字段缺省不输出
//! - 边界归一化：缺省 / 0 视为缺省 / 超上限截断三类口径
//! - 时间基准固定为常量，不依赖当前时钟
//!
//! 安全约束:
//! - 仅使用合成样例数据，不涉及真实 API key / 网络调用 / 用户数据。

use super::*;
use chrono::{DateTime, Utc};
use ramaria_core::types::{
    FactSource, FactStatus, FactTier, MessageRole, MessageSource, PersonaKind, Presentation,
    ProfileField, StyleStatsStatus, TraitLayer,
};
use std::collections::BTreeMap;
use uuid::Uuid;

/// 构造固定时间（毫秒 → ISO UTC），避免测试依赖当前时钟。
fn fixed_time(ms: i64) -> DateTime<Utc> {
    DateTime::from_timestamp_millis(ms).expect("合法毫秒时间戳应可转换")
}

#[test]
fn chat_role_serde_is_lowercase() {
    assert_eq!(
        serde_json::to_string(&ChatRole::User).expect("序列化成功"),
        r#""user""#
    );
    assert_eq!(
        serde_json::to_string(&ChatRole::Assistant).expect("序列化成功"),
        r#""assistant""#
    );
    // 内核角色转换（外部两值 → 内核四值）
    assert_eq!(MessageRole::from(ChatRole::User), MessageRole::User);
    assert_eq!(
        MessageRole::from(ChatRole::Assistant),
        MessageRole::Assistant
    );
}

#[test]
fn chat_turn_serde_roundtrip() {
    let turn = ChatTurn {
        role: ChatRole::User,
        content: "最近工作压力有点大".to_string(),
    };
    let json = serde_json::to_string(&turn).expect("序列化成功");
    let back: ChatTurn = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(turn, back);
    // 非法角色（system）应被拒绝：外部消息只允许 user / assistant
    let invalid = r#"{"role":"system","content":"x"}"#;
    assert!(
        serde_json::from_str::<ChatTurn>(invalid).is_err(),
        "system 角色不应通过外部入口类型"
    );
}

#[test]
fn recall_layer_serde_matches_contract() {
    let cases = [
        (RecallLayer::L1, "l1"),
        (RecallLayer::L2, "l2"),
        (RecallLayer::L3, "l3"),
        (RecallLayer::Knowledge, "knowledge"),
        (RecallLayer::Behavior, "behavior"),
        (RecallLayer::Style, "style"),
        (RecallLayer::Narrative, "narrative"),
        (RecallLayer::Raw, "raw"),
    ];
    for (layer, expected) in cases {
        let json = serde_json::to_string(&layer).expect("序列化成功");
        assert_eq!(
            json,
            format!("\"{expected}\""),
            "{layer:?} 应序列化为 {expected}"
        );
        let back: RecallLayer = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(layer, back);
        assert_eq!(layer.as_str(), expected);
    }
}

#[test]
fn recall_request_defaults_follow_decisions() {
    let req = RecallRequest::default();
    assert_eq!(req.effective_max_items(), DEFAULT_MAX_ITEMS);
    assert_eq!(req.effective_max_chars(), DEFAULT_MAX_CHARS);
    let include = req.effective_include();
    assert_eq!(
        include,
        vec![
            RecallLayer::L1,
            RecallLayer::L2,
            RecallLayer::Knowledge,
            RecallLayer::Narrative,
        ],
        "默认分层 = 记忆类 + 知识 + 脉络（行为/风格/原文默认关）"
    );
    assert!(!include.contains(&RecallLayer::Behavior));
    assert!(!include.contains(&RecallLayer::Style));
    assert!(!include.contains(&RecallLayer::Raw));
}

#[test]
fn recall_request_clamps_boundaries() {
    let mut req = RecallRequest {
        max_items: Some(0),
        max_chars: Some(0),
        include: Some(Vec::new()),
        ..RecallRequest::default()
    };
    // 0 视为缺省
    assert_eq!(req.effective_max_items(), DEFAULT_MAX_ITEMS);
    assert_eq!(req.effective_max_chars(), DEFAULT_MAX_CHARS);
    assert_eq!(
        req.effective_include(),
        RecallRequest::DEFAULT_INCLUDE.to_vec()
    );

    // 超上限截断
    req.max_items = Some(999);
    assert_eq!(req.effective_max_items(), MAX_ITEMS_LIMIT);

    // 合法值透传
    req.max_items = Some(3);
    req.max_chars = Some(500);
    assert_eq!(req.effective_max_items(), 3);
    assert_eq!(req.effective_max_chars(), 500);
}

#[test]
fn recall_request_serde_roundtrip_full() {
    let req = RecallRequest {
        messages: vec![
            ChatTurn {
                role: ChatRole::User,
                content: "昨天说的那个项目怎么样了".to_string(),
            },
            ChatTurn {
                role: ChatRole::Assistant,
                content: "还在等需求确认".to_string(),
            },
        ],
        persona: Some("char-0001".to_string()),
        query: Some("项目进度".to_string()),
        include: Some(vec![RecallLayer::L1, RecallLayer::Raw]),
        max_items: Some(8),
        max_chars: Some(2048),
        conversation_id: Some("conv-42".to_string()),
    };
    let json = serde_json::to_string(&req).expect("序列化成功");
    let back: RecallRequest = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(req, back);
}

#[test]
fn recall_result_serde_uses_iso_time_and_lowercase_mode() {
    let result = RecallResult {
        context: "## 相关记忆\n- 用户最近在赶项目".to_string(),
        items: vec![RecallItem {
            layer: RecallLayer::L1,
            id: "550e8400-e29b-41d4-a716-446655440000".to_string(),
            text: "用户最近在赶项目".to_string(),
            score: Some(0.71),
            time: Some(fixed_time(1_756_000_000_000)),
        }],
        stats: RecallStats {
            mode: RecallMode::Search,
            channels: BTreeMap::from([
                ("vector".to_string(), 4usize),
                ("bm25".to_string(), 3usize),
            ]),
            truncated: false,
        },
    };
    let json = serde_json::to_string(&result).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    // 契约口径：layer / mode 小写，time 为 ISO-8601 字符串
    assert_eq!(value["items"][0]["layer"], "l1");
    assert_eq!(value["stats"]["mode"], "search");
    let time = value["items"][0]["time"].as_str().expect("time 应为字符串");
    assert!(
        time.starts_with("2025-"),
        "time 应为 ISO 字符串，实际 {time}"
    );
    assert!(time.ends_with('Z'), "time 应为 UTC 后缀 Z，实际 {time}");
    // 往返一致
    let back: RecallResult = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(result, back);
}

#[test]
fn recall_result_empty_is_serializable() {
    // 空结果（无命中 / 空库）应输出结构完整的空骨架，而非 None
    let result = RecallResult::default();
    let json = serde_json::to_string(&result).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    assert_eq!(value["context"], "");
    assert_eq!(value["items"].as_array().map(Vec::len), Some(0));
    assert_eq!(value["stats"]["mode"], "search");
    let back: RecallResult = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(result, back);
}

#[test]
fn ingest_types_serde_roundtrip() {
    let req = IngestRequest {
        messages: vec![ChatTurn {
            role: ChatRole::Assistant,
            content: "好的，明天见".to_string(),
        }],
        persona: Some(DEFAULT_PERSONA_UID.to_string()),
        conversation_id: Some("client-A".to_string()),
        channel: CHANNEL_MCP.to_string(),
        finalize: true,
    };
    let json = serde_json::to_string(&req).expect("序列化成功");
    let back: IngestRequest = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(req, back);

    let outcome = IngestOutcome {
        session_id: Uuid::nil(),
        written: 4,
        deduplicated: 1,
        finalized: true,
    };
    let json = serde_json::to_string(&outcome).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    assert!(
        value["session_id"].is_string(),
        "session_id 应序列化为字符串，实际 {}",
        value["session_id"]
    );
    let back: IngestOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(outcome, back);
}

#[test]
fn seal_outcome_serde_roundtrip() {
    let outcome = SealOutcome {
        session_id: Uuid::nil(),
        sealed: true,
        l1_count: 2,
    };
    let json = serde_json::to_string(&outcome).expect("序列化成功");
    let back: SealOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(outcome, back);
    // 未抢到场景：sealed=false 且 l1_count=0（不重复生成）
    let skipped = SealOutcome {
        session_id: Uuid::nil(),
        sealed: false,
        l1_count: 0,
    };
    let json = serde_json::to_string(&skipped).expect("序列化成功");
    let back: SealOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(skipped, back);
}

#[test]
fn persona_card_sections_normalization() {
    let req = PersonaCardRequest {
        uid: "rama-0001".to_string(),
        sections: None,
    };
    assert_eq!(req.effective_sections().len(), 5, "缺省返回全部分段");

    let req = PersonaCardRequest {
        uid: "rama-0001".to_string(),
        sections: Some(Vec::new()),
    };
    assert_eq!(req.effective_sections().len(), 5, "空列表视为缺省");

    let req = PersonaCardRequest {
        uid: "rama-0001".to_string(),
        sections: Some(vec![PersonaSection::Traits]),
    };
    assert_eq!(req.effective_sections(), vec![PersonaSection::Traits]);
}

#[test]
fn persona_card_view_serde_roundtrip() {
    let card = PersonaCardView {
        uid: "char-0001".to_string(),
        name: "小林".to_string(),
        kind: PersonaKind::Char,
        source: "local".to_string(),
        description: Some("大学同学".to_string()),
        active: true,
        traits: vec![TraitView {
            layer: TraitLayer::Base,
            label: "温和".to_string(),
            meaning: "说话节奏慢，很少打断别人".to_string(),
            trigger: None,
            confidence: 0.82,
        }],
        behaviors: vec![BehaviorRuleView {
            id: 7,
            situation: "被问到工作压力".to_string(),
            reaction: Some("先自嘲一句再聊具体事".to_string()),
            avoid: vec!["直接说教".to_string()],
            confidence: 0.7,
            enabled: true,
        }],
        style: Some(StyleView {
            rule_text: Some("句尾常用「啦」".to_string()),
            status: StyleStatsStatus::Ready,
            sample_count: 320,
        }),
        facts: vec![FactView {
            field: ProfileField::Interests,
            content: "喜欢露营".to_string(),
            tier: FactTier::Stable,
            confidence: 0.9,
        }],
        maturity: DataMaturityView {
            l1_count: 42,
            event_count: 12,
            trait_count: 5,
            fact_count: 8,
            example_count: 20,
        },
    };
    let json = serde_json::to_string(&card).expect("序列化成功");
    let back: PersonaCardView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(card, back);
    // 嵌套枚举序列化口径
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    assert_eq!(value["kind"], "char");
    assert_eq!(value["traits"][0]["layer"], "base");
    assert_eq!(value["style"]["status"], "ready");
    assert_eq!(value["facts"][0]["tier"], "stable");
    assert_eq!(value["facts"][0]["field"], "interests");
}

#[test]
fn session_summary_view_serde_roundtrip() {
    let view = SessionSummaryView {
        id: Uuid::nil(),
        started_at: fixed_time(1_756_000_000_000),
        ended_at: None,
        persona_uid: Some("rama-0001".to_string()),
        channel: CHANNEL_MCP.to_string(),
        external_ref: Some("client-A".to_string()),
        message_count: 6,
    };
    let json = serde_json::to_string(&view).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    assert_eq!(value["channel"], "mcp");
    assert!(value["started_at"].as_str().is_some());
    let back: SessionSummaryView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(view, back);
}

#[test]
fn history_types_serde_roundtrip_and_paging_defaults() {
    let req = HistoryRequest {
        session_id: Some(Uuid::nil()),
        persona: None,
        limit: None,
        offset: None,
    };
    assert_eq!(req.effective_limit(), DEFAULT_HISTORY_LIMIT);
    assert_eq!(req.effective_offset(), 0);
    let json = serde_json::to_string(&req).expect("序列化成功");
    let back: HistoryRequest = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(req, back);

    let result = HistoryResult {
        session_id: Some(Uuid::nil()),
        messages: vec![HistoryMessageView {
            role: MessageRole::Assistant,
            content: "嗯，我在".to_string(),
            time: fixed_time(1_756_000_000_000),
            persona_uid: Some("char-0001".to_string()),
        }],
        total: 1,
    };
    let json = serde_json::to_string(&result).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    assert_eq!(value["messages"][0]["role"], "assistant");
    let back: HistoryResult = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(result, back);
}

#[test]
fn persona_summary_view_serde_roundtrip() {
    let view = PersonaSummaryView {
        uid: "rama-0001".to_string(),
        name: "Ramaria".to_string(),
        kind: PersonaKind::Rama,
        source: "local".to_string(),
        description: None,
        active: true,
    };
    let json = serde_json::to_string(&view).expect("序列化成功");
    let back: PersonaSummaryView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(view, back);
}

/// 人格管理视图：JSON 字段名与桌面契约逐字对齐（snake_case，`is_active`）。
#[test]
fn persona_management_views_serde() {
    let view = PersonaFullView {
        uid: "char-0001".to_string(),
        name: "小林".to_string(),
        kind: "char".to_string(),
        source: "file".to_string(),
        ref_id: Some("qq-123456".to_string()),
        avatar: Some("avatar.png".to_string()),
        config: Some("assistant_name = \"小林\"".to_string()),
        description: Some("大学同学".to_string()),
        is_active: true,
        created_at: 1_000,
        updated_at: 2_000,
    };
    let json = serde_json::to_string(&view).expect("序列化成功");
    let value: serde_json::Value = serde_json::from_str(&json).expect("JSON 解析成功");
    for key in [
        "uid",
        "name",
        "kind",
        "source",
        "ref_id",
        "avatar",
        "config",
        "description",
        "is_active",
        "created_at",
        "updated_at",
    ] {
        assert!(value.get(key).is_some(), "字段 {key} 应存在于 JSON: {json}");
    }
    assert!(value.get("isActive").is_none(), "不应输出 camelCase 字段名");
    let back: PersonaFullView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(view, back);

    // 更新请求：三个可选字段缺省为 None
    let req = PersonaUpdateRequest::default();
    assert!(req.name.is_none() && req.avatar.is_none() && req.description.is_none());
    let json = serde_json::to_string(&PersonaUpdateRequest {
        name: Some("新名字".to_string()),
        avatar: None,
        description: Some(String::new()),
    })
    .expect("序列化成功");
    assert!(json.contains("\"description\":\"\""), "空描述应可表达清空");

    // 文件导入动作：小写序列化口径
    for (action, expected) in [
        (PersonaFileAction::Created, "created"),
        (PersonaFileAction::Updated, "updated"),
        (PersonaFileAction::Skipped, "skipped"),
        (PersonaFileAction::Failed, "failed"),
    ] {
        let json = serde_json::to_string(&action).expect("序列化成功");
        assert_eq!(json, format!("\"{expected}\""));
        let back: PersonaFileAction = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(action, back);
    }

    // 结果条目往返
    let outcome = PersonaFileOutcome {
        uid: "char-0001".to_string(),
        action: PersonaFileAction::Updated,
        message: "已更新 persona: char-0001 (小林)".to_string(),
    };
    let json = serde_json::to_string(&outcome).expect("序列化成功");
    let back: PersonaFileOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(outcome, back);
}

/// 模型管理视图：可选字段缺省时不出现，降级原因按蛇形小写序列化。
#[test]
fn model_management_types_serde() {
    // 校验失败：只有 valid + reason，dimension 字段不出现
    let invalid = EmbeddingValidation::invalid("模型目录不存在: /tmp/x");
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&invalid).expect("序列化成功"))
            .expect("JSON 解析成功");
    assert!(value.get("dimension").is_none());
    assert_eq!(value["valid"], false);

    // 校验成功：valid + dimension，reason 字段不出现
    let valid = EmbeddingValidation {
        valid: true,
        dimension: Some(384),
        reason: None,
    };
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&valid).expect("序列化成功"))
            .expect("JSON 解析成功");
    assert!(value.get("reason").is_none());
    assert_eq!(value["dimension"], 384);

    // 已加载模型视图：不暴露本地路径
    let loaded = EmbeddingModelView {
        model_path: None,
        valid: true,
        dimension: Some(1024),
    };
    let value: serde_json::Value =
        serde_json::from_str(&serde_json::to_string(&loaded).expect("序列化成功"))
            .expect("JSON 解析成功");
    assert!(value.get("model_path").is_none());

    // 降级原因四态序列化口径
    let cases = [
        (DegradedReason::EmbeddingMissing, "embedding_missing"),
        (DegradedReason::LlmUnavailable, "llm_unavailable"),
        (DegradedReason::BothUnavailable, "both_unavailable"),
        (DegradedReason::Unknown, "unknown"),
    ];
    for (reason, expected) in cases {
        let json = serde_json::to_string(&reason).expect("序列化成功");
        assert_eq!(json, format!("\"{expected}\""));
    }
}

/// 浏览 / 关键词用例：请求默认值与分页字段形态。
#[test]
fn browse_request_defaults() {
    let l1 = L1BrowseRequest::default();
    assert!(l1.persona.is_none());
    assert!(!l1.unabsorbed_only, "L1 默认走按会话收集口径");
    assert_eq!(l1.limit, None);
    assert_eq!(l1.offset, None);

    let l2 = L2BrowseRequest::default();
    assert!(l2.persona.is_none());
    assert_eq!(l2.limit, None);

    let sessions = SessionBrowseRequest::default();
    assert_eq!(sessions.limit, None, "会话列表缺省返回全部");
    assert_eq!(sessions.offset, None);

    let messages = SessionMessagesRequest {
        session_id: Uuid::nil(),
        limit: None,
        offset: None,
    };
    assert!(messages.limit.is_none(), "limit=None 表示全量加载");
}

/// 浏览 / 关键词用例：代表性视图 serde 往返与枚举口径。
#[test]
fn browse_and_keyword_views_serde_roundtrip() {
    // 别名裁决动作枚举口径（confirm / reject）
    for (action, expected) in [
        (AliasAction::Confirm, "confirm"),
        (AliasAction::Reject, "reject"),
    ] {
        let json = serde_json::to_string(&action).expect("序列化成功");
        assert_eq!(json, format!("\"{expected}\""));
        let back: AliasAction = serde_json::from_str(&json).expect("反序列化成功");
        assert_eq!(action, back);
    }

    // L1 摘要视图往返
    let l1 = L1MemoryView {
        id: Uuid::nil(),
        session_id: Uuid::nil(),
        summary: "用户最近在准备考试".to_string(),
        keywords: Some("考试".to_string()),
        atmosphere: Some("专注".to_string()),
        time_period: Some("夜间".to_string()),
        context_json: None,
        valence: 0.2,
        salience: 0.7,
        persona_uid: Some("char-0001".to_string()),
        created_at: 1_756_000_000_000,
    };
    let json = serde_json::to_string(&l1).expect("序列化成功");
    let back: L1MemoryView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(l1, back);

    // 会话详情视图往返（UTC 时间与消息条目）
    let detail = SessionDetailView {
        id: Uuid::nil(),
        started_at: fixed_time(1_756_000_000_000),
        ended_at: None,
        persona_uid: Some("char-0001".to_string()),
        total_messages: 1,
        has_more: false,
        messages: vec![SessionMessageView {
            id: Uuid::nil(),
            role: MessageRole::User,
            content: "你好".to_string(),
            created_at: 1_756_000_000_001,
            source: MessageSource::Local,
            persona_uid: Some("char-0001".to_string()),
            is_proactive: true,
        }],
    };
    let json = serde_json::to_string(&detail).expect("序列化成功");
    assert!(
        json.contains("\"role\":\"user\""),
        "role 应小写序列化: {json}"
    );
    assert!(
        json.contains("\"is_proactive\":true"),
        "is_proactive 应序列化透出: {json}"
    );
    let back: SessionDetailView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(detail, back);

    // L2 事件视图往返（presentation 小写序列化）
    let event = L2EventView {
        id: 7,
        persona_uid: "char-0001".to_string(),
        title: "备考冲刺".to_string(),
        summary: "连续几天复习到深夜".to_string(),
        keywords: Some("考试,复习".to_string()),
        valence: -0.1,
        confidence: 0.8,
        presentation: Presentation::Subjective,
        share: 0.5,
        attitude: Some("有点紧张但坚持".to_string()),
        salience: 0.6,
        created_at: 1_756_000_000_000,
        start: 1_756_000_000_000,
        end: 1_756_001_800_000,
    };
    let json = serde_json::to_string(&event).expect("序列化成功");
    assert!(
        json.contains("\"presentation\":\"subjective\""),
        "presentation 应小写序列化: {json}"
    );
    assert!(
        json.contains("\"start\":1756000000000") && json.contains("\"end\":1756001800000"),
        "事件起止时间应随视图序列化: {json}"
    );
    let back: L2EventView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(event, back);

    // 事实条目视图往返（枚举字段口径）
    let fact = FactEntryView {
        id: 3,
        persona_uid: "char-0001".to_string(),
        field: ProfileField::Interests,
        content: "喜欢露营".to_string(),
        source: FactSource::Manual,
        status: FactStatus::Active,
        tier: FactTier::Stable,
        version_of: None,
        confidence: 0.9,
        keyword_hint: Some("露营".to_string()),
        ref_event_id: None,
        ref_l1_id: None,
        created_at: 1_000,
        updated_at: 2_000,
    };
    let json = serde_json::to_string(&fact).expect("序列化成功");
    assert!(json.contains("\"field\":\"interests\""));
    assert!(json.contains("\"status\":\"active\""));
    assert!(json.contains("\"source\":\"manual\""));
    let back: FactEntryView = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(fact, back);

    // 别名裁决结果（reject 后 canonical_keyword 为 null）
    let outcome = AliasResolveOutcome {
        alias: "职场焦虑".to_string(),
        canonical_keyword: None,
        status: "canonical".to_string(),
        already_applied: false,
    };
    let json = serde_json::to_string(&outcome).expect("序列化成功");
    assert!(json.contains("\"canonical_keyword\":null"));
    let back: AliasResolveOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(outcome, back);

    // 关键词 seed 结果（逐条 inserted / status 口径）
    let seed = KeywordSeedOutcome {
        seeded: 1,
        skipped: 1,
        results: vec![
            KeywordSeedItem {
                keyword: "工作压力".to_string(),
                inserted: true,
                status: "canonical".to_string(),
            },
            KeywordSeedItem {
                keyword: "职场焦虑".to_string(),
                inserted: false,
                status: "pending".to_string(),
            },
        ],
    };
    let json = serde_json::to_string(&seed).expect("序列化成功");
    let back: KeywordSeedOutcome = serde_json::from_str(&json).expect("反序列化成功");
    assert_eq!(seed, back);
}
