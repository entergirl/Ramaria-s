//! rust/crates/ramaria-app/tests/m3_integration.rs - M3 L3 管线闭环集成测试
//!
//! 设计特点:
//! - 使用 MockStorage + Mock LLM 验证 L3 Phase B/C 全链路
//! - mock LLM 返回 JSON 格式的三步推断结果
//! - 验证置信度更新 + 证据链记录
//! - 覆盖正常路径、降级路径、首轮推断路径
//! - 验证 System Prompt Block A 在 L3 推断后包含结构化性格标签

mod mock_backend;

use mock_backend::{MockLlm, MockStorage};
use ramaria_core::{
    traits::{LlmProvider, StorageBackend},
    types::{MemoryEvent, PersonalityTrait, Presentation, TraitLayer, TraitSource, TraitStatus},
};
use ramaria_memory::inference::{
    CategoryStats, CrossCategoryMetrics, InferrerConfig, PhaseBSource, RepresentativeEvent,
    StatsSummary, run_phase_b_inference, run_phase_c_update,
};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

// =========================================================
// 多步 Mock LLM — 每次调用返回不同回复
// =========================================================

/// 多步 Mock LLM：按顺序返回预设回复列表，每次调用 consume 一项。
///
/// 用于模拟三步推断中 LLM 按 Step 1→2→3 返回不同 JSON。
struct MultiStepLlm {
    replies: Vec<String>,
    call_count: AtomicUsize,
    model_capability: ramaria_core::types::ModelCapability,
    config: ramaria_core::types::BackendConfig,
}

impl MultiStepLlm {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies,
            call_count: AtomicUsize::new(0),
            model_capability: ramaria_core::types::ModelCapability {
                provider: ramaria_core::types::LlmProvider::LmStudio,
                model_id: "mock-multi-step".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: true,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: ramaria_core::types::BackendConfig::lm_studio_default(),
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MultiStepLlm {
    async fn chat(
        &self,
        _request: &ramaria_core::traits::ChatRequest,
    ) -> ramaria_core::error::RamariaResult<String> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx >= self.replies.len() {
            // 超出范围返回空 JSON（不应发生）
            Ok("{}".to_string())
        } else {
            Ok(self.replies[idx].clone())
        }
    }

    async fn chat_stream(
        &self,
        _request: &ramaria_core::traits::ChatRequest,
    ) -> ramaria_core::error::RamariaResult<
        std::pin::Pin<
            Box<
                dyn futures::Stream<
                        Item = ramaria_core::error::RamariaResult<
                            ramaria_core::traits::StreamDelta,
                        >,
                    > + Send,
            >,
        >,
    > {
        let reply = self.chat(_request).await?;
        let deltas: Vec<ramaria_core::traits::StreamDelta> = reply
            .chars()
            .map(|c| ramaria_core::traits::StreamDelta {
                content: c.to_string(),
                done: false,
                metadata: None,
            })
            .collect();
        Ok(Box::pin(futures::stream::iter(deltas.into_iter().map(Ok))))
    }

    fn capability(&self) -> &ramaria_core::types::ModelCapability {
        &self.model_capability
    }

    fn config(&self) -> &ramaria_core::types::BackendConfig {
        &self.config
    }

    async fn validate(&self) -> ramaria_core::error::RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MultiStepMock"
    }
}

// =========================================================
// 三步推断的测试用 JSON 回复
// =========================================================

/// Step 1 回复：逐分类性格信号。
fn step1_reply() -> String {
    r#"{
        "工作": {
            "signal_label": "尽责",
            "evidence_citation": "valence_mean=0.55, positive_ratio=75%",
            "stability_judgment": "stable",
            "sufficient_evidence": true
        },
        "社交": {
            "signal_label": "社交回避",
            "evidence_citation": "share_mean=0.8 但 valence_mean=-0.1",
            "stability_judgment": "contextual",
            "sufficient_evidence": false
        }
    }"#
    .to_string()
}

/// Step 2 回复：跨分类一致性分析。
fn step2_reply() -> String {
    r#"{
        "base_candidates": ["尽责"],
        "primary_candidates": ["温和"],
        "accent_candidates": ["社交回避", "幽默"],
        "notes": "尽责在工作和家庭分类中均出现，判断为底色。"
    }"#
    .to_string()
}

/// Step 3 回复：结构化性格画像。
fn step3_reply() -> String {
    r#"[
        {
            "layer": "base",
            "trait_label": "尽责",
            "meaning": "对交给自己的任务有强烈的完成意愿，重视承诺",
            "not_meaning": null,
            "trigger": null,
            "suppress": null,
            "related": null,
            "seq": 0
        },
        {
            "layer": "primary",
            "trait_label": "温和",
            "meaning": "在社交中倾向于倾听和包容，避免直接冲突",
            "not_meaning": "并非软弱或没有主见",
            "trigger": null,
            "suppress": null,
            "related": null,
            "seq": 0
        },
        {
            "layer": "accent",
            "trait_label": "社交回避",
            "meaning": "对大型社交场合感到消耗，倾向小圈子深聊",
            "not_meaning": null,
            "trigger": "10人以上场合",
            "suppress": "与熟悉的人一对一交流时不会触发",
            "related": "与温和互为因果",
            "seq": 0
        },
        {
            "layer": "accent",
            "trait_label": "幽默",
            "meaning": "在舒适环境中用自嘲和调侃化解紧张",
            "not_meaning": "并非轻浮或不认真",
            "trigger": "与信任的朋友相处",
            "suppress": "正式场合",
            "related": null,
            "seq": 1
        }
    ]"#
    .to_string()
}

// =========================================================
// 测试用 StatsSummary 构建辅助
// =========================================================

fn make_stats_summary() -> StatsSummary {
    StatsSummary {
        total_events_in: 15,
        total_events_filtered: 12,
        confirmed_count: 12,
        tentative_count: 0,
        discarded_count: 3,
        category_count: 2,
        categories: vec![
            CategoryStats {
                category: "工作".into(),
                event_count: 8,
                n_eff: 6.5,
                valence_mean: 0.55,
                valence_std: 0.35,
                valence_positive_ratio: 0.75,
                share_mean: 0.7,
                share_std: 0.2,
                presentation_objective_ratio: 0.5,
                presentation_subjective_ratio: 0.3,
                presentation_mixed_ratio: 0.2,
                group_weight: 0.6,
            },
            CategoryStats {
                category: "社交".into(),
                event_count: 4,
                n_eff: 3.2,
                valence_mean: -0.1,
                valence_std: 0.5,
                valence_positive_ratio: 0.45,
                share_mean: 0.8,
                share_std: 0.15,
                presentation_objective_ratio: 0.2,
                presentation_subjective_ratio: 0.6,
                presentation_mixed_ratio: 0.2,
                group_weight: 0.4,
            },
        ],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.45,
            narrative_consistency: 0.7,
            attitude_contradiction_count: 0,
            share_skewness: 0.1,
            share_kurtosis: -0.5,
        },
        representative_events: vec![RepresentativeEvent {
            title: "项目验收".into(),
            summary: "完成项目验收".into(),
            attitude: Some("对成果满意".into()),
            valence: 0.8,
            salience: 0.9,
            category: "工作".into(),
        }],
        motive_stats: Vec::new(),
    }
}

/// 创建测试用 MemoryEvent 列表。
fn make_test_events(persona_uid: &str) -> Vec<MemoryEvent> {
    let now = ramaria_core::types::now_ms();
    vec![
        MemoryEvent {
            id: 1,
            persona_uid: persona_uid.to_string(),
            title: "项目验收".into(),
            summary: "顺利完成项目验收".into(),
            keywords: Some("工作,项目".into()),
            participants: None,
            start: now - 86400000,
            end: 0,
            confidence: 0.85,
            salience: 0.9,
            valence: 0.8,
            presentation: Presentation::Objective,
            share: 0.7,
            attitude: Some("对成果满意".into()),
            paraphrase: None,
            absorbed: 0,
            situation_strength: None,
            motives: None,
            created_at: now - 86400000,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        },
        MemoryEvent {
            id: 2,
            persona_uid: persona_uid.to_string(),
            title: "社交团建".into(),
            summary: "参加部门团建感到压力".into(),
            keywords: Some("社交,团建".into()),
            participants: None,
            start: now - 172800000,
            end: 0,
            confidence: 0.7,
            salience: 0.6,
            valence: -0.3,
            presentation: Presentation::Subjective,
            share: 0.4,
            attitude: Some("对强制社交感到焦虑".into()),
            paraphrase: None,
            absorbed: 0,
            situation_strength: None,
            motives: None,
            created_at: now - 172800000,
            last_accessed_at: None,
            indexed_at: None,
            index_version: None,
        },
    ]
}

// =========================================================
// T-V12-3-008: L3 全管线端到端测试
// =========================================================

/// 测试使用 mock LLM 产出 PersonalityTrait 记录并写入 DB。
#[tokio::test]
async fn phase_b_produces_traits_with_mock_llm() {
    let storage = Arc::new(MockStorage::new());
    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    let result = run_phase_b_inference(&multi_llm, &*storage, &stats, "rama-0001", &config)
        .await
        .expect("Phase B 应成功完成");

    // 验证推断来源
    assert_eq!(result.source, PhaseBSource::LlmInference);
    // 验证有 trait 被保存（4 个来自 Step 3 JSON）
    assert!(result.traits_saved >= 4, "应至少保存 4 个 trait");
    // 验证无更新（首轮）
    assert_eq!(result.traits_updated, 0);
    // 验证无废弃（首轮）
    assert_eq!(result.traits_deprecated, 0);

    // 验证 storage 中确实有数据
    let saved_traits = storage
        .list_traits_by_persona("rama-0001")
        .await
        .expect("查询 traits 应成功");
    assert!(!saved_traits.is_empty(), "storage 中应有 trait 记录");

    // 验证 trait 属性正确
    let base_trait = saved_traits.iter().find(|t| t.layer == TraitLayer::Base);
    assert!(base_trait.is_some(), "应有底色层 trait");
    assert_eq!(base_trait.unwrap().trait_label, "尽责");

    // 验证置信度初始值
    for t in &saved_traits {
        assert!(t.confidence > 0.0, "trait 置信度应 > 0");
        assert_eq!(t.source, TraitSource::Inferred);
        assert_eq!(t.status, TraitStatus::Active);
        assert_eq!(t.persona_uid, "rama-0001");
    }
}

/// 测试在 LLM 失败时降级至 mock_infer。
#[tokio::test]
async fn phase_b_falls_back_to_mock_infer_on_llm_error() {
    let storage = Arc::new(MockStorage::new());
    // 使用返回错误的 Mock LLM
    let failing_llm = MockLlm::failing("connection refused");
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    let result = run_phase_b_inference(&failing_llm, &*storage, &stats, "rama-0001", &config)
        .await
        .expect("降级到 mock_infer 后应成功完成");

    // 验证推断来源为 MockFallback
    assert_eq!(result.source, PhaseBSource::MockFallback);

    // 验证仍有 trait 被保存（mock_infer 基于统计规则推断）
    assert!(result.traits_saved > 0, "mock_infer 应产出 trait");

    let saved_traits = storage
        .list_traits_by_persona("rama-0001")
        .await
        .expect("查询 traits 应成功");
    assert!(!saved_traits.is_empty());
}

/// 测试增量推断（有旧 traits 时的 diff 更新）。
#[tokio::test]
async fn phase_b_incremental_update_with_existing_traits() {
    let storage = Arc::new(MockStorage::new());

    // 预置旧 trait
    let old_trait = PersonalityTrait {
        id: 0,
        persona_uid: "rama-0001".into(),
        layer: TraitLayer::Base,
        trait_label: "尽责".into(),
        meaning: "旧描述".into(),
        not_meaning: None,
        trigger: None,
        suppress: None,
        related: None,
        seq: 0,
        source: TraitSource::Inferred,
        ref_event_id: None,
        ref_l1_id: None,
        confidence: 0.5,
        evidence: 1.0,
        consistency: 0.5,
        status: TraitStatus::Active,
        created_at: 1000,
        updated_at: 1000,
    };
    storage.add_trait(old_trait);

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();

    let result = run_phase_b_inference(&multi_llm, &*storage, &stats, "rama-0001", &config)
        .await
        .expect("增量推断应成功");

    // 验证差异处理：尽责已存在 → keep（不新增）
    // 但新 trait（温和、社交回避、幽默）应新增
    assert!(result.traits_saved > 0, "应有新增 trait");
}

/// 测试置信度更新 + 证据链记录。
#[tokio::test]
async fn phase_c_confidence_and_evidence() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    // 先运行创建 traits
    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    let phase_b = run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config)
        .await
        .expect("Phase B 应成功");

    // 准备测试事件
    let events = make_test_events(persona_uid);

    // 运行置信度更新
    let confidence_config = ramaria_memory::inference::confidence::ConfidenceConfig::default();
    let drift_config = ramaria_memory::inference::drift::DriftConfig::default();
    let phase_c = run_phase_c_update(
        &confidence_config,
        &drift_config,
        &*storage,
        persona_uid,
        &phase_b.traits,
        &events,
        true, // 首轮推断
    )
    .await
    .expect("Phase C 应成功");

    // 验证置信度更新
    assert!(phase_c.traits_updated > 0, "应有 trait 置信度被更新");

    // 验证证据记录
    assert!(phase_c.evidence_saved > 0, "应有证据记录被保存");

    // 首轮应跳过漂移检测
    assert!(!phase_c.has_significant_drift);

    // 验证 storage 中确有 evidence 记录
    let evidence_count: usize = {
        let saved_traits = storage.list_traits_by_persona(persona_uid).await.unwrap();
        let mut total = 0;
        for t in &saved_traits {
            let ev = storage.list_evidence_by_trait(t.id).await.unwrap();
            total += ev.len();
        }
        total
    };
    assert!(evidence_count > 0, "storage 中应有 evidence 记录");

    // 验证 confidence 已被更新为非默认值
    let updated_traits = storage.list_traits_by_persona(persona_uid).await.unwrap();
    for t in &updated_traits {
        // 有证据更新后 confidence 应不再完全是初始值 0.5
        // 至少 evidence 字段应 > 1.0（有新增证据）
        assert!(t.evidence > 1.0, "evidence 应在 Phase C 后被更新");
    }
}

/// 测试 L3 推断后 list_traits_by_persona 返回非空结果。
#[tokio::test]
async fn list_traits_by_persona_returns_traits_after_l3() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    // 推断前应为空
    let before = storage.list_traits_by_persona(persona_uid).await.unwrap();
    assert!(before.is_empty(), "推断前应无 trait");

    // 运行推断
    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config)
        .await
        .expect("Phase B 应成功");

    // 推断后应非空
    let after = storage.list_traits_by_persona(persona_uid).await.unwrap();
    assert!(!after.is_empty(), "推断后应有 trait");
    assert!(after.len() >= 4, "应至少有 4 个 trait");
}

// =========================================================
// T-V12-3-009: System Prompt Block A 验证
// =========================================================

/// 验证 L3 推断后的 trait 包含结构化性格标签，可用于 System Prompt Block A。
#[tokio::test]
async fn traits_have_structured_labels_for_system_prompt() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config)
        .await
        .expect("Phase B 应成功");

    let traits = storage.list_traits_by_persona(persona_uid).await.unwrap();

    // 验证三层模型齐全
    let has_base = traits.iter().any(|t| t.layer == TraitLayer::Base);
    let has_primary = traits.iter().any(|t| t.layer == TraitLayer::Primary);
    let has_accent = traits.iter().any(|t| t.layer == TraitLayer::Accent);

    assert!(has_base, "应包含底色层 trait");
    assert!(has_primary, "应包含主色调层 trait");
    assert!(has_accent, "应包含点缀层 trait");

    // 验证每个 trait 都有用于 System Prompt 的必要字段
    for t in &traits {
        assert!(!t.trait_label.is_empty(), "trait_label 不应为空");
        assert!(!t.meaning.is_empty(), "meaning 不应为空");
        // accent trait 应有 trigger
        if t.layer == TraitLayer::Accent {
            assert!(t.trigger.is_some(), "accent trait 应有 trigger 字段");
        }
    }
}

/// 验证 L3 推断后产物可组装为 Block A 格式文本。
#[tokio::test]
async fn traits_can_format_as_block_a_text() {
    let storage = Arc::new(MockStorage::new());
    let persona_uid = "rama-0001";

    let multi_llm = MultiStepLlm::new(vec![step1_reply(), step2_reply(), step3_reply()]);
    let stats = make_stats_summary();
    let config = InferrerConfig::default();
    run_phase_b_inference(&multi_llm, &*storage, &stats, persona_uid, &config)
        .await
        .expect("Phase B 应成功");

    let traits = storage.list_traits_by_persona(persona_uid).await.unwrap();

    // 模拟 build_system_prompt_with_context 中 Block A 的格式化逻辑
    let block_a = format_traits_for_prompt(&traits);

    // 验证 Block A 包含关键性格标签
    assert!(block_a.contains("尽责"), "Block A 应包含底色 trait");
    assert!(block_a.contains("温和"), "Block A 应包含主色调 trait");
    assert!(
        block_a.contains("社交回避") || block_a.contains("幽默"),
        "Block A 应包含点缀 trait"
    );

    // 验证 Block A 包含结构化信息
    assert!(block_a.contains("底色"), "Block A 应有层级标签");
    assert!(
        block_a.contains("重视承诺"),
        "Block A 应包含 trait 的具体含义说明"
    );
}

/// 模拟 build_system_prompt_with_context 中 Block A 的格式化逻辑。
fn format_traits_for_prompt(traits: &[PersonalityTrait]) -> String {
    let mut s = String::from("## 性格画像\n\n");

    // 底色层
    let base: Vec<_> = traits
        .iter()
        .filter(|t| t.layer == TraitLayer::Base)
        .collect();
    if !base.is_empty() {
        s.push_str("### 底色（跨情境稳定特征）\n");
        for t in base {
            s.push_str(&format!("- **{}**：{}", t.trait_label, t.meaning));
            if let Some(ref not_meaning) = t.not_meaning {
                s.push_str(&format!("（非：{}）", not_meaning));
            }
            s.push('\n');
        }
        s.push('\n');
    }

    // 主色调
    let primary: Vec<_> = traits
        .iter()
        .filter(|t| t.layer == TraitLayer::Primary)
        .collect();
    if !primary.is_empty() {
        s.push_str("### 主色调（日常最突出特征）\n");
        for t in primary {
            s.push_str(&format!("- **{}**：{}\n", t.trait_label, t.meaning));
        }
        s.push('\n');
    }

    // 点缀
    let accent: Vec<_> = traits
        .iter()
        .filter(|t| t.layer == TraitLayer::Accent)
        .collect();
    if !accent.is_empty() {
        s.push_str("### 点缀（特定条件下浮现）\n");
        for t in accent {
            s.push_str(&format!("- **{}**：{}", t.trait_label, t.meaning));
            if let Some(ref trigger) = t.trigger {
                s.push_str(&format!("（浮现条件：{}）", trigger));
            }
            s.push('\n');
        }
    }

    s
}
