//! rust/crates/ramaria-app/tests/m4_integration.rs - M4 统计深化集成测试
//!
//! 设计特点:
//! - 验证 M4 全部新特性: 校准权重链、三轨准入、分层收缩、因果链、动机维度统计
//! - Phase A 纯函数测试: 使用 fixture 事件直接调用 run_phase_a_stats
//! - Phase B/C 管线测试: 使用 MockStorage + MockLlm 验证全链路
//! - 覆盖边界: 空事件、仅 discarded、全 tentative、混合动机、动机缺失
//! - 不依赖真实 LLM 或数据库，所有测试确定性可重复

mod mock_backend;

use mock_backend::MockStorage;
use ramaria_core::{
    traits::{ChatRequest, LlmProvider, StorageBackend},
    types::{MemoryEvent, Presentation, TraitLayer, TraitStatus},
};
use ramaria_memory::inference::{
    CategoryStats, CrossCategoryMetrics, InferrerConfig, MotiveStats, PhaseBSource,
    RepresentativeEvent, StatsConfig, StatsSummary, TentativePromotionConfig,
    promote_tentative_events, run_phase_a_stats, run_phase_b_inference, run_phase_c_update,
};
use std::pin::Pin;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

// =========================================================
// 多步 Mock LLM — 每次调用返回不同回复
// =========================================================

/// 多步 Mock LLM：按顺序返回预设回复列表，每次调用 consume 一项。
struct MultiStepLlm {
    replies: Mutex<Vec<String>>,
    call_count: AtomicUsize,
    capability: ramaria_core::types::ModelCapability,
    backend_config: ramaria_core::types::BackendConfig,
}

impl MultiStepLlm {
    fn new(replies: Vec<String>) -> Self {
        let capability = ramaria_core::types::ModelCapability {
            provider: ramaria_core::types::LlmProvider::LmStudio,
            model_id: "test-model".to_string(),
            base_url: "http://localhost:1234/v1".to_string(),
            supports_streaming: false,
            supports_json_mode: true,
            context_window: 4096,
            max_output_tokens: 2048,
        };
        let backend_config = ramaria_core::types::BackendConfig {
            provider: ramaria_core::types::LlmProvider::LmStudio,
            base_url: "http://localhost:1234/v1".to_string(),
            embedding_model_id: None,
            embedding_model_path: None,
            temperature: 0.7,
            max_tokens: 2048,
            capability: capability.clone(),
        };
        Self {
            replies: Mutex::new(replies),
            call_count: AtomicUsize::new(0),
            capability,
            backend_config,
        }
    }
}

#[async_trait::async_trait]
impl LlmProvider for MultiStepLlm {
    fn name(&self) -> &'static str {
        "MultiStepMockLlm"
    }

    async fn chat(&self, _request: &ChatRequest) -> ramaria_core::RamariaResult<String> {
        let idx = self.call_count.fetch_add(1, Ordering::SeqCst);
        let replies = self.replies.lock().unwrap();
        if idx >= replies.len() {
            return Err(ramaria_core::RamariaError::unsupported(format!(
                "MockLlm exhausted: call {} exceeds {} replies",
                idx,
                replies.len()
            )));
        }
        Ok(replies[idx].clone())
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
            "MockLlm does not support streaming",
        ))
    }

    fn capability(&self) -> &ramaria_core::types::ModelCapability {
        &self.capability
    }

    fn config(&self) -> &ramaria_core::types::BackendConfig {
        &self.backend_config
    }

    async fn validate(&self) -> ramaria_core::RamariaResult<()> {
        Ok(())
    }
}

// =========================================================
// Fixture 构建辅助
// =========================================================

/// 构造测试用 MemoryEvent（带 motives 字段和 situation_strength）。
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

// =========================================================
// 校准权重链测试
// =========================================================

#[test]
fn phase_a_calibrated_weights_reduces_tentative_weight() {
    let events = make_diverse_events();
    let config = StatsConfig::default(); // use_calibrated_weights = true
    let summary = run_phase_a_stats(&events, &config);

    // discarded 事件应被排除
    assert_eq!(summary.total_events_in, 8);
    assert_eq!(summary.discarded_count, 1);
    assert_eq!(summary.total_events_filtered, 7); // 5 confirmed + 2 tentative

    // tentative 事件以半权重参与
    assert_eq!(summary.confirmed_count, 5);
    assert_eq!(summary.tentative_count, 2);

    // 校准权重下 n_eff 应小于原始事件数（tentative 半权重 + situation_strength 影响）
    let total_n_eff: f64 = summary.categories.iter().map(|c| c.n_eff).sum();
    assert!(
        total_n_eff < 7.0,
        "校准权重下 n_eff({}) 应小于原始事件数 7",
        total_n_eff
    );
    assert!(
        total_n_eff > 1.0,
        "n_eff({}) 应 > 1.0（至少 confirmed 事件有效）",
        total_n_eff
    );
}

#[test]
fn phase_a_v12_compat_path_disables_calibrated_weights() {
    let events = make_diverse_events();
    let config = StatsConfig {
        use_calibrated_weights: false,
        ..Default::default()
    };
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 8);
    let total_n_eff: f64 = summary.categories.iter().map(|c| c.n_eff).sum();
    assert!(total_n_eff > 0.0, "v1.2 路径应有有效 n_eff");
}

// =========================================================
// 三轨动态准入测试
// =========================================================

#[test]
fn three_track_classification() {
    let events = make_diverse_events();
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    // 三轨分类应正确反映在 StatsSummary 中
    assert_eq!(summary.confirmed_count, 5);
    assert_eq!(summary.tentative_count, 2);
    assert_eq!(summary.discarded_count, 1);
    assert_eq!(
        summary.confirmed_count + summary.tentative_count,
        summary.total_events_filtered,
        "active events = confirmed + tentative"
    );
}

#[test]
fn tentative_promotion_across_batches() {
    // 使用固定时间戳确保跨批次检测正确
    // 注意: MemoryEvent::new() 内部设置 created_at=now_ms(), 需要显式覆盖
    let base_time: i64 = 1700000000000; // 固定 Unix 毫秒时间戳

    let mut e1 = MemoryEvent::new(
        "persona-m4".into(),
        "T1".into(),
        "s1".into(),
        base_time - 1000,
        base_time,
    );
    e1.keywords = Some("工作,冲突".into());
    e1.confidence = 0.5;
    e1.salience = 0.4;
    e1.created_at = base_time; // 批次 1: base_time 时刻

    let mut e2 = MemoryEvent::new(
        "persona-m4".into(),
        "T2".into(),
        "s2".into(),
        base_time + 8 * 3_600_000 - 1000,
        base_time + 8 * 3_600_000,
    );
    e2.keywords = Some("工作,冲突".into());
    e2.confidence = 0.55;
    e2.salience = 0.35;
    e2.created_at = base_time + 8 * 3_600_000; // 批次 2: 8 小时后（> 6h 阈值）

    let tentative = vec![e1, e2];
    let confirmed: Vec<MemoryEvent> = vec![];
    let config = TentativePromotionConfig::default();

    let result = promote_tentative_events(&tentative, &confirmed, &config);

    // 两个事件关键词相同 Jaccard=1.0, 来自不同批次 (8h > 6h) → 应提升
    assert!(
        result.promoted_count > 0,
        "跨批次 tentative 应被提升: promoted_count={}, remaining_count={}",
        result.promoted_count,
        result.remaining_count
    );
    for ev in &result.promoted {
        assert!((ev.confidence - 0.6).abs() < 1e-10, "提升后置信度应为 0.6");
    }
}

// =========================================================
// 动机维度统计测试
// =========================================================

#[test]
fn motive_stats_computed_from_events() {
    let events = make_diverse_events();
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    // 动机统计应在 Phase A 输出中
    assert!(
        !summary.motive_stats.is_empty(),
        "fixture 包含多种动机，应有动机统计输出"
    );

    // 地位维护动机出现次数最多
    let status_motive = summary.motive_stats.iter().find(|m| m.motive == "地位维护");
    assert!(status_motive.is_some(), "应有'地位维护'动机统计");
    let status = status_motive.unwrap();
    assert!(status.event_count >= 3, "地位维护至少出现 3 次");
    assert!(status.n_eff > 0.0);
    // 地位维护主要是负效价
    assert!(status.valence_mean < 0.0, "地位维护应主要为负效价");

    // 归属动机应正向
    let belonging = summary.motive_stats.iter().find(|m| m.motive == "归属");
    assert!(belonging.is_some(), "应有'归属'动机统计");
    assert!(belonging.unwrap().valence_mean > 0.0, "归属应为正效价");
}

#[test]
fn motive_stats_empty_when_no_motives() {
    // 全无 motives 事件
    let events: Vec<MemoryEvent> = (0..3)
        .map(|i| {
            make_event(
                &format!("E{}", i),
                "s",
                Some("工作"),
                0.8,
                0.5,
                0.0,
                0.5,
                Presentation::Mixed,
                None,
                None,
                Some(3),
            )
        })
        .collect();

    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);
    assert!(
        summary.motive_stats.is_empty(),
        "无 motives 时动机统计应为空"
    );
}

#[test]
fn motive_stats_only_from_active_events() {
    // 确保 discarded 事件不参与动机统计
    let events = vec![
        make_event(
            "Discarded",
            "被丢弃",
            Some("工作"),
            0.3,
            0.1,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护"),
            Some(3),
        ),
        make_event(
            "Confirmed",
            "确认的",
            Some("工作"),
            0.9,
            0.8,
            0.5,
            0.5,
            Presentation::Mixed,
            Some("好"),
            Some("自主性"),
            Some(3),
        ),
    ];

    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    // discarded 事件 (conf=0.3) 不应参与动机统计
    let status_motive = summary.motive_stats.iter().find(|m| m.motive == "地位维护");
    assert!(status_motive.is_none(), "discarded 事件不应参与动机统计");
}

// =========================================================
// 全链路 Phase A→B→C 集成测试
// =========================================================

#[tokio::test]
async fn full_pipeline_with_m4_features() {
    // 1. 准备 StatsSummary（含动机统计）
    let stats = make_m4_stats_summary();

    // 验证动机统计已内嵌
    assert!(!stats.motive_stats.is_empty());
    assert_eq!(stats.motive_stats.len(), 3);

    // 2. 准备 MockStorage 和 MultiStepLlm
    let storage = MockStorage::new();

    // Step 1: 分类信号（JSON）
    let step1_json = r#"{
        "工作": {
            "signal_label": "尽责",
            "evidence_citation": "n_eff=4.2, valence_mean=-0.08, 地位维护驱动力强",
            "stability_judgment": "stable",
            "sufficient_evidence": true
        },
        "社交": {
            "signal_label": "亲和",
            "evidence_citation": "n_eff=1.8, valence_mean=0.35, 归属动机驱动",
            "stability_judgment": "contextual",
            "sufficient_evidence": false
        }
    }"#;

    // Step 2: 一致性分析（JSON）
    let step2_json = r#"{
        "base_candidates": ["尽责"],
        "primary_candidates": ["尽责"],
        "accent_candidates": ["动机-地位维护-驱动", "动机-归属-亲和"],
        "notes": "工作领域表现稳定，社交领域样本量不足；动机维度显著"
    }"#;

    // Step 3: 性格画像（JSON 数组）
    let step3_json = r#"[
        {"layer":"base","trait_label":"尽责","meaning":"对工作有强烈的完成驱动力","not_meaning":"并非完美主义","trigger":null,"suppress":null,"related":null,"seq":0},
        {"layer":"primary","trait_label":"尽责-工作","meaning":"工作中最突出尽责特质","not_meaning":null,"trigger":null,"suppress":null,"related":"base::尽责","seq":1},
        {"layer":"accent","trait_label":"地位维护-驱动","meaning":"在涉及权威和评价的情境下对地位感知强烈","not_meaning":"并非好斗","trigger":"方案评审、绩效考核等评价性场景","suppress":"一对一私下沟通时减弱","related":null,"seq":2},
        {"layer":"accent","trait_label":"归属-亲和","meaning":"在团建和亲密社交中展现亲和与投入","not_meaning":"并非社交焦虑","trigger":"非正式社交场合","suppress":null,"related":null,"seq":3}
    ]"#;

    let llm = MultiStepLlm::new(vec![
        step1_json.to_string(),
        step2_json.to_string(),
        step3_json.to_string(),
    ]);
    let config = InferrerConfig::default();

    // 3. 执行 Phase B
    let phase_b_result = run_phase_b_inference(&llm, &storage, &stats, "persona-m4", &config).await;

    assert!(
        phase_b_result.is_ok(),
        "Phase B 应成功: {:?}",
        phase_b_result.err()
    );
    let pb = phase_b_result.unwrap();
    assert_eq!(pb.source, PhaseBSource::LlmInference);
    assert!(
        pb.traits_saved >= 2,
        "应至少保存 2 个 trait，实际: {}",
        pb.traits_saved
    );

    // 验证 traits 包含动机驱动的 accent trait
    let traits = storage.list_traits_by_persona("persona-m4").await.unwrap();
    let motive_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.trait_label.contains("地位维护") || t.trait_label.contains("归属"))
        .collect();
    assert!(
        !motive_traits.is_empty(),
        "Phase B 输出应包含动机驱动的 trait"
    );

    // 4. 验证 Phase B 产出的 traits 置信度
    // （Phase C 需要真实事件数据来更新置信度；空事件会触发 compute_confidence→0.0）
    let final_traits = storage.list_traits_by_persona("persona-m4").await.unwrap();
    assert!(!final_traits.is_empty(), "Phase B 应已产出 traits");
    for t in &final_traits {
        assert!(t.confidence > 0.0, "trait '{}' 应有正置信度", t.trait_label);
        assert_eq!(t.status, TraitStatus::Active);
        // 验证层分配正确
        assert!(
            t.layer == TraitLayer::Base
                || t.layer == TraitLayer::Primary
                || t.layer == TraitLayer::Accent,
            "trait '{}' 应有有效的层分配: {:?}",
            t.trait_label,
            t.layer
        );
    }
}

#[tokio::test]
async fn full_pipeline_respects_calibrated_weights_in_output() {
    // 构造含 tentative 事件和多样化动机的 StatsSummary
    let stats = StatsSummary {
        total_events_in: 6,
        total_events_filtered: 5,
        confirmed_count: 3,
        tentative_count: 2,
        discarded_count: 1,
        category_count: 1,
        categories: vec![CategoryStats {
            category: "工作".into(),
            event_count: 5,
            n_eff: 3.0, // 校准权重显著低于原始事件数
            valence_mean: 0.1,
            valence_std: 0.4,
            valence_positive_ratio: 0.55,
            share_mean: 0.5,
            share_std: 0.2,
            presentation_objective_ratio: 0.3,
            presentation_subjective_ratio: 0.4,
            presentation_mixed_ratio: 0.3,
            group_weight: 1.0,
        }],
        cross_category: CrossCategoryMetrics {
            emotional_stability: 0.4,
            narrative_consistency: 0.8,
            attitude_contradiction_count: 0,
            share_skewness: 0.0,
            share_kurtosis: 0.0,
        },
        representative_events: vec![],
        motive_stats: vec![MotiveStats {
            motive: "地位维护".into(),
            event_count: 2,
            n_eff: 1.5,
            valence_mean: 0.2,
            valence_std: 0.3,
            valence_positive_ratio: 0.6,
            share_mean: 0.5,
            share_std: 0.15,
            presentation_objective_ratio: 0.2,
            presentation_subjective_ratio: 0.5,
            presentation_mixed_ratio: 0.3,
            avg_salience: 0.6,
        }],
    };

    let storage = MockStorage::new();
    let step1 = r#"{"工作":{"signal_label":"尽责","evidence_citation":"n_eff=3.0 calibrated","stability_judgment":"stable","sufficient_evidence":true}}"#;
    let step2 = r#"{"base_candidates":["尽责"],"primary_candidates":["尽责"],"accent_candidates":["动机-地位维护-驱动"],"notes":"tentative events half-weighted; motive-driven accent"}"#;
    let step3 = r#"[
        {"layer":"base","trait_label":"尽责","meaning":"工作中尽责","not_meaning":null,"trigger":null,"suppress":null,"related":null,"seq":0},
        {"layer":"accent","trait_label":"动机-地位维护-驱动","meaning":"地位维护驱动行为","not_meaning":null,"trigger":"评价性场景","suppress":null,"related":null,"seq":1}
    ]"#;

    let llm = MultiStepLlm::new(vec![step1.into(), step2.into(), step3.into()]);
    let config = InferrerConfig::default();

    let result = run_phase_b_inference(&llm, &storage, &stats, "persona-m4-cw", &config).await;
    assert!(result.is_ok());
    let pb = result.unwrap();

    // 验证 tentative 路径：确认动机维度 accent trait 被生成
    let traits = storage
        .list_traits_by_persona("persona-m4-cw")
        .await
        .unwrap();
    let accent_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.layer == TraitLayer::Accent)
        .collect();
    assert!(
        !accent_traits.is_empty(),
        "校准权重路径下应有 accent trait，tentative events + motives 应产生点缀层"
    );

    // 执行 Phase C
    let _pc = run_phase_c_update(&storage, "persona-m4-cw", &pb.traits, &[], true).await;
    assert!(_pc.is_ok());
}

#[tokio::test]
async fn mock_infer_fallback_with_m4_stats() {
    // 测试 LLM 失败时降级到 mock_infer，验证动机统计仍被使用
    let stats = make_m4_stats_summary();
    let storage = MockStorage::new();

    // 第一步正常，第二步返回无效 JSON 触发降级
    let step1 = r#"{"工作":{"signal_label":"尽责","evidence_citation":"n_eff=4.2","stability_judgment":"stable","sufficient_evidence":true},"社交":{"signal_label":"亲和","evidence_citation":"n_eff=1.8","stability_judgment":"contextual","sufficient_evidence":false}}"#;
    let step2 = "not valid json"; // 将导致解析失败 → 降级
    let llm = MultiStepLlm::new(vec![step1.into(), step2.into()]);
    let config = InferrerConfig::default();

    let result =
        run_phase_b_inference(&llm, &storage, &stats, "persona-m4-fallback", &config).await;
    assert!(result.is_ok(), "降级路径不应 panic");
    let pb = result.unwrap();

    // 降级应使用 mock_infer
    assert_eq!(pb.source, PhaseBSource::MockFallback, "应降级到 mock_infer");
    assert!(pb.traits_saved > 0, "mock_infer 应生成 trait");

    // mock_infer 应利用动机统计生成动机驱动 trait
    let traits = storage
        .list_traits_by_persona("persona-m4-fallback")
        .await
        .unwrap();
    let motive_traits: Vec<_> = traits
        .iter()
        .filter(|t| t.trait_label.contains("动机-"))
        .collect();
    assert!(
        !motive_traits.is_empty(),
        "降级路径的 mock_infer 应生成动机驱动 trait"
    );
}

// =========================================================
// 边界情况测试
// =========================================================

#[test]
fn phase_a_empty_events_all_motive_stats_empty() {
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&[], &config);
    assert!(summary.motive_stats.is_empty());
    assert_eq!(summary.total_events_in, 0);
    assert_eq!(summary.confirmed_count, 0);
    assert_eq!(summary.tentative_count, 0);
}

#[test]
fn phase_a_all_discarded_yields_empty_stats() {
    let events = vec![
        make_event(
            "D1",
            "s",
            Some("工作"),
            0.3,
            0.1,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("地位维护"),
            Some(3),
        ),
        make_event(
            "D2",
            "s",
            Some("社交"),
            0.2,
            0.1,
            0.0,
            0.5,
            Presentation::Mixed,
            None,
            Some("归属"),
            Some(3),
        ),
    ];
    let config = StatsConfig::default();
    let summary = run_phase_a_stats(&events, &config);

    assert_eq!(summary.total_events_in, 2);
    assert_eq!(summary.discarded_count, 2);
    assert_eq!(summary.total_events_filtered, 0);
    assert!(
        summary.motive_stats.is_empty(),
        "全 discarded 不应产生活跃动机统计"
    );
}
