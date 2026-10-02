//! crates/ramaria-memory/src/behavior/rule_gen/tests.rs - //! crates/ramaria-memory/src/behavior/rule_gen.rs - 行为规则生成单元测试
//!
//! 设计特点:
//! - 位于 behavior::rule_gen 模块内，经 use super::* 取用被测项（含私有项）。
//! - 由 rule_gen.rs 以 #[cfg(test)] mod tests; 收纳，与生产代码物理隔离。
//! - 用例为确定性断言，可离线运行。
use super::*;
use ramaria_core::behavior::BehaviorSituation;
use ramaria_core::types::Presentation;

// ---- mock LLM ----

struct MockRuleLlm {
    /// 每次调用返回的内容（可注入失败序列）
    responses: std::sync::Mutex<Vec<String>>,
    calls: std::sync::atomic::AtomicUsize,
    capability: ramaria_core::types::ModelCapability,
    config: ramaria_core::types::BackendConfig,
}

impl MockRuleLlm {
    fn new(responses: Vec<&str>) -> Self {
        Self {
            responses: std::sync::Mutex::new(responses.iter().map(|s| s.to_string()).collect()),
            calls: std::sync::atomic::AtomicUsize::new(0),
            capability: ramaria_core::types::ModelCapability {
                provider: ramaria_core::types::LlmProvider::LmStudio,
                model_id: "mock".into(),
                base_url: "http://localhost:1234/v1".into(),
                supports_streaming: false,
                supports_json_mode: false,
                context_window: 4096,
                max_output_tokens: 4096,
            },
            config: ramaria_core::types::BackendConfig::lm_studio_default(),
        }
    }
    fn call_count(&self) -> usize {
        self.calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[async_trait::async_trait]
impl LlmProviderTrait for MockRuleLlm {
    async fn chat(&self, _request: &ChatRequest) -> RamariaResult<String> {
        let n = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let mut guard = self.responses.lock().unwrap();
        if guard.is_empty() {
            return Err(RamariaError::llm("mock LLM 响应耗尽"));
        }
        let _ = n;
        Ok(guard.remove(0))
    }
    async fn chat_stream(
        &self,
        _request: &ChatRequest,
    ) -> RamariaResult<
        std::pin::Pin<
            Box<
                dyn futures::Stream<Item = RamariaResult<ramaria_core::traits::StreamDelta>> + Send,
            >,
        >,
    > {
        Err(RamariaError::unsupported("mock 不支持流式"))
    }
    fn capability(&self) -> &ramaria_core::types::ModelCapability {
        &self.capability
    }
    fn config(&self) -> &ramaria_core::types::BackendConfig {
        &self.config
    }
    async fn validate(&self) -> RamariaResult<()> {
        Ok(())
    }
    fn name(&self) -> &'static str {
        "MockRuleLlm"
    }
}

// ---- 辅助：构造 RefinedCluster ----

fn make_cluster(
    valence_mean: f64,
    valence_std: f64,
    sample_count: usize,
    n_eff: f64,
) -> RefinedCluster {
    RefinedCluster {
        situation: BehaviorSituation {
            keywords: vec!["加班".into(), "累".into()],
            centroid: None,
            response_centroid: None,
            valence_mean,
            valence_std,
            sample_count,
            presentation_dist: vec![
                ramaria_core::behavior::PresentationFreq {
                    presentation: Presentation::Subjective,
                    freq: 0.7,
                },
                ramaria_core::behavior::PresentationFreq {
                    presentation: Presentation::Objective,
                    freq: 0.3,
                },
            ],
            situation_strength_mean: 3.5,
            time_span_days: 20.0,
            trait_refs: Vec::new(),
        },
        n_eff,
        cohesion: 0.8,
        quality: 0.6,
        member_event_ids: (1..=sample_count as i64).collect(),
        member_events: (1..=sample_count as i64)
            .map(|id| crate::behavior::clustering::ClusterMember {
                event_id: id,
                // 默认近 5 天内的近期事件（recency_factor ≈ 1.0）
                start_ms: ramaria_core::types::now_ms() - 5 * 86_400_000,
                salience: 0.5,
            })
            .collect(),
    }
}

// ---- 极性一致性校验（表驱动） ----

#[test]
fn polarity_consistency_cases() {
    // 输入-期望表：(文本, 簇 valence, 期望一致)
    let cases: [(&str, f64, bool); 4] = [
        ("我也很难过，压力好大", -0.4, true), // 文本消极 + 簇消极 → 一致
        ("太好了真开心", -0.4, false),        // 文本积极 + 簇消极 → 不一致（降级候选）
        ("别担心，会好的，我支持你", 0.5, true), // 文本积极 + 簇积极 → 一致
        ("", -0.4, true),                     // 中性文本（无词典词）→ 无信息不误判
    ];
    for (text, cluster_valence, expected) in cases {
        let v = check_polarity(polarity_of_text(text), cluster_valence);
        assert_eq!(
            v.consistent, expected,
            "文本 {text:?} 对簇 valence {cluster_valence} 的极性一致结果应为 {expected}"
        );
    }
    // 附加：消极文本极性为负；无词典词文本极性≈0
    let v = check_polarity(polarity_of_text("我也很难过，压力好大"), -0.4);
    assert!(
        v.text_polarity < 0.0,
        "消极文本极性应为负，实际 {}",
        v.text_polarity
    );
    let neutral = check_polarity(polarity_of_text(""), -0.4);
    assert!(
        neutral.text_polarity.abs() < 0.1,
        "无词典词文本极性应≈0，实际 {}",
        neutral.text_polarity
    );
}

// ---- 质控门槛（表驱动） ----

#[test]
fn quality_gate_cases() {
    // 输入-期望表：(valence_mean, valence_std, sample_count, n_eff, 期望结论)
    let config = RuleGenConfig::default();
    let cases: [(f64, f64, usize, f64, QualityVerdict); 4] = [
        (-0.4, 0.2, 6, 6.0, QualityVerdict::Pass),
        (
            -0.4,
            0.2,
            3,
            3.0,
            QualityVerdict::Degrade(RuleDegradeReason::LowEvidence),
        ),
        (
            -0.4,
            0.2,
            8,
            2.0,
            QualityVerdict::Degrade(RuleDegradeReason::LowNeff),
        ),
        (
            -0.2,
            0.8,
            8,
            8.0,
            QualityVerdict::Degrade(RuleDegradeReason::HighValenceVariance),
        ),
    ];
    for (valence_mean, valence_std, sample_count, n_eff, expected) in cases {
        let cluster = make_cluster(valence_mean, valence_std, sample_count, n_eff);
        assert_eq!(
            quality_gate(&cluster, &config),
            expected,
            "簇(vm={valence_mean}, vs={valence_std}, n={sample_count}, n_eff={n_eff}) 结论应为 {expected:?}"
        );
    }
}

// ---- avoid 校验 ----

#[test]
fn avoid_filter_removes_conflict_in_positive_cluster() {
    let mut cluster = make_cluster(0.6, 0.2, 6, 6.0);
    cluster.situation.keywords = vec!["旅行".into(), "开心".into()];
    let filtered = validate_avoid(&["旅行".into(), "随便".into()], &cluster);
    // "旅行" 与积极簇关键词冲突 → 移除；"随便" 无关保留
    assert_eq!(filtered, vec!["随便"]);
}

#[test]
fn avoid_kept_in_negative_cluster() {
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let filtered = validate_avoid(&["加班".into(), "深夜".into()], &cluster);
    assert_eq!(filtered.len(), 2, "消极簇不过滤 avoid");
}

#[test]
fn avoid_kept_when_no_keywords() {
    let mut cluster = make_cluster(0.6, 0.2, 6, 6.0);
    cluster.situation.keywords = Vec::new();
    let filtered = validate_avoid(&["加班".into()], &cluster);
    assert_eq!(filtered, vec!["加班"]);
}

// ---- 参数化 ----

#[test]
fn parameterize_maps_valence_and_presentation() {
    let cluster = make_cluster(-0.5, 0.2, 6, 6.0);
    let p = parameterize(&cluster);
    assert!(
        (p.emotional_intensity + 0.5).abs() < 1e-9,
        "情感强度 = 加权 valence"
    );
    // 主观占比 0.7 → 主动程度 0.4+0.6*0.7=0.82
    assert!((p.proactiveness - 0.82).abs() < 1e-9);
    // 客观占比 0.3 → 正式度 0.4+0.6*0.3=0.58
    assert!((p.formality - 0.58).abs() < 1e-9);
    // 详细度随 |valence| 增加
    assert!(p.detail_level > 0.5);
}

// ---- 近期加权 ----

#[test]
fn recency_factor_inside_window_is_one() {
    let now = 2_000_000_000_000i64;
    assert_eq!(recency_factor(now - 86_400_000, now, 30), 1.0);
    assert_eq!(recency_factor(now, now, 30), 1.0);
}

#[test]
fn recency_factor_decays_after_window() {
    let now = 2_000_000_000_000i64;
    let f = recency_factor(now - 60 * 86_400_000, now, 30); // 60 天前
    assert!(f < 1.0 && f > 0.0, "窗口外衰减，实际 {f}");
    let f2 = recency_factor(now - 10 * 86_400_000, now, 30);
    assert_eq!(f2, 1.0);
    assert!(f < f2, "越旧权重越低");
}

#[test]
fn evidence_weight_clamps_range() {
    assert!((0.0..=1.0).contains(&evidence_weight(0.8, 1, 2, 30)));
}

/// T-V17-1-004：主路径下，旧事件 weight < 新事件 weight（D-V16-007 近期加权）。
#[test]
fn build_evidence_near_event_outweighs_old_event() {
    let now = ramaria_core::types::now_ms();
    let day = 86_400_000i64;
    // 两个真实事件，salience 相同：一新（昨天）、一旧（200 天前）。
    let cluster = RefinedCluster {
        situation: BehaviorSituation {
            keywords: vec!["加班".into(), "累".into()],
            centroid: None,
            response_centroid: None,
            valence_mean: -0.4,
            valence_std: 0.2,
            sample_count: 2,
            presentation_dist: Vec::new(),
            situation_strength_mean: 3.5,
            time_span_days: 200.0,
            trait_refs: Vec::new(),
        },
        n_eff: 2.0,
        cohesion: 0.8,
        quality: 0.6,
        member_event_ids: vec![1, 2],
        member_events: vec![
            crate::behavior::clustering::ClusterMember {
                event_id: 1,
                start_ms: now - day, // 近期
                salience: 0.8,
            },
            crate::behavior::clustering::ClusterMember {
                event_id: 2,
                start_ms: now - 200 * day, // 很旧
                salience: 0.8,
            },
        ],
    };
    let ev = build_evidence(&cluster, now, 30);
    let near = ev.iter().find(|(id, _)| *id == 1).expect("有新事件").1;
    let old = ev.iter().find(|(id, _)| *id == 2).expect("有旧事件").1;
    assert!(
        old < near,
        "旧事件 weight 必须小于新事件 weight（D-V16-007 近期加权主路径）: old={old}, new={near}"
    );
}

// ---- 置信度与稳定性 ----

#[test]
fn confidence_scales_with_evidence_and_consistency() {
    let healthy = make_cluster(-0.4, 0.2, 10, 10.0);
    let c = compute_confidence(&healthy, &RuleGenConfig::default());
    assert!(c > 0.8, "证据足且一致 → 高置信，实际 {c}");

    let noisy = make_cluster(-0.2, 1.0, 10, 10.0);
    let c2 = compute_confidence(&noisy, &RuleGenConfig::default());
    assert!(c2 < c, "valence 方差大 → 低置信");
}

#[test]
fn stability_requires_time_span() {
    let mut cluster = make_cluster(-0.4, 0.1, 6, 6.0);
    cluster.situation.time_span_days = 0.0; // 同一时刻 → 时间积累因子 0.5
    let s0 = compute_stability(&cluster);
    cluster.situation.time_span_days = 30.0;
    let s30 = compute_stability(&cluster);
    assert!(s30 > s0, "跨度越大越稳定");
    assert!((0.0..=1.0).contains(&s0));
}

// ---- LLM 翻译与 JSON 解析 ----

#[test]
fn parse_translation_valid_json() {
    let raw =
        r#"{"reaction": "当聊到加班时，倾向表达疲惫并安慰对方。", "avoid": ["深夜", "加班"]}"#;
    let (reaction, avoid) = parse_translation(raw).expect("解析成功");
    assert_eq!(
        reaction.as_deref(),
        Some("当聊到加班时，倾向表达疲惫并安慰对方。")
    );
    assert_eq!(avoid, vec!["深夜", "加班"]);
}

#[test]
fn parse_translation_tolerates_surrounding_text() {
    let raw = "好的，以下是规则：\n{\"reaction\": \"会安慰对方\", \"avoid\": []}\n请查收。";
    let (reaction, avoid) = parse_translation(raw).expect("宽容解析成功");
    assert_eq!(reaction.as_deref(), Some("会安慰对方"));
    assert!(avoid.is_empty());
}

#[test]
fn parse_translation_missing_reaction_ok() {
    let (reaction, avoid) = parse_translation(r#"{"avoid": ["x"]}"#).expect("解析成功");
    assert!(reaction.is_none());
    assert_eq!(avoid, vec!["x"]);
}

#[test]
fn parse_translation_invalid_json_errors() {
    assert!(parse_translation("没有 JSON").is_err());
    assert!(parse_translation("{").is_err());
}

#[tokio::test]
async fn translate_reaction_success_path() {
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "当聊到加班时，倾向表达疲惫并安慰对方。", "avoid": ["深夜"]}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let cfg = RuleGenConfig::default();
    let out = translate_reaction(&llm, &cluster, &cfg)
        .await
        .expect("成功");
    let (reaction, avoid) = out.expect("应返回规则");
    assert!(reaction.contains("加班"));
    assert_eq!(avoid, vec!["深夜"], "消极簇不过滤 avoid");
    assert_eq!(llm.call_count(), 1);
}

#[tokio::test]
async fn translate_reaction_polarity_retry_once_then_succeed() {
    // 第一次翻译极性错误（积极文本 vs 消极簇），第二次正确 → 共 2 次调用
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "太棒了，庆祝一下！", "avoid": []}"#,
        r#"{"reaction": "辛苦了，别太累，我陪着你。", "avoid": ["深夜"]}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let out = translate_reaction(&llm, &cluster, &RuleGenConfig::default())
        .await
        .expect("成功");
    assert!(out.is_some(), "重试后应成功");
    assert_eq!(llm.call_count(), 2, "重试 1 次");
}

#[tokio::test]
async fn translate_reaction_polarity_mismatch_degrades_to_none() {
    // 两次都极性错误 → None（降级候选规则，仅参数注入）
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "太棒了，庆祝一下！", "avoid": []}"#,
        r#"{"reaction": "太好了，真开心！", "avoid": []}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let out = translate_reaction(&llm, &cluster, &RuleGenConfig::default())
        .await
        .expect("成功");
    assert!(out.is_none(), "极性不一致应降级");
    assert_eq!(llm.call_count(), 2, "重试到上限");
}

#[tokio::test]
async fn translate_reaction_llm_failure_returns_none() {
    let llm = MockRuleLlm::new(vec![]); // 无响应 → 全部失败
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let out = translate_reaction(&llm, &cluster, &RuleGenConfig::default())
        .await
        .expect("成功");
    assert!(out.is_none(), "LLM 失败应降级不报错");
}

#[tokio::test]
async fn translate_reaction_skips_on_invalid_json() {
    // 输出非 JSON → 重试后仍失败 → None
    let llm = MockRuleLlm::new(vec!["这不是 JSON", "还是不是 JSON"]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let out = translate_reaction(&llm, &cluster, &RuleGenConfig::default())
        .await
        .expect("成功");
    assert!(out.is_none());
    assert_eq!(llm.call_count(), 2);
}

// ---- 生成编排 ----

#[tokio::test]
async fn generate_rule_full_rule_when_quality_passes() {
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "当聊到加班时，倾向表达疲惫并安慰对方。", "avoid": ["深夜"]}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::default(), &llm);
    let out = generator.generate_rule(&cluster).await;
    assert_eq!(out.degrade, RuleDegradeReason::None);
    assert!(out.rule.has_reaction());
    assert_eq!(out.rule.source, RuleSource::Auto);
    assert!(out.rule.enabled, "Auto 规则自动生效");
    assert_eq!(out.rule.evidence.len(), 6, "证据链完整");
    assert!((out.rule.params.emotional_intensity + 0.4).abs() < 1e-9);
    assert!(out.rule.confidence > 0.5);
}

#[tokio::test]
async fn generate_rule_degrades_to_candidate_on_low_evidence() {
    let llm = MockRuleLlm::new(vec![r#"{"reaction": "x", "avoid": []}"#]);
    let cluster = make_cluster(-0.4, 0.2, 3, 3.0); // 证据量 3 < 5
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::default(), &llm);
    let out = generator.generate_rule(&cluster).await;
    assert_eq!(out.degrade, RuleDegradeReason::LowEvidence);
    assert!(out.rule.is_candidate(), "候选规则仅参数注入");
    assert_eq!(llm.call_count(), 0, "质控不通过不调 LLM");
}

#[tokio::test]
async fn generate_rule_degrades_on_polarity_mismatch() {
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "太棒了！", "avoid": []}"#,
        r#"{"reaction": "太好了！", "avoid": []}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::default(), &llm);
    let out = generator.generate_rule(&cluster).await;
    assert_eq!(out.degrade, RuleDegradeReason::TranslationFailed);
    assert!(out.rule.is_candidate());
}

#[tokio::test]
async fn generate_rules_skips_failed_cluster_without_blocking() {
    // 簇 1 翻译两次极性均不一致（积极 vs 消极簇）→ 降级候选；
    // 簇 2 翻译成功 → 两者都返回，不阻塞
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "太好了真棒！", "avoid": []}"#,
        r#"{"reaction": "太开心了！", "avoid": []}"#,
        r#"{"reaction": "辛苦了，别太累，我陪着你。", "avoid": ["深夜"]}"#,
    ]);
    let clusters = vec![
        make_cluster(-0.4, 0.2, 6, 6.0),
        make_cluster(-0.3, 0.1, 6, 6.0),
    ];
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::default(), &llm);
    let out = generator.generate_rules(&clusters).await;
    assert_eq!(out.len(), 2);
    assert!(out[0].rule.is_candidate(), "簇 1 降级候选");
    assert_eq!(llm.call_count(), 3, "簇 1 用尽 2 次 + 簇 2 成功 1 次");
    assert!(out[1].rule.has_reaction(), "簇 2 完整规则");
}

/// 隐私红线：真实生成路径下，规则证据链只含簇内事件 id + 权重，不携带原文文本。
#[tokio::test]
async fn generated_rule_evidence_uses_ids() {
    let llm = MockRuleLlm::new(vec![
        r#"{"reaction": "辛苦了，别太累，我陪着你。", "avoid": []}"#,
    ]);
    let cluster = make_cluster(-0.4, 0.2, 6, 6.0);
    let generator = BehaviorRuleGenerator::new(RuleGenConfig::default(), &llm);
    let out = generator.generate_rule(&cluster).await;
    assert_eq!(out.degrade, RuleDegradeReason::None, "健康簇应生成完整规则");

    // 证据只引用簇内成员事件 id（非 0/占位）
    let evidence = out.rule.evidence;
    assert!(!evidence.is_empty(), "完整规则应带证据链");
    for ev in &evidence {
        assert!(
            cluster.member_event_ids.contains(&ev.event_id),
            "证据 event_id={} 应来自簇内成员事件",
            ev.event_id
        );
    }

    // 证据 JSON 只含 event_id/weight 字段（序列化不携带任何原文文本）
    let json = serde_json::to_string(&evidence).unwrap();
    assert!(!json.contains("原文"), "证据 JSON 不含原文");
}
