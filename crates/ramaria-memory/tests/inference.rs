//! crates/ramaria-memory/tests/inference.rs - 离线推断链路（Phase A/B/C）用例
//!
//! 设计特点:
//! - 覆盖 Phase A 统计：校准权重、暂定事件提升、动机维度统计与空输入边界
//! - 覆盖 Phase B 推断：多步 LLM 产出性格标签、增量更新、LLM 失败回退本地推断
//! - 覆盖 Phase C 置信度更新与证据链记录
//! - 覆盖性格标签的结构化渲染（System Prompt Block A 形态）
//! - 使用脚本化多步 mock LLM 与内存存储，无网络无真实模型

mod common;

use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use ramaria_core::{
    traits::{ChatRequest, LlmProvider, StoreCrud},
    types::{MemoryEvent, PersonalityTrait, Presentation, TraitLayer, TraitSource, TraitStatus},
};
use ramaria_memory::inference::{
    CategoryStats, CrossCategoryMetrics, InferrerConfig, MotiveStats, PhaseBSource,
    RepresentativeEvent, StatsConfig, StatsSummary, TentativePromotionConfig,
    promote_tentative_events, run_phase_a_stats, run_phase_b_inference, run_phase_c_update,
};

use common::mock_store::{MockLlm, MockStorage};

// =========================================================
// 多步 Mock LLM — 每次调用返回不同回复
// =========================================================

/// 多步 Mock LLM：按顺序返回预设回复列表，每次调用消耗一项。
///
/// 用于模拟三步推断中 LLM 按 Step 1→2→3 返回不同 JSON。
struct MultiStepLlm {
    replies: Vec<String>,
    call_count: AtomicUsize,
    capability: ramaria_core::types::ModelCapability,
    config: ramaria_core::types::BackendConfig,
}

impl MultiStepLlm {
    fn new(replies: Vec<String>) -> Self {
        Self {
            replies,
            call_count: AtomicUsize::new(0),
            capability: ramaria_core::types::ModelCapability {
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
    async fn chat(&self, _request: &ChatRequest) -> ramaria_core::RamariaResult<String> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        if idx >= self.replies.len() {
            return Err(ramaria_core::RamariaError::unsupported(format!(
                "MultiStepLlm exhausted: call {} exceeds {} replies",
                idx,
                self.replies.len()
            )));
        }
        Ok(self.replies[idx].clone())
    }

    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> ramaria_core::RamariaResult<
        Pin<
            Box<
                dyn futures::Stream<
                        Item = ramaria_core::RamariaResult<ramaria_core::traits::StreamDelta>,
                    > + Send,
            >,
        >,
    > {
        Err(ramaria_core::RamariaError::unsupported(
            "MultiStepLlm does not support streaming",
        ))
    }

    fn capability(&self) -> &ramaria_core::types::ModelCapability {
        &self.capability
    }

    fn config(&self) -> &ramaria_core::types::BackendConfig {
        &self.config
    }

    async fn validate(&self) -> ramaria_core::RamariaResult<()> {
        Ok(())
    }

    fn name(&self) -> &'static str {
        "MultiStepMockLlm"
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

// =========================================================
// Fixture 构建辅助
// =========================================================

/// 构造测试用 MemoryEvent（带 motives 字段和 situation_strength）。
#[allow(clippy::too_many_arguments)]
fn make_event(
    title: &str,
    summary: &str,
    keywords: Option<&str>,
    confidence: f64,
    salience: f64,
    valence: f64,
    share: f64,
    presentation: Presentation,
    attitude: Option<&str>,
    motives: Option<&str>,
    situation_strength: Option<i32>,
) -> MemoryEvent {
    let now = ramaria_core::types::now_ms();
    let mut ev = MemoryEvent::new(
        "persona-m4".into(),
        title.into(),
        summary.into(),
        now - 1000,
        now,
    );
    ev.keywords = keywords.map(|k| k.into());
    ev.confidence = confidence;
    ev.salience = salience;
    ev.valence = valence;
    ev.share = share;
    ev.presentation = presentation;
    ev.attitude = attitude.map(|a| a.into());
    ev.motives = motives.map(|m| m.into());
    ev.situation_strength = situation_strength;
    ev
}

/// 构建包含多样化事件（不同置信度/动机/情境强度）的 fixture。
fn make_diverse_events() -> Vec<MemoryEvent> {
    vec![
        // ---- 工作分类，高置信度 ----
        make_event(
            "项目验收",
            "完成项目验收",
            Some("工作,专业"),
            0.9,
            0.85,
            0.7,
            0.5,
            Presentation::Objective,
            Some("对成果满意"),
            Some("自主性"), // 动机: 自主性
            Some(3),        // 中性情境
        ),
        make_event(
            "加班赶工",
            "连续加班赶项目进度",
            Some("工作,压力"),
            0.75,
            0.7,
            -0.3,
            0.6,
            Presentation::Mixed,
            Some("疲惫但坚持"),
            Some("地位维护,自主性"), // 双动机
            Some(5),                 // 强情境
        ),
        make_event(
            "方案被否",
            "精心准备的方案被领导否决",
            Some("工作,权威"),
            0.65,
            0.6,
            -0.6,
            0.4,
            Presentation::Subjective,
            Some("挫败感强烈"),
            Some("地位维护,公平"), // 双动机
            Some(2),               // 弱情境（更能反映人格）
        ),
        // ---- 社交分类 ----
        make_event(
            "团建活动",
            "参加公司团建，主动组织游戏",
            Some("社交,活动"),
            0.8,
            0.75,
            0.5,
            0.8,
            Presentation::Mixed,
            Some("享受社交"),
            Some("归属"),
            Some(1), // 弱情境
        ),
        make_event(
            "朋友倾诉",
            "朋友深夜来电倾诉烦恼",
            Some("社交,情感"),
            0.7,
            0.65,
            0.2,
            0.9,
            Presentation::Subjective,
            Some("耐心倾听"),
            Some("归属,公平"),
            Some(3),
        ),
        // ---- tentative 事件（置信度 0.45-0.6） ----
        make_event(
            "潜在冲突",
            "和同事发生轻微意见分歧",
            Some("工作,冲突"),
            0.55,
            0.4,
            -0.2,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护"),
            Some(3),
        ),
        make_event(
            "匿名反馈",
            "收到匿名负面工作反馈",
            Some("工作,评价"),
            0.5,
            0.35,
            -0.4,
            0.3,
            Presentation::Subjective,
            Some("不安"),
            Some("地位维护"),
            Some(4),
        ),
        // ---- discarded 事件 ----
        make_event(
            "路过闲聊",
            "电梯中随口寒暄",
            Some("社交,日常"),
            0.3,
            0.15,
            0.0,
            0.2,
            Presentation::Mixed,
            None,
            None,
            Some(3),
        ),
    ]
}

/// 构造含动机统计的 StatsSummary。
fn make_m4_stats_summary() -> StatsSummary {
    StatsSummary {
        total_events_in: 8,
        total_events_filtered: 7, // 1 discarded
        confirmed_count: 5,
        tentative_count: 2,
        discarded_count: 1,
        category_count: 2,
        categories: vec![
            CategoryStats {
                category: "工作".into(),
                event_count: 5,
                n_eff: 4.2,
                valence_mean: -0.08,
                valence_std: 0.55,
                valence_positive_ratio: 0.40,
                share_mean: 0.46,
                share_std: 0.12,
                presentation_objective_ratio: 0.25,
                presentation_subjective_ratio: 0.45,
                presentation_mixed_ratio: 0.30,
                group_weight: 0.55,
            },
            CategoryStats {
                category: "社交".into(),
                event_count: 2,
                n_eff: 1.8,
                valence_mean: 0.35,
                valence_std: 0.20,
                valence_positive_ratio: 0.90,
                share_mean: 0.85,
                share_std: 0.05,
                presentation_objective_ratio: 0.10,
                presentation_subjective_ratio: 0.55,
                presentation_mixed_ratio: 0.35,
                group_weight: 0.30,
            },
        ],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.50,
            narrative_consistency: 0.65,
            attitude_contradiction_count: 1,
            share_skewness: 0.15,
            share_kurtosis: -0.30,
        },
        representative_events: vec![
            RepresentativeEvent {
                title: "项目验收".into(),
                summary: "完成项目验收".into(),
                attitude: Some("对成果满意".into()),
                valence: 0.7,
                salience: 0.85,
                category: "工作".into(),
            },
            RepresentativeEvent {
                title: "方案被否".into(),
                summary: "方案被否决".into(),
                attitude: Some("挫败感强烈".into()),
                valence: -0.6,
                salience: 0.6,
                category: "工作".into(),
            },
        ],
        motive_stats: vec![
            MotiveStats {
                motive: "地位维护".into(),
                event_count: 4,
                n_eff: 2.8,
                valence_mean: -0.35,
                valence_std: 0.20,
                valence_positive_ratio: 0.25,
                share_mean: 0.45,
                share_std: 0.12,
                presentation_objective_ratio: 0.10,
                presentation_subjective_ratio: 0.60,
                presentation_mixed_ratio: 0.30,
                avg_salience: 0.60,
            },
            MotiveStats {
                motive: "自主性".into(),
                event_count: 2,
                n_eff: 2.2,
                valence_mean: 0.20,
                valence_std: 0.45,
                valence_positive_ratio: 0.50,
                share_mean: 0.55,
                share_std: 0.10,
                presentation_objective_ratio: 0.40,
                presentation_subjective_ratio: 0.30,
                presentation_mixed_ratio: 0.30,
                avg_salience: 0.75,
            },
            MotiveStats {
                motive: "归属".into(),
                event_count: 2,
                n_eff: 2.0,
                valence_mean: 0.35,
                valence_std: 0.15,
                valence_positive_ratio: 0.80,
                share_mean: 0.85,
                share_std: 0.05,
                presentation_objective_ratio: 0.10,
                presentation_subjective_ratio: 0.55,
                presentation_mixed_ratio: 0.35,
                avg_salience: 0.70,
            },
        ],
    }
}

#[path = "inference/edge.rs"]
mod edge;
#[path = "inference/motive.rs"]
mod motive;
#[path = "inference/phase_a.rs"]
mod phase_a;
#[path = "inference/phase_b.rs"]
mod phase_b;
#[path = "inference/pipeline.rs"]
mod pipeline;
#[path = "inference/promotion.rs"]
mod promotion;
#[path = "inference/traits.rs"]
mod traits;
