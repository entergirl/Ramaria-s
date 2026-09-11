//! crates/ramaria-cli/src/commands/probe/tests.rs - probe 命令单元测试
//!
//! 设计特点:
//! - 覆盖数据集构建（build / fixture 兜底 / 文件解析 / 序列化往返）与确定性抽样复现性。
//! - 覆盖档位与消融 Profile（默认代表配对、F0~F4 / S_* / B0~B1 注入闸门映射）。
//! - 覆盖自动评分（fact / tone / emotion 判定，含事实维长度中性多判据）、
//!   --repeat 逐轮聚合与旧格式向后兼容。
//! - 覆盖对比报告统计（Wilcoxon / Cohen's d / BH-FDR / normal-CDF）与消融显著性归因。
//! - 覆盖缺失/非法输入文件统一归为业务校验失败（RamariaError::Validation）。

use super::*;

// 以下为子模块中仅测试使用的内部函数/类型与统一错误类型，
// 迁移后在此显式引入（根文件仅保留运行时 `use`，非测试编译零多余引用）。
use super::dataset::tone_pairs_from_messages;
use super::evaluate::{
    FactItemScore, ItemEvaluation, ProbeEvaluation, VariantEvaluation,
    aggregate_round_dimension_scores, content_bigrams, fact_point_score, is_local_backend,
    keyword_hit_norm_score, load_golden_references, read_experiment, reference_clauses,
    score_emotion_item, tone_judge_system_prompt,
};
use super::report::{
    KnowledgeJudgeRates, KnowledgeQualityScope, bh_fdr_adjust, build_ablation_report,
    cohens_d_paired, cohens_d_pooled, compute_auxiliary_metrics, erf_approx, normal_cdf,
    read_manual_scores, student_t_cdf, tost_equivalence, wilcoxon_signed_rank_p,
};
use super::run::{
    STATEMENT_REGISTER_LEAD, aggregate_repeat_stats, effective_question, filter_variants,
    metric_stat, run_validity, seed_history_from_context, t_critical_975,
};
use super::types::{ContextTurn, DATASET_SCHEMA_VERSION, ItemRegister, VariantOverrides};
use ramaria_core::error::RamariaError;
use ramaria_core::traits::{EmbeddingModelInfo, EmbeddingProvider};
use ramaria_core::types::{MessageRole, PersonaKind};
use std::path::Path;
use std::sync::Arc;

// ---- DeterministicRng ----

#[test]
fn rng_same_seed_same_sequence() {
    let mut a = DeterministicRng::new(42);
    let mut b = DeterministicRng::new(42);
    for _ in 0..100 {
        assert_eq!(a.next_u64(), b.next_u64(), "同 seed 序列必须一致");
    }
}

#[test]
fn rng_different_seed_different_sequence() {
    let mut a = DeterministicRng::new(1);
    let mut b = DeterministicRng::new(2);
    let mut same = 0;
    for _ in 0..10 {
        if a.next_u64() == b.next_u64() {
            same += 1;
        }
    }
    assert!(same <= 1, "不同 seed 的序列应几乎完全不同（同次数={same}）");
}

#[test]
fn rng_shuffle_is_permutation() {
    let mut rng = DeterministicRng::new(7);
    let mut items = vec![1, 2, 3, 4, 5];
    rng.shuffle(&mut items);
    let mut sorted = items.clone();
    sorted.sort_unstable();
    assert_eq!(sorted, vec![1, 2, 3, 4, 5], "洗牌必须是排列（不增不减）");
}

// ---- sample_with_fallback ----

#[test]
fn sample_fallback_uses_fixture_when_no_candidates() {
    let (items, real) = sample_with_fallback::<i32>(&[], &[10, 20, 30], 2, 99);
    assert_eq!(items, vec![10, 20]);
    assert_eq!(real, 0, "无真实候选时 real=0");
}

#[test]
fn sample_fallback_deterministic_same_seed() {
    let cands = vec![1, 2, 3, 4, 5, 6, 7, 8];
    let (a, _) = sample_with_fallback(&cands, &[0], 4, 123);
    let (b, _) = sample_with_fallback(&cands, &[0], 4, 123);
    assert_eq!(a, b, "同 seed 抽样结果必须一致（可复跑）");
    assert_eq!(a.len(), 4);
}

#[test]
fn sample_fallback_pads_with_fixture_when_short() {
    let cands = vec![1, 2];
    let fixture = vec![100, 200, 300];
    let (items, real) = sample_with_fallback(&cands, &fixture, 4, 5);
    assert_eq!(real, 2);
    assert_eq!(items.len(), 4, "不足部分必须用夹具补满");
    // 真实数据在前（洗牌后顺序不定，按集合比较）
    let mut head = items[..2].to_vec();
    head.sort_unstable();
    assert_eq!(head, vec![1, 2], "真实数据应排在前面");
    assert_eq!(&items[2..], &[100, 200], "夹具补齐排在真实数据之后");
}

// ---- 档位 ----

#[test]
fn default_variants_are_representative_pairs() {
    let variants = default_variants();
    assert_eq!(variants.len(), 4);
    // baseline 即对照基准值
    let base = &variants[0];
    assert_eq!(base.id, "baseline");
    assert_eq!(base.theta_gap_minutes, 10);
    assert_eq!(base.max_msgs_per_block, 80);
    assert_eq!(base.retrieve_top_k, 3);
    // 每个档位只动一个参数（相对定稿基准 baseline）
    for v in &variants[1..] {
        let changed = [
            v.theta_gap_minutes != base.theta_gap_minutes,
            v.max_msgs_per_block != base.max_msgs_per_block,
            v.retrieve_top_k != base.retrieve_top_k,
        ]
        .iter()
        .filter(|b| **b)
        .count();
        assert_eq!(changed, 1, "档位 {} 应只变化一个参数", v.id);
    }
}

// ---- read_experiment（实验结果文件缺失/非法 → 业务校验失败）----

#[test]
fn read_experiment_missing_file_is_validation_error() {
    let err = read_experiment(Path::new("/nonexistent/run.json")).expect_err("文件缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(
        matches!(ramaria_err, Some(RamariaError::Validation { .. })),
        "文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
    );
}

#[test]
fn read_experiment_malformed_json_is_validation_error() {
    let path = std::env::temp_dir().join("ramaria_probe_bad_experiment.json");
    std::fs::write(&path, "{ not json").expect("写入临时文件失败");
    let err = read_experiment(&path).expect_err("非法 JSON 必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
    let _ = std::fs::remove_file(&path);
}

// ---- 统计法（--repeat）----

/// metric_stat：单样本退化为该值，stddev=0，CI=该值。
#[test]
fn metric_stat_single_sample_degenerates() {
    let s = metric_stat(&[42.0]);
    assert_eq!(s.n, 1);
    assert_eq!(s.mean, 42.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.ci_low, 42.0);
    assert_eq!(s.ci_high, 42.0);
}

/// metric_stat：空样本 → 全零。
#[test]
fn metric_stat_empty_is_zero() {
    let s = metric_stat(&[]);
    assert_eq!(s.n, 0);
    assert_eq!(s.mean, 0.0);
    assert_eq!(s.stddev, 0.0);
    assert_eq!(s.ci_low, 0.0);
    assert_eq!(s.ci_high, 0.0);
}

/// metric_stat：多样本 → 均值正确、stddev 为样本标准差、CI 对称且随 n 增大收窄。
#[test]
fn metric_stat_multiple_mean_stddev_ci() {
    let samples = [10.0, 12.0, 11.0]; // mean=11
    let s = metric_stat(&samples);
    assert_eq!(s.n, 3);
    assert!((s.mean - 11.0).abs() < 1e-9, "均值应为 11, 实际 {}", s.mean);
    // 样本标准差 = sqrt(((1)^2+(-1)^2+0)/2) = sqrt(1)=1
    assert!(
        (s.stddev - 1.0).abs() < 1e-9,
        "stddev 应为 1, 实际 {}",
        s.stddev
    );
    // t(2,0.975)=4.303, half = 4.303*1/sqrt(3)
    let half = 4.303 / 3.0f64.sqrt();
    assert!((s.ci_low - (11.0 - half)).abs() < 1e-6);
    assert!((s.ci_high - (11.0 + half)).abs() < 1e-6);
    assert!(s.ci_low < s.mean && s.mean < s.ci_high);
}

/// metric_stat：n 增大 → 置信区间收窄（同一分布更稳）。
#[test]
fn metric_stat_more_samples_narrower_ci() {
    let small = metric_stat(&[10.0, 12.0, 11.0, 10.5, 11.2]);
    let bigger = metric_stat(&[
        10.0, 12.0, 11.0, 10.5, 11.2, 10.8, 11.4, 10.9, 11.1, 10.7, 11.3, 10.6, 11.0, 11.2, 10.9,
        11.1, 10.8, 11.0, 10.9, 11.1, 11.0, 11.0, 11.0, 11.0,
    ]);
    let w_small = small.ci_high - small.ci_low;
    let w_bigger = bigger.ci_high - bigger.ci_low;
    assert!(
        w_bigger < w_small,
        "样本量增大后 CI 应收窄, 小 {w_small} vs 大 {w_bigger}"
    );
}

/// t_critical_975：边界值正确且单调递减趋近于 2。
#[test]
fn t_critical_975_table_and_approximation() {
    assert!((t_critical_975(2) - 12.706).abs() < 1e-6);
    assert!((t_critical_975(5) - 2.776).abs() < 1e-6);
    // 超表项 → 近似 2.0
    assert_eq!(t_critical_975(100), 2.0);
    // 单调递减（自由度越高，临界值越小）
    assert!(t_critical_975(3) < t_critical_975(2));
    assert!(t_critical_975(8) < t_critical_975(5));
}

/// aggregate_repeat_stats：按档位+item 配对，缺轮样本以实际计数。
#[test]
fn aggregate_repeat_stats_pairs_by_variant_and_item() {
    // 构造两个 round 的 ProbeExperiment
    fn round(item_chars: &[(usize, usize)]) -> ProbeExperiment {
        let vr = ProbeVariantResult {
            variant_id: "v1".to_string(),
            description: "档位".to_string(),
            params: VariantParams {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
                retrieve_top_k: 3,
                ablation: None,
            },
            runs: item_chars
                .iter()
                .map(|(id, chars)| ProbeRunItem {
                    item_id: format!("fact-{id:04}"),
                    dimension: "fact".to_string(),
                    question: "q".to_string(),
                    reply: String::new(),
                    metrics: ProbeMetrics {
                        reply_chars: *chars,
                        elapsed_ms: 100,
                    },
                    error: None,
                })
                .collect(),
            failed_count: 0,
        };
        ProbeExperiment {
            dataset_file: "d".to_string(),
            dataset_seed: 1,
            persona_uid: "p".to_string(),
            rebuild_utt: true,
            variants: vec![vr],
            repeat: None,
            diagnostics: None,
            generated_at: "t".to_string(),
        }
    }
    let r1 = round(&[(1, 10), (2, 20)]);
    let r2 = round(&[(1, 14), (2, 24)]);
    let stats = aggregate_repeat_stats(&[r1, r2]);
    assert_eq!(stats.len(), 1);
    assert_eq!(stats[0].per_item.len(), 2);
    // item1: chars=[10,14] mean=12
    let it1 = &stats[0].per_item[0];
    assert_eq!(it1.item_id, "fact-0001");
    assert!((it1.reply_chars.mean - 12.0).abs() < 1e-6);
    assert_eq!(it1.reply_chars.n, 2);
    // item2: chars=[20,24] mean=22
    let it2 = &stats[0].per_item[1];
    assert!((it2.reply_chars.mean - 22.0).abs() < 1e-6);
    // 缺口 A：rounds 保留该档位每一轮的完整结果明细（逐轮全量 reply）
    assert_eq!(stats[0].rounds.len(), 2, "应保留两轮的完整结果");
    // round1 item chars=10 / round2 item chars=14
    assert_eq!(stats[0].rounds[0].runs[0].metrics.reply_chars, 10);
    assert_eq!(stats[0].rounds[1].runs[0].metrics.reply_chars, 14);
    assert_eq!(stats[0].rounds[0].runs.len(), 2);
    assert_eq!(stats[0].rounds[1].runs.len(), 2);
}

/// 缺口 A 向后兼容：旧 repeat 聚合 JSON 无 `rounds` 字段时反序列化为空，
/// 序列化时空 `rounds` 被省略（不破坏旧文件读/写与契约）。
#[test]
fn repeat_rounds_serde_roundtrip_and_backcompat() {
    // 新格式：rounds 非空，序列化应保留逐轮明细。
    let with_rounds = VariantRepeatStats {
        variant_id: "v1".to_string(),
        per_item: vec![],
        rounds: vec![ProbeVariantResult {
            variant_id: "v1".to_string(),
            description: "d".to_string(),
            params: VariantParams {
                theta_gap_minutes: 30,
                max_msgs_per_block: 40,
                retrieve_top_k: 3,
                ablation: None,
            },
            runs: vec![],
            failed_count: 0,
        }],
    };
    let roundtrip: VariantRepeatStats =
        serde_json::from_str(&serde_json::to_string(&with_rounds).unwrap()).unwrap();
    assert_eq!(roundtrip.rounds.len(), 1);

    // 旧格式：JSON 无 rounds 字段 → 反序列化 rounds 为空（serde default）。
    let old = r#"{"variant_id":"v1","per_item":[]}"#;
    let parsed: VariantRepeatStats = serde_json::from_str(old).unwrap();
    assert!(parsed.rounds.is_empty());

    // 空的 rounds 序列化时应省略该键（skip_serializing_if），保持与旧文件最小差异。
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("rounds"), "空 rounds 应省略，实际: {s}");
}

// ---- 输入文件缺失统一归业务校验失败（--results / --evaluation / --calibration / --dataset / --source）----

#[test]
fn read_manual_scores_missing_file_is_validation_error() {
    let err =
        read_manual_scores(Path::new("/nonexistent/calib.json")).expect_err("校准文件缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(
        matches!(ramaria_err, Some(RamariaError::Validation { .. })),
        "校准文件缺失应归类为业务校验失败（exit 4），实际: {ramaria_err:?}"
    );
}

#[test]
fn load_golden_references_missing_file_is_validation_error() {
    let err = load_golden_references(Path::new("/nonexistent/dataset.json"))
        .expect_err("数据集缺失必须报错");
    let ramaria_err = err.downcast_ref::<RamariaError>();
    assert!(matches!(ramaria_err, Some(RamariaError::Validation { .. })));
}

/// golden reference 索引应同时收集 fact（事件摘要）与 tone（persona 原回复）
/// 两个维度的参考：tone 参考供语气维 judge 比较"候选回复 vs 原回复"。
#[test]
fn load_golden_references_collects_fact_and_tone() {
    use super::types::{DatasetItem, ProbeDataset};
    let dataset = ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed: 1,
        persona_uid: "char-0001".to_string(),
        dimensions: vec!["tone".to_string(), "fact".to_string()],
        questions_per_dimension: 2,
        source: "db".to_string(),
        generated_at: "t".to_string(),
        variants: vec![],
        items: vec![
            DatasetItem {
                id: "tone-0001".to_string(),
                dimension: "tone".to_string(),
                question: "今天上班好累".to_string(),
                reference: Some("辛苦了，早点休息。工作是做不完的，身体才是自己的。".to_string()),
                source: "db".to_string(),
                source_ref: None,
                context: Vec::new(),
                register: ItemRegister::Chat,
            },
            DatasetItem {
                id: "fact-0001".to_string(),
                dimension: "fact".to_string(),
                question: "还记得「团子」吗？".to_string(),
                reference: Some("去年收养了一只三花猫，取名团子。".to_string()),
                source: "db".to_string(),
                source_ref: Some("养猫".to_string()),
                context: Vec::new(),
                register: ItemRegister::Chat,
            },
            // 空 reference 忽略
            DatasetItem {
                id: "tone-0002".to_string(),
                dimension: "tone".to_string(),
                question: "周末去爬山吗".to_string(),
                reference: None,
                source: "db".to_string(),
                source_ref: None,
                context: Vec::new(),
                register: ItemRegister::Chat,
            },
        ],
    };
    let path =
        std::env::temp_dir().join(format!("ramaria_probe_golden_{}.json", std::process::id()));
    std::fs::write(&path, serde_json::to_string(&dataset).unwrap()).expect("写入数据集失败");
    let map = load_golden_references(&path).expect("数据集应可加载");
    let _ = std::fs::remove_file(&path);

    assert_eq!(map.len(), 2, "应收集 tone + fact 各一条非空 reference");
    assert!(
        map.contains_key("tone-0001"),
        "tone 参考应收集（语气 judge 用）"
    );
    assert!(
        map.contains_key("fact-0001"),
        "fact 参考应收集（事实维 golden）"
    );
}

/// 本地 judge 判定（D-V20-006 隐私口径）：本地 LM Studio（localhost:1234）
/// 与本地 Ollama（localhost:11434）均为可用 judge；线上 DeepSeek/OpenAI 一律拒绝。
#[test]
fn is_local_backend_only_accepts_local_providers() {
    use ramaria_core::types::LlmProvider as P;

    // 本地 LM Studio（provider 非线上 + localhost host）
    assert!(is_local_backend(P::LmStudio, "http://localhost:1234/v1"));
    // 本地 Ollama（OpenAI-compatible，localhost:11434）
    assert!(is_local_backend(P::LmStudio, "http://localhost:11434/v1"));
    assert!(is_local_backend(P::LmStudio, "http://127.0.0.1:11434/v1"));
    assert!(is_local_backend(P::LmStudio, "http://[::1]:1234/v1"));
    // 线上后端一律拒绝（隐私：不调线上 judge）
    assert!(!is_local_backend(
        P::DeepSeek,
        "https://api.deepseek.com/v1"
    ));
    assert!(!is_local_backend(P::OpenAI, "https://api.openai.com/v1"));
    // 本地 provider 但指向远程 host → 拒绝（防误配外泄）
    assert!(!is_local_backend(
        P::LmStudio,
        "https://remote.example.com/v1"
    ));
    assert!(!is_local_backend(P::LmStudio, ""));
}

/// 语气维 judge 口径（M8 复核）：必须显式声明"不按长短判分"，且 few-shot 给出
/// "参考很短、候选同样简短 → 高分"的正锚点与"书面助手腔冗长候选 → 低分"的负锚点，
/// 防止"越长分越高"的长度偏置回退（该偏置实测使长度-分数相关 0.58~0.78）。
#[test]
fn tone_judge_prompt_is_length_neutral() {
    let prompt = tone_judge_system_prompt();
    assert!(
        prompt.contains("不要按回复长短判分"),
        "rubric 必须显式禁止按长短判分"
    );
    assert!(
        prompt.contains("同样简短的候选回复完全可能是 5 分"),
        "rubric 必须说明短回复同样可判高分"
    );
    assert!(
        prompt.contains("参考回复：对啊对啊\n候选回复：对对对\n分数：5"),
        "few-shot 必须含短参考 + 短候选得 5 分的正锚点"
    );
    assert!(
        prompt.contains(
            "候选回复：好的，我这就去数据库里帮您查询相关记录，还请您稍等片刻。\n分数：1"
        ),
        "few-shot 必须含书面助手腔冗长候选得 1 分的负锚点"
    );
}

// ---- fixture ----

#[test]
fn fixture_data_covers_default_scale() {
    assert!(fixture_tone_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
    assert!(fixture_fact_events().len() >= DEFAULT_QUESTIONS_PER_DIM);
    assert!(fixture_emotion_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
    // emotion 夹具的 question 必须命中情感线索（否则不会被收集/评分语义判定）
    for (q, _) in fixture_emotion_pairs() {
        assert!(has_emotion_cue(&q), "emotion 夹具问题应含情感线索: {q}");
    }
}

// ---- select_target_persona（不按发言量，白名单过滤对方）----

/// 构造测试 persona。
fn test_persona(uid: &str, kind: PersonaKind) -> ramaria_core::types::Persona {
    ramaria_core::types::Persona::new(
        uid.to_string(),
        uid.to_string(),
        kind,
        1,
        "test".to_string(),
    )
}

#[test]
fn select_persona_excludes_user_kind() {
    // 我方（kind=user）不得入选探针目标
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("char-0001", PersonaKind::Char),
    ];
    assert_eq!(select_target_persona(&personas, None), "char-0001");
}

#[test]
fn select_persona_first_whitelisted() {
    // 多个对方 persona：取第一个白名单，不引入发言量排序
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("anim-0001", PersonaKind::Anim),
        test_persona("char-0001", PersonaKind::Char),
        test_persona("hist-0001", PersonaKind::Hist),
    ];
    assert_eq!(select_target_persona(&personas, None), "anim-0001");
}

#[test]
fn select_persona_explicit_wins() {
    // 显式 --persona 优先（不校验 kind，尊重用户指定）
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("char-0001", PersonaKind::Char),
    ];
    assert_eq!(
        select_target_persona(&personas, Some("rama-0001")),
        "rama-0001"
    );
}

#[test]
fn select_persona_all_user_role_falls_back() {
    // 全 user-role 退化场景：无白名单 persona → 默认 char-0001（夹具兜底）
    let personas = vec![
        test_persona("user-0001", PersonaKind::User),
        test_persona("user-0002", PersonaKind::User),
    ];
    assert_eq!(select_target_persona(&personas, None), DEFAULT_PERSONA);
}

#[test]
fn select_persona_empty_falls_back() {
    assert_eq!(select_target_persona(&[], None), DEFAULT_PERSONA);
}

// ---- build_from_fixture ----

#[test]
fn build_from_fixture_shape() {
    let ds = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
    assert_eq!(ds.schema_version, DATASET_SCHEMA_VERSION);
    assert_eq!(ds.persona_uid, DEFAULT_PERSONA);
    assert_eq!(ds.source, "fixture");
    assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
    assert_eq!(ds.items.len(), DEFAULT_QUESTIONS_PER_DIM * 3);
    assert_eq!(ds.variants.len(), 4);
    // 全部来自夹具
    assert!(ds.items.iter().all(|i| i.source == "fixture"));
    // 每维恰好 qpd 题
    for dim in ["tone", "fact", "emotion"] {
        assert_eq!(
            ds.items.iter().filter(|i| i.dimension == dim).count(),
            DEFAULT_QUESTIONS_PER_DIM,
            "维度 {dim} 应有 qpd 题"
        );
    }
    // 每题都有 reference 与 id（前缀含 emotion-）
    for item in &ds.items {
        assert!(item.reference.is_some(), "{} 应有参考回答", item.id);
        assert!(
            item.id.starts_with("tone-")
                || item.id.starts_with("fact-")
                || item.id.starts_with("emotion-")
        );
    }
    // seed 固定 → 复跑一致
    let again = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
    let qs: Vec<&str> = ds.items.iter().map(|i| i.question.as_str()).collect();
    let qs2: Vec<&str> = again.items.iter().map(|i| i.question.as_str()).collect();
    assert_eq!(qs, qs2, "同 seed 复跑必须产生相同测试集");
}

// ---- 数据集序列化 roundtrip ----

#[test]
fn dataset_roundtrip_json() {
    let ds = build_from_fixture(DEFAULT_PERSONA, 3, 42);
    let json = serde_json::to_string(&ds).expect("序列化失败");
    let back: ProbeDataset = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(back.items.len(), ds.items.len());
    assert_eq!(back.items[0].question, ds.items[0].question);
    assert_eq!(back.variants.len(), ds.variants.len());
}

// ---- 题项上文（context）----

/// 含 context 的题项序列化 → 反序列化等价（新增字段可落盘/可读回）。
#[test]
fn dataset_item_context_serde_roundtrip() {
    let item = DatasetItem {
        id: "tone-0001".to_string(),
        dimension: "tone".to_string(),
        question: "我去（）".to_string(),
        reference: Some("去哪呀".to_string()),
        source: "db".to_string(),
        source_ref: None,
        context: vec![
            ContextTurn {
                role: "user".to_string(),
                content: "[小明] 在吗".to_string(),
            },
            ContextTurn {
                role: "assistant".to_string(),
                content: "[小九] 在呀".to_string(),
            },
        ],
        register: ItemRegister::Chat,
    };
    let json = serde_json::to_string(&item).expect("序列化失败");
    assert!(
        json.contains("\"context\""),
        "非空 context 应序列化: {json}"
    );
    let back: DatasetItem = serde_json::from_str(&json).expect("反序列化失败");
    assert_eq!(back.question, item.question);
    assert_eq!(back.reference, item.reference);
    assert_eq!(back.context.len(), 2);
    assert_eq!(back.context[0].role, "user");
    assert_eq!(back.context[0].content, "[小明] 在吗");
    assert_eq!(back.context[1].role, "assistant");
    assert_eq!(back.context[1].content, "[小九] 在呀");
}

/// 旧形态题项（无 context 字段）反序列化兼容：context 为空，且空值序列化时省略该键。
#[test]
fn dataset_item_without_context_deserializes_empty() {
    let old = r#"{"id":"tone-0001","dimension":"tone","question":"今天好累",
        "reference":"早点休息","source":"db","source_ref":null}"#;
    let parsed: DatasetItem = serde_json::from_str(old).expect("旧数据集题项应可反序列化");
    assert!(parsed.context.is_empty(), "缺 context 字段应默认为空");
    // 空 context 序列化省略该键，不改变旧数据集文件的 byte 形态
    let s = serde_json::to_string(&parsed).expect("序列化失败");
    assert!(!s.contains("context"), "空 context 应省略: {s}");
}

// ---- 题项体裁（register）与语域题面 ----

/// 题项体裁 serde：`statement` 往返保留；旧 JSON 缺字段 → `Chat`；
/// `Chat` 序列化时省略键（旧数据集反/序列化保持最小差异）。
#[test]
fn dataset_item_register_serde_roundtrip_and_backcompat() {
    // 非缺省体裁 roundtrip：反序列化为 Statement 并原样序列化回。
    let statement = r#"{"id":"fact-0001","dimension":"fact","question":"还记得「团子」吗？",
        "reference":null,"source":"db","source_ref":null,"register":"statement"}"#;
    let parsed: DatasetItem = serde_json::from_str(statement).expect("statement 题项应可反序列化");
    assert_eq!(parsed.register, ItemRegister::Statement);
    let json = serde_json::to_string(&parsed).expect("序列化失败");
    assert!(
        json.contains("\"register\":\"statement\""),
        "非缺省体裁应序列化: {json}"
    );
    let back: DatasetItem = serde_json::from_str(&json).expect("往返失败");
    assert_eq!(back.register, ItemRegister::Statement);

    // 旧 JSON 缺字段 → 缺省 Chat（旧数据集兼容）。
    let old = r#"{"id":"tone-0001","dimension":"tone","question":"今天好累",
        "reference":"早点休息","source":"db","source_ref":null}"#;
    let parsed: DatasetItem = serde_json::from_str(old).expect("旧数据集题项应可反序列化");
    assert_eq!(parsed.register, ItemRegister::Chat, "缺字段应为缺省 Chat");

    // 缺省 Chat 序列化省略键（byte 形态与旧数据集一致）。
    let s = serde_json::to_string(&parsed).expect("序列化失败");
    assert!(!s.contains("register"), "缺省 Chat 应省略: {s}");

    // 显式 chat 与缺省等价。
    let explicit = r#"{"id":"tone-0009","dimension":"tone","question":"q",
        "reference":null,"source":"db","source_ref":null,"register":"chat"}"#;
    let parsed: DatasetItem = serde_json::from_str(explicit).expect("chat 应可反序列化");
    assert_eq!(parsed.register, ItemRegister::Chat);
}

/// 语域题面：`statement` = 原题面 + 换行 + 陈述引导；`chat` = 原题面逐字不变。
#[test]
fn effective_question_appends_statement_lead_only() {
    let base = DatasetItem {
        id: "fact-0001".to_string(),
        dimension: "fact".to_string(),
        question: "还记得「团子」这件事吗？".to_string(),
        reference: Some("去年收养了一只三花猫，取名团子。".to_string()),
        source: "db".to_string(),
        source_ref: Some("养猫".to_string()),
        context: Vec::new(),
        register: ItemRegister::Chat,
    };
    // chat：逐字不变（社交聊天语域，与既有结果可比）。
    assert_eq!(effective_question(&base), base.question);

    // statement：原题面 + 换行 + 引导；题面本体保持在前、逐字不变。
    let statement = DatasetItem {
        register: ItemRegister::Statement,
        ..base
    };
    let effective = effective_question(&statement);
    assert_eq!(
        effective,
        format!("{}\n{}", statement.question, STATEMENT_REGISTER_LEAD)
    );
    assert!(
        effective.starts_with(&statement.question),
        "陈述题面应保留原题面前缀"
    );
    assert!(
        effective.ends_with(STATEMENT_REGISTER_LEAD),
        "引导应追加在题面末尾"
    );
}

/// "零字数表述"口径锁定：陈述引导只做语域切换，不得含任何字数/篇幅表述
/// （防回归：避免模型把引导当作复述长度约束而污染事实维评估）。
#[test]
fn statement_lead_contains_no_length_wording() {
    for banned in ["字", "句", "篇幅", "长度"] {
        assert!(
            !STATEMENT_REGISTER_LEAD.contains(banned),
            "陈述引导不得含字数/篇幅表述「{banned}」: {STATEMENT_REGISTER_LEAD}"
        );
    }
    assert!(
        !STATEMENT_REGISTER_LEAD.trim().is_empty(),
        "陈述引导不得为空（语域切换需实际生效）"
    );
}

/// 数据集上文 → 管线历史：role 字符串映射为 MessageRole，内容保序。
#[test]
fn seed_history_from_context_maps_roles() {
    let context = vec![
        ContextTurn {
            role: "user".to_string(),
            content: "[小明] 在吗".to_string(),
        },
        ContextTurn {
            role: "assistant".to_string(),
            content: "[小九] 在呀".to_string(),
        },
        ContextTurn {
            role: "system".to_string(),
            content: "未知角色".to_string(),
        },
    ];
    let history = seed_history_from_context(&context);
    assert_eq!(history.len(), 3);
    assert_eq!(history[0].role, MessageRole::User);
    assert_eq!(history[1].role, MessageRole::Assistant);
    assert_eq!(
        history[2].role,
        MessageRole::Assistant,
        "非 user 角色一律映射为 assistant"
    );
    let contents: Vec<&str> = history.iter().map(|m| m.content.as_str()).collect();
    assert_eq!(contents, vec!["[小明] 在吗", "[小九] 在呀", "未知角色"]);
    assert!(seed_history_from_context(&[]).is_empty());
}

/// 配对提取的上文取"question 之前紧邻的消息"，不含 question 本身；
/// 已消费的 question 与 persona 回复继续计入窗口供后续题项复用。
#[test]
fn tone_pairs_from_messages_carries_preceding_context() {
    use ramaria_core::types::{Message, MessageSource};
    let sid = uuid::Uuid::new_v4();
    let user = |content: &str| {
        Message::new(
            sid,
            MessageRole::User,
            content.to_string(),
            MessageSource::Local,
        )
    };
    let persona = |content: &str| {
        Message::new(
            sid,
            MessageRole::Assistant,
            content.to_string(),
            MessageSource::Online,
        )
        .with_persona_uid(Some("char-0001".to_string()))
    };

    let messages = vec![
        user("[小明] 在吗"),
        persona("[小九] 在呀"),
        user("[小明] 我去（）"),
        persona("[小九] 去哪呀"),
        user("[小明] 是呀"),
        persona("[小九] 嗯嗯"),
    ];
    let pairs = tone_pairs_from_messages(&messages, "char-0001");
    assert_eq!(pairs.len(), 3, "三对 user → persona 回复");

    // 首题之前无消息 → 上文为空
    assert_eq!(pairs[0].0, "[小明] 在吗");
    assert_eq!(pairs[0].1, "[小九] 在呀");
    assert!(pairs[0].2.is_empty(), "首题不应带上文");

    // 次题上文 = 紧邻前文（时间正序），不含 question 本身
    let ctx2: Vec<&str> = pairs[1].2.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(ctx2, vec!["[小明] 在吗", "[小九] 在呀"]);
    assert_eq!(pairs[1].2[0].role, "user");
    assert_eq!(pairs[1].2[1].role, "assistant");
    assert!(
        !ctx2.contains(&"[小明] 我去（）"),
        "上文不得包含 question 本身（避免与 user_input 重复）"
    );

    // 第三题上文继续累积：前面的 question 与 persona 回复均计入窗口
    let ctx3: Vec<&str> = pairs[2].2.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(
        ctx3,
        vec![
            "[小明] 在吗",
            "[小九] 在呀",
            "[小明] 我去（）",
            "[小九] 去哪呀"
        ]
    );
}

/// 上文窗口容量上限：只保留 question 之前最近的 6 条，更早的弹出。
#[test]
fn tone_pairs_context_window_capped() {
    use ramaria_core::types::{Message, MessageSource};
    let sid = uuid::Uuid::new_v4();
    let user = |content: &str| {
        Message::new(
            sid,
            MessageRole::User,
            content.to_string(),
            MessageSource::Local,
        )
    };
    let persona = |content: &str| {
        Message::new(
            sid,
            MessageRole::Assistant,
            content.to_string(),
            MessageSource::Online,
        )
        .with_persona_uid(Some("char-0001".to_string()))
    };

    let mut messages = Vec::new();
    for i in 1..=5 {
        messages.push(user(&format!("u{i}")));
        messages.push(persona(&format!("p{i}")));
    }
    let pairs = tone_pairs_from_messages(&messages, "char-0001");
    assert_eq!(pairs.len(), 5);

    // 第五题（u5）之前共 8 条消息，窗口只保留最近 6 条（u1/p1 已被弹出）
    let ctx5: Vec<&str> = pairs[4].2.iter().map(|t| t.content.as_str()).collect();
    assert_eq!(ctx5.len(), 6, "上文窗口上限为 6 条");
    assert_eq!(ctx5, vec!["u2", "p2", "u3", "p3", "u4", "p4"]);
}

// ---- 问题模板 ----

#[test]
fn fact_question_template() {
    let (q, _ref, title) = fixture_fact_events()[0].clone();
    assert!(q.contains(&title), "事实记忆问题应包含事件标题");
    assert!(q.contains("还记得"), "事实记忆问题应使用回忆问法");
}

// ---- filter_variants ----

#[test]
fn filter_variants_selects_and_ignores_unknown() {
    let variants = default_variants();
    let filtered = filter_variants(&variants, Some("baseline,top_k_1,nonexistent"));
    assert_eq!(filtered.len(), 2);
    assert_eq!(filtered[0].id, "baseline");
    assert_eq!(filtered[1].id, "top_k_1");
}

#[test]
fn filter_variants_empty_falls_back_to_all() {
    let variants = default_variants();
    let filtered = filter_variants(&variants, Some("bad,bad2"));
    assert_eq!(filtered.len(), 4, "过滤为空时应回退全部档位");
}

// ---- 数据源文件解析 ----

#[test]
fn build_from_file_parses_source_json() {
    let tmp = std::env::temp_dir().join(format!("ramaria_probe_src_{}", std::process::id()));
    let path = tmp.join("source.json");
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        &path,
        r#"{
            "persona_uid": "char-0009",
            "messages": [
                {"question": "今天好累", "reply": "早点休息"},
                {"question": "周末去哪", "reply": "去公园"}
            ],
            "events": [
                {"title": "学钢琴", "summary": "今年开始学钢琴，会弹《致爱丽丝》"}
            ]
        }"#,
    )
    .unwrap();

    let rt = tokio::runtime::Runtime::new().unwrap();
    let ds = rt
        .block_on(build_from_file(&path, DEFAULT_PERSONA, 3, 7))
        .expect("文件构建应成功");
    let _ = std::fs::remove_dir_all(&tmp);

    assert_eq!(ds.persona_uid, "char-0009", "文件中的 persona_uid 应优先");
    assert_eq!(ds.source, "file");
    assert_eq!(ds.items.len(), 9, "3 维 × 3 题");
    // 真实数据在前：tone 2 条 + fact 1 条 + emotion 1 条
    // （"今天好累"含情感线索"累" → 同时进入 emotion 候选；"周末去哪"不含）。
    assert_eq!(ds.items.iter().filter(|i| i.source == "file").count(), 4);
    assert_eq!(ds.items.iter().filter(|i| i.source == "fixture").count(), 5);
    assert_eq!(
        ds.items.iter().filter(|i| i.dimension == "emotion").count(),
        3,
        "emotion 维应补齐 3 题"
    );
    assert_eq!(ds.dimensions, vec!["tone", "fact", "emotion"]);
}

#[test]
fn build_from_file_missing_is_err() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let missing = std::env::temp_dir().join("ramaria_probe_nonexistent.json");
    let result = rt.block_on(build_from_file(&missing, DEFAULT_PERSONA, 3, 7));
    assert!(result.is_err(), "文件不存在应返回 Err（由上层夹具兜底）");
}

// =========================================================
// M5a 消融档位 Profile（D-V17-015 / 技术报告 §16.3）
// =========================================================

/// 全部 15 个名称可解析且往返一致；未知名称返回 None。
#[test]
fn ablation_profile_parse_roundtrip_all_names() {
    assert_eq!(ABLATION_PROFILE_NAMES.len(), 15);
    for name in ABLATION_PROFILE_NAMES {
        let p = AblationProfile::parse_name(name).unwrap_or_else(|| panic!("名称 {name} 应可解析"));
        assert_eq!(p.name(), name, "解析后名称往返一致");
    }
    assert!(AblationProfile::parse_name("unknown").is_none());
    assert!(AblationProfile::parse_name("").is_none());
    assert!(AblationProfile::parse_name("f0").is_none(), "大小写敏感");
}

/// ablation_variants：15 档、id=Profile 名、utt 取定稿基准、ablation 回显。
#[test]
fn ablation_variants_shape_and_baseline_utt() {
    let variants = ablation_variants();
    assert_eq!(variants.len(), 15);
    for v in &variants {
        assert_eq!(
            v.ablation.as_deref(),
            Some(v.id.as_str()),
            "ablation 与 id 一致"
        );
        assert_eq!(v.theta_gap_minutes, 10, "消融档位 utt 取定稿基准");
        assert_eq!(v.max_msgs_per_block, 80);
        assert_eq!(v.retrieve_top_k, 3);
        assert!(v.description.contains("[消融]"));
    }
    // 覆盖 B0/B1/F0/F1~F4/S_* 全集
    let ids: Vec<&str> = variants.iter().map(|v| v.id.as_str()).collect();
    for name in ABLATION_PROFILE_NAMES {
        assert!(ids.contains(&name), "缺少档位 {name}");
    }
}

/// B0 无记忆注入：闸门全关；B1 压缩摘要基座：仅 memory_rag 开。
#[test]
fn ablation_profile_b0_b1_gates() {
    let mut cfg = ramaria_core::config::RamariaConfig::default();
    AblationProfile::B0.apply_to(&mut cfg);
    assert!(!cfg.injection.behavior);
    assert!(!cfg.injection.knowledge);
    assert!(!cfg.injection.speaking_style);
    assert!(!cfg.injection.examples);
    assert!(!cfg.injection.utt);
    assert!(!cfg.injection.narrative);
    assert!(!cfg.injection.bridge);
    assert!(!cfg.injection.memory_rag, "B0 关闭 RAG 相关记忆");

    let mut cfg = ramaria_core::config::RamariaConfig::default();
    AblationProfile::B1.apply_to(&mut cfg);
    assert!(cfg.injection.memory_rag, "B1 保留 RAG 摘要基座");
    assert!(!cfg.injection.behavior);
    assert!(!cfg.injection.knowledge);
    assert!(!cfg.injection.speaking_style);
    assert!(!cfg.injection.examples);
    assert!(!cfg.injection.utt);
    assert!(!cfg.injection.narrative);
    assert!(!cfg.injection.bridge);
}

/// F0 全开（与 None 等同）；F1~F4 在全开基础上只关对应层。
#[test]
fn ablation_profile_f0_to_f4_gates() {
    let mut cfg = ramaria_core::config::RamariaConfig::default();
    cfg.injection = ramaria_core::config::InjectionGate::all_off();
    AblationProfile::F0.apply_to(&mut cfg);
    assert!(cfg.injection.behavior && cfg.injection.memory_rag && cfg.injection.utt);
    assert!(cfg.injection.narrative && cfg.injection.bridge);

    let mut f1 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F1.apply_to(&mut f1);
    assert!(!f1.injection.behavior, "F1 关行为层");
    assert!(f1.injection.knowledge && f1.injection.memory_rag && f1.injection.utt);
    assert!(f1.injection.narrative && f1.injection.bridge && f1.injection.examples);

    let mut f2 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F2.apply_to(&mut f2);
    assert!(!f2.injection.knowledge, "F2 关知识层");
    assert!(f2.injection.behavior && f2.injection.memory_rag);

    let mut f3 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F3.apply_to(&mut f3);
    assert!(!f3.injection.speaking_style, "F3 关表达层（风格）");
    assert!(!f3.injection.examples, "F3 关表达层（示例）");
    assert!(!f3.injection.utt, "F3 关表达层（原文样例）");
    assert!(f3.injection.behavior && f3.injection.knowledge);
    assert!(f3.injection.narrative && f3.injection.bridge && f3.injection.memory_rag);

    let mut f4 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F4.apply_to(&mut f4);
    assert!(!f4.injection.narrative, "F4 关脉络（近期脉络）");
    assert!(!f4.injection.bridge, "F4 关脉络（桥接）");
    assert!(f4.injection.utt && f4.injection.behavior && f4.injection.knowledge);
    assert!(f4.injection.speaking_style && f4.injection.examples && f4.injection.memory_rag);
}

/// S_* 替代对照：去掉 RAG 摘要基座，仅开目标专属层（对照 B1 测单层替代能力）。
#[test]
fn ablation_profile_s_group_gates() {
    let mut sb = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SBehavior.apply_to(&mut sb);
    assert!(sb.injection.behavior, "S_behavior 开行为层");
    assert!(
        !sb.injection.memory_rag,
        "S_behavior 为替代对照：应去掉 RAG 摘要基座"
    );
    assert!(!sb.injection.knowledge && !sb.injection.speaking_style);
    assert!(!sb.injection.examples && !sb.injection.utt);
    assert!(!sb.injection.narrative && !sb.injection.bridge);

    let mut sk = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SKnowledge.apply_to(&mut sk);
    assert!(sk.injection.knowledge);
    assert!(!sk.injection.behavior && !sk.injection.memory_rag);

    let mut se = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SExpression.apply_to(&mut se);
    assert!(!se.injection.memory_rag, "S_expression 去掉 RAG 摘要基座");
    assert!(se.injection.speaking_style && se.injection.examples && se.injection.utt);
    assert!(!se.injection.behavior && !se.injection.knowledge);
    assert!(!se.injection.narrative && !se.injection.bridge);

    let mut sn = ramaria_core::config::RamariaConfig::default();
    AblationProfile::SNarrative.apply_to(&mut sn);
    assert!(!sn.injection.memory_rag, "S_narrative 去掉 RAG 摘要基座");
    assert!(sn.injection.narrative && sn.injection.bridge);
    assert!(!sn.injection.utt && !sn.injection.behavior && !sn.injection.knowledge);
    assert!(!sn.injection.speaking_style && !sn.injection.examples);
}

/// I_* 净增量对照：保留 B1 RAG 基座（memory_rag），仅叠加目标专属层。
#[test]
fn ablation_profile_i_group_gates() {
    let mut ib = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IBehavior.apply_to(&mut ib);
    assert!(ib.injection.memory_rag, "I_behavior 保留 B1 RAG 基座");
    assert!(ib.injection.behavior, "I_behavior 叠加行为层");
    assert!(!ib.injection.knowledge && !ib.injection.speaking_style);
    assert!(!ib.injection.examples && !ib.injection.utt);
    assert!(!ib.injection.narrative && !ib.injection.bridge);

    let mut ik = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IKnowledge.apply_to(&mut ik);
    assert!(ik.injection.memory_rag && ik.injection.knowledge);
    assert!(!ik.injection.behavior);

    let mut ie = ramaria_core::config::RamariaConfig::default();
    AblationProfile::IExpression.apply_to(&mut ie);
    assert!(ie.injection.memory_rag, "I_expression 保留 B1 RAG 基座");
    assert!(ie.injection.speaking_style && ie.injection.examples && ie.injection.utt);
    assert!(!ie.injection.behavior && !ie.injection.knowledge);
    assert!(!ie.injection.narrative && !ie.injection.bridge);

    let mut inn = ramaria_core::config::RamariaConfig::default();
    AblationProfile::INarrative.apply_to(&mut inn);
    assert!(inn.injection.memory_rag, "I_narrative 保留 B1 RAG 基座");
    assert!(inn.injection.narrative && inn.injection.bridge);
    assert!(!inn.injection.utt && !inn.injection.behavior && !inn.injection.knowledge);
    assert!(!inn.injection.speaking_style && !inn.injection.examples);
}

/// ProbeVariant serde 向后兼容：旧数据集（无 ablation 字段）→ None；
/// 带 ablation 的档位 roundtrip 保留该字段。
#[test]
fn probe_variant_ablation_serde_backcompat() {
    // 旧格式：无 ablation 字段 → 反序列化为 None。
    let old = r#"{"id":"baseline","description":"对照基准","theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3}"#;
    let parsed: ProbeVariant = serde_json::from_str(old).unwrap();
    assert!(parsed.ablation.is_none(), "旧数据集 ablation 应为 None");

    // 新格式：ablation 存在则保留。
    let v = ProbeVariant {
        id: "F1".to_string(),
        description: "d".to_string(),
        theta_gap_minutes: 10,
        max_msgs_per_block: 80,
        retrieve_top_k: 3,
        ablation: Some("F1".to_string()),
        overrides: VariantOverrides::default(),
    };
    let json = serde_json::to_string(&v).unwrap();
    assert!(
        json.contains("\"ablation\":\"F1\""),
        "ablation 应序列化: {json}"
    );
    let back: ProbeVariant = serde_json::from_str(&json).unwrap();
    assert_eq!(back.ablation.as_deref(), Some("F1"));

    // ablation=None 序列化时省略该键（保持 M1 旧产物最小差异）。
    let plain = ProbeVariant {
        ablation: None,
        ..v
    };
    let plain_json = serde_json::to_string(&plain).unwrap();
    assert!(
        !plain_json.contains("ablation"),
        "None ablation 应省略: {plain_json}"
    );
}

/// 档位参数覆盖 serde：旧数据集无 `overrides` 键 → 默认全 None（行为等价）；
/// 带覆盖的档位往返保留；空覆盖序列化时省略该键。
#[test]
fn variant_overrides_serde_roundtrip_and_backcompat() {
    // 旧格式（无 overrides）→ 默认空
    let old = r#"{"id":"baseline","description":"d","theta_gap_minutes":10,
        "max_msgs_per_block":80,"retrieve_top_k":3}"#;
    let parsed: ProbeVariant = serde_json::from_str(old).unwrap();
    assert!(parsed.overrides.is_empty(), "旧数据集 overrides 应为空");

    // 新格式：带覆盖 → 往返保留；序列化含 overrides 键
    let v = ProbeVariant {
        id: "p_rag_mem_10".to_string(),
        description: "rag_max_memories=10".to_string(),
        theta_gap_minutes: 10,
        max_msgs_per_block: 80,
        retrieve_top_k: 3,
        ablation: Some("B1".to_string()),
        overrides: VariantOverrides {
            rag_max_memories: Some(10),
            rag_max_summary_chars: None,
            knowledge_retrieve_top_k: None,
            knowledge_retrieve_threshold: Some(0.3),
        },
    };
    let json = serde_json::to_string(&v).unwrap();
    assert!(
        json.contains("\"rag_max_memories\":10"),
        "非空覆盖应序列化: {json}"
    );
    let back: ProbeVariant = serde_json::from_str(&json).unwrap();
    assert_eq!(back.overrides.rag_max_memories, Some(10));
    assert_eq!(back.overrides.knowledge_retrieve_threshold, Some(0.3));
    assert_eq!(back.overrides.rag_max_summary_chars, None);

    // 空覆盖序列化省略该键（旧产物最小差异）
    let plain = ProbeVariant {
        overrides: VariantOverrides::default(),
        ..v
    };
    let plain_json = serde_json::to_string(&plain).unwrap();
    assert!(
        !plain_json.contains("overrides"),
        "空覆盖应省略: {plain_json}"
    );
}

/// F0 档位（ablation="F0"）注入闸门全开——与 ablation=None 行为一致。
#[test]
fn ablation_f0_equivalent_to_none() {
    let mut with_f0 = ramaria_core::config::RamariaConfig::default();
    AblationProfile::F0.apply_to(&mut with_f0);
    let default_cfg = ramaria_core::config::RamariaConfig::default();
    assert!(
        with_f0.injection.behavior == default_cfg.injection.behavior
            && with_f0.injection.memory_rag == default_cfg.injection.memory_rag,
        "F0 闸门应与默认全开一致"
    );
}

// =========================================================
// M5a emotion 第三维（T-V17-5a-002）
// =========================================================

/// 情感线索判定：负面/正面触发词命中；中性消息不命中。
#[test]
fn emotion_cue_detection_cases() {
    assert!(has_emotion_cue("今天被领导骂了，很难过"));
    assert!(has_emotion_cue("我好生气，想投诉"));
    assert!(has_emotion_cue("收到 offer 了，太开心了"));
    assert!(!has_emotion_cue("周末一起去爬山吗"));
    assert!(!has_emotion_cue("请问这个功能怎么用"));
    assert!(has_negative_cue("很担心") && !has_positive_cue("很担心"));
    assert!(!has_negative_cue("太开心了") && has_positive_cue("太开心了"));
}

/// rubric：负面情境 + 充分安慰 → 1.0；1 个标记 → 0.5；无标记 → 0.0。
#[test]
fn emotion_rubric_negative_situation() {
    let q = "今天被领导当众批评，好难过";
    // 充分安慰：命中多个安慰/共情标记
    let full = score_emotion_item("别难过，我理解你，会好的，先深呼吸", q);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_negative && !full.situation_positive);
    // 单标记：部分回应
    let partial = score_emotion_item("别担心，睡一觉就好了", q);
    assert_eq!(partial.score, 0.5);
    assert_eq!(partial.marker_hit, 1);
    // 无标记：冷漠回应
    let cold = score_emotion_item("这个方案本身就有问题，明天重写吧", q);
    assert_eq!(cold.score, 0.0);
    // 空回复
    let empty = score_emotion_item("", q);
    assert_eq!(empty.score, 0.0);
}

/// rubric：正面情境 + 分享喜悦 → 1.0；单标记 → 0.5；无 → 0.0。
#[test]
fn emotion_rubric_positive_situation() {
    let q = "我升职了，太开心了";
    let full = score_emotion_item("太好了，真棒，恭喜你！这是你应得的", q);
    assert_eq!(full.score, 1.0);
    assert!(full.situation_positive && !full.situation_negative);
    let partial = score_emotion_item("嗯，不错", q);
    assert_eq!(partial.score, 0.5);
    let cold = score_emotion_item("下次注意保持", q);
    assert_eq!(cold.score, 0.0);
}

/// 中性情境：两类标记合计弱判定。
#[test]
fn emotion_rubric_neutral_situation() {
    let q = "帮我看看这段代码";
    let score = score_emotion_item("别担心，我帮你看看，一起加油", q);
    assert_eq!(score.score, 1.0, "中性情境按共情标记合计");
    assert!(!score.situation_negative && !score.situation_positive);
}

// =========================================================
// M5a --repeat 逐轮评分聚合（T-V17-5a-003）
// =========================================================

/// 构造一个含单条 fact 题的轮次结果。
fn fact_round(reply: &str) -> ProbeVariantResult {
    ProbeVariantResult {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        runs: vec![ProbeRunItem {
            item_id: "fact-0001".to_string(),
            dimension: "fact".to_string(),
            question: "还记得「团子」吗？".to_string(),
            reply: reply.to_string(),
            metrics: ProbeMetrics {
                reply_chars: reply.chars().count(),
                elapsed_ms: 1,
            },
            error: None,
        }],
        failed_count: 0,
    }
}

/// 空 rounds → 无聚合记录。
#[tokio::test]
async fn aggregate_round_scores_empty_returns_none() {
    let agg = aggregate_round_dimension_scores(&[], &None, None, None, 0).await;
    assert!(agg.is_empty());
}

/// 三轮回复与 golden 完全一致 → fact 三口径轮均分恒 1.0，n=3、std=0、CI 退化。
#[tokio::test]
async fn aggregate_round_scores_pools_round_means() {
    let reference = "用户去年收养了一只猫，取名团子";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> = vec![
        fact_round(reference),
        fact_round(reference),
        fact_round(reference),
    ];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    assert_eq!(agg.len(), 3, "事实维三口径各一条聚合");
    let fact = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert_eq!(fact.n, 3, "有效轮数 = 3");
    assert!((fact.mean - 1.0).abs() < 1e-9, "满分均值应为 1.0");
    assert_eq!(fact.std, 0.0);
    assert!((fact.ci95_low - 1.0).abs() < 1e-9);
    assert!((fact.ci95_high - 1.0).abs() < 1e-9);
}

/// 三轮回复质量不同 → 轮均分存在波动，mean 介于 (0,1)，std > 0，CI 有效。
#[tokio::test]
async fn aggregate_round_scores_captures_variation() {
    let reference = "用户去年收养了一只猫，取名团子";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> = vec![
        fact_round(reference),
        fact_round("不太记得了"),
        fact_round(reference),
    ];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    let fact = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert_eq!(fact.n, 3);
    assert!(fact.mean > 0.0 && fact.mean < 1.0, "波动后均值应介于 0..1");
    assert!(fact.std > 0.0, "质量波动应产生正 std");
    assert!(fact.ci95_low < fact.mean && fact.mean < fact.ci95_high);
}

/// 旧评分数值文件（无 dimension_scores/emotion/fact 新判据字段）反序列化兼容。
#[test]
fn evaluation_variant_serde_backcompat_new_fields() {
    let old = r#"{
        "variant_id":"v1",
        "description":"d",
        "params":{"theta_gap_minutes":10,"max_msgs_per_block":80,"retrieve_top_k":3},
        "fact_score":0.5,"tone_score":null,"failed_count":0,"items":[]
    }"#;
    let parsed: VariantEvaluation = serde_json::from_str(old).unwrap();
    assert!(parsed.dimension_scores.is_none());
    assert!(parsed.emotion_score.is_none());
    assert!(parsed.fact_score_norm.is_none(), "旧产物无长度归一均分");
    assert!(parsed.fact_score_point.is_none(), "旧产物无事实点均分");
    // 空聚合序列化时省略 dimension_scores（保持最小差异）
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("dimension_scores"), "None 聚合应省略: {s}");
    assert!(!s.contains("fact_score_norm"), "None 均分应省略: {s}");
    assert!(!s.contains("fact_score_point"), "None 均分应省略: {s}");
}

// =========================================================
// 事实维多判据（旧 2-gram 覆盖 + 长度归一 + 子句级事实点）
// =========================================================

/// 内容 2-gram 提取：剔除含标点的组合与两侧皆功能字的组合。
#[test]
fn content_bigrams_filters_punct_and_function_pairs() {
    // 任意一侧为标点 → 剔除
    assert!(
        content_bigrams("我，你").is_empty(),
        "含标点的 2-gram 应剔除"
    );
    // 两侧皆为功能字 → 剔除
    assert!(
        content_bigrams("我是").is_empty(),
        "两侧功能字的 2-gram 应剔除"
    );
    // 只有一侧为功能字 → 保留（内容字参与的组合仍计入）
    assert_eq!(content_bigrams("团子是").len(), 2, "内容字参与的组合应保留");
    // 不足 2 字 / 空串 → 无 2-gram
    assert!(content_bigrams("猫").is_empty());
    assert!(content_bigrams("").is_empty());
}

/// 长度归一命中率边界：一致 / 子串 / 无重叠 / 空回复 / 参考过短。
#[test]
fn keyword_hit_norm_score_extremes() {
    let reference = "团子是三花猫";
    assert!(
        (keyword_hit_norm_score(reference, reference) - 1.0).abs() < 1e-9,
        "回复与参考一致应满分"
    );
    assert!(
        (keyword_hit_norm_score("团子", reference) - 1.0).abs() < 1e-9,
        "回复为参考子串时分母取 min(参考, 回复) → 满分"
    );
    assert_eq!(keyword_hit_norm_score("唔唔唔唔", reference), 0.0, "无重叠");
    assert_eq!(keyword_hit_norm_score("", reference), 0.0, "空回复");
    assert_eq!(keyword_hit_norm_score("   ", reference), 0.0, "空白回复");
    assert_eq!(
        keyword_hit_norm_score("猫", "猫"),
        0.0,
        "参考无内容 2-gram（过短）"
    );
}

/// 长度中性：短回复在旧口径（分母固定为参考 2-gram 总数）下被机械压低，新口径不再衰减。
#[test]
fn keyword_hit_norm_is_length_neutral_for_short_reply() {
    let reference = "用户去年收养了一只猫，取名团子";
    let reply = "团子";

    // 复算旧口径（分母 = 参考全部 2-gram，含标点组合；分子 = 回复命中的参考 2-gram 数）。
    let ref_chars: Vec<char> = reference.chars().collect();
    let ref_all: std::collections::HashSet<(char, char)> =
        ref_chars.windows(2).map(|w| (w[0], w[1])).collect();
    let reply_chars: Vec<char> = reply.chars().collect();
    let hit = reply_chars
        .windows(2)
        .filter(|w| ref_all.contains(&(w[0], w[1])))
        .count();
    assert_eq!(ref_all.len(), 14, "旧口径分母 = 参考 2-gram 总数");
    assert_eq!(hit, 1, "旧口径分子 = 回复命中的参考 2-gram 数");
    let old_score = hit as f64 / ref_all.len() as f64;
    assert!(old_score < 0.1, "旧口径下短回复被压到 {old_score}");

    // 新口径：命中数 / min(参考内容 2-gram 数, 回复内容 2-gram 数) = 3 / min(12, 3) → 1.0。
    let norm = keyword_hit_norm_score(reply, reference);
    assert!((norm - 1.0).abs() < 1e-9, "长度归一口径应满分，实际 {norm}");
    assert!(norm > old_score, "短回复在长度归一口径下不被压低");
}

/// 事实点召回：按子句计分，部分体现 / 全覆盖 / 空回复 / 参考无有效子句。
#[test]
fn fact_point_score_counts_hit_clauses() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let one = fact_point_score("他养了猫", reference);
    assert!(
        (one - 1.0 / 3.0).abs() < 1e-9,
        "3 个子句命中 1 个 → 1/3，实际 {one}"
    );
    assert!(
        (fact_point_score(reference, reference) - 1.0).abs() < 1e-9,
        "回复与参考一致应全命中"
    );
    assert_eq!(fact_point_score("", reference), 0.0, "空回复");
    assert_eq!(fact_point_score("他养了猫", "，，"), 0.0, "参考无有效子句");
}

/// 参考子句切分：中英文标点均切分，丢弃 <2 字片段。
#[test]
fn reference_clauses_split_and_drop_short_segments() {
    assert_eq!(reference_clauses("他养了猫，他喜欢狗。他怕蛇").len(), 3);
    assert_eq!(reference_clauses("他养了猫,他喜欢狗.他怕蛇").len(), 3);
    assert_eq!(
        reference_clauses("猫，狗，他养了三花猫"),
        vec!["他养了三花猫".to_string()],
        "单字片段应丢弃"
    );
    assert_eq!(
        reference_clauses("团子是三花猫"),
        vec!["团子是三花猫".to_string()],
        "无标点时整体为单子句"
    );
}

/// 事实维子评分向后兼容：旧产物无新判据字段 → None；新产物可解析到 Some。
#[test]
fn fact_item_score_serde_backcompat_new_criteria() {
    let old = r#"{"cosine":0.7,"keyword_hit":0.3,"score":0.54}"#;
    let parsed: FactItemScore = serde_json::from_str(old).expect("旧事实维子评分应可反序列化");
    assert_eq!(parsed.cosine, Some(0.7));
    assert!(parsed.keyword_hit_norm.is_none(), "旧产物无长度归一判据");
    assert!(parsed.fact_point.is_none(), "旧产物无事实点判据");
    assert!(parsed.score_norm.is_none(), "旧产物无长度归一综合分");
    assert!(parsed.score_point.is_none(), "旧产物无事实点综合分");

    let new = r#"{"cosine":0.7,"keyword_hit":0.3,"score":0.54,
        "keyword_hit_norm":1.0,"fact_point":0.3333333333333333,
        "score_norm":0.82,"score_point":0.5533333333333333}"#;
    let parsed: FactItemScore = serde_json::from_str(new).expect("新产物应可反序列化");
    assert_eq!(parsed.keyword_hit_norm, Some(1.0));
    let point = parsed.fact_point.expect("新产物应含事实点判据");
    assert!((point - 1.0 / 3.0).abs() < 1e-12);
    let norm = parsed.score_norm.expect("新产物应含长度归一综合分");
    assert!((norm - 0.82).abs() < 1e-12);
    let scored = parsed.score_point.expect("新产物应含事实点综合分");
    assert!((scored - 0.5533333333333333).abs() < 1e-12);
}

/// 逐轮聚合纳入两个新事实维判据：无 embedder 降级时与纯函数逐轮均值一致。
#[tokio::test]
async fn aggregate_round_scores_includes_norm_and_point_dimensions() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let reply = "他养了猫";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let rounds: Vec<ProbeVariantResult> =
        vec![fact_round(reply), fact_round(reply), fact_round(reply)];
    let agg = aggregate_round_dimension_scores(&rounds, &None, None, Some(&golden), 0).await;
    let dims: Vec<&str> = agg.iter().map(|d| d.dimension.as_str()).collect();
    assert_eq!(
        dims,
        vec!["fact", "fact_norm", "fact_point"],
        "事实维三口径各一条聚合"
    );

    let norm = agg
        .iter()
        .find(|d| d.dimension == "fact_norm")
        .expect("应有 fact_norm 聚合");
    let point = agg
        .iter()
        .find(|d| d.dimension == "fact_point")
        .expect("应有 fact_point 聚合");
    // 降级（无 embedding）时综合分 = 纯关键词项，故与纯函数值一致
    let expect_norm = keyword_hit_norm_score(reply, reference);
    let expect_point = fact_point_score(reply, reference);
    assert!(
        (norm.mean - expect_norm).abs() < 1e-9,
        "长度归一均值应与纯函数一致：{} vs {expect_norm}",
        norm.mean
    );
    assert!(
        (point.mean - expect_point).abs() < 1e-9,
        "事实点均值应与纯函数一致：{} vs {expect_point}",
        point.mean
    );
    assert!(
        (point.mean - 1.0 / 3.0).abs() < 1e-9,
        "3 个子句命中 1 个 → 1/3，实际 {}",
        point.mean
    );
    assert_eq!(norm.n, 3);
    assert_eq!(point.n, 3);

    // 旧口径（分母 = 参考 2-gram 总数 12，命中 3）冻结为 0.25；
    // 新长度归一口径为 1.0：短回复不再被机械压低。
    let old = agg
        .iter()
        .find(|d| d.dimension == "fact")
        .expect("应有 fact 聚合");
    assert!(
        (old.mean - 0.25).abs() < 1e-9,
        "旧 2-gram 覆盖口径应冻结为 0.25，实际 {}",
        old.mean
    );
    assert!(norm.mean > old.mean, "长度归一口径应高于旧口径");
}

/// 固定向量 mock embedding：按文本查表返回预设向量（未命中返回 `[1.0, 0.0]`）。
struct TableEmbedding {
    table: Vec<(&'static str, Vec<f32>)>,
}

#[async_trait::async_trait]
impl EmbeddingProvider for TableEmbedding {
    async fn embed(&self, text: &str) -> ramaria_core::error::RamariaResult<Vec<f32>> {
        Ok(self
            .table
            .iter()
            .find(|(k, _)| *k == text)
            .map(|(_, v)| v.clone())
            .unwrap_or_else(|| vec![1.0, 0.0]))
    }

    async fn embed_batch(
        &self,
        texts: &[&str],
    ) -> ramaria_core::error::RamariaResult<Vec<Vec<f32>>> {
        let mut out = Vec::with_capacity(texts.len());
        for t in texts {
            out.push(self.embed(t).await?);
        }
        Ok(out)
    }

    fn model_info(&self) -> EmbeddingModelInfo {
        EmbeddingModelInfo {
            model_id: "table-embedding".to_string(),
            dimension: 2,
        }
    }

    async fn validate(&self) -> ramaria_core::error::RamariaResult<()> {
        Ok(())
    }

    async fn download_model(&self) -> ramaria_core::error::RamariaResult<()> {
        Ok(())
    }

    fn download_progress(&self) -> f64 {
        1.0
    }

    fn is_available(&self) -> bool {
        true
    }
}

/// cosine 可用时：新综合分与旧综合分同权重（0.6 / 0.4），仅关键词项不同。
#[tokio::test]
async fn fact_norm_and_point_scores_reuse_old_weights() {
    let reference = "他养了猫，他喜欢狗。他怕蛇";
    let reply = "他养了猫";
    let mut golden = std::collections::HashMap::new();
    golden.insert("fact-0001".to_string(), reference.to_string());
    let embedder: Option<Arc<dyn EmbeddingProvider>> = Some(Arc::new(TableEmbedding {
        table: vec![(reply, vec![1.0, 0.0]), (reference, vec![1.0, 1.0])],
    }));
    let rounds: Vec<ProbeVariantResult> = vec![fact_round(reply), fact_round(reply)];
    let agg = aggregate_round_dimension_scores(&rounds, &embedder, None, Some(&golden), 0).await;

    // 语义余弦 = [1,0] 与 [1,1] 的夹角余弦 = 1/√2
    let cos = 1.0 / 2.0f64.sqrt();
    let kw_norm = keyword_hit_norm_score(reply, reference);
    let point = fact_point_score(reply, reference);
    // 权重与事实维综合分一致：cosine 0.6 + 关键词项 0.4
    let expect_old = 0.6 * cos + 0.4 * 0.25;
    let expect_norm = 0.6 * cos + 0.4 * kw_norm;
    let expect_point = 0.6 * cos + 0.4 * point;

    let get = |name: &str| {
        agg.iter()
            .find(|d| d.dimension == name)
            .unwrap_or_else(|| panic!("应有 {name} 聚合"))
            .mean
    };
    let old = get("fact");
    assert!(
        (old - expect_old).abs() < 1e-9,
        "旧综合分 {old} 应为 {expect_old}"
    );
    let norm = get("fact_norm");
    assert!(
        (norm - expect_norm).abs() < 1e-9,
        "长度归一综合分 {norm} 应为 {expect_norm}"
    );
    let scored = get("fact_point");
    assert!(
        (scored - expect_point).abs() < 1e-9,
        "事实点综合分 {scored} 应为 {expect_point}"
    );
    assert!(norm > old && scored > old, "新口径应高于旧口径（短回复）");
}

// =========================================================
// M5a 消融对比报告统计（T-V17-5a-004）
// =========================================================

/// erf / 正态 CDF 关键值：cdf(0)=0.5，cdf(1.96)≈0.975。
#[test]
fn normal_cdf_key_values() {
    assert!((normal_cdf(0.0) - 0.5).abs() < 1e-9);
    assert!((normal_cdf(1.96) - 0.975).abs() < 0.005);
    assert!((normal_cdf(-1.96) - 0.025).abs() < 0.005);
    assert!((erf_approx(0.0)).abs() < 1e-9);
}

/// Wilcoxon：单向强效应 → p 小；符号混合 → p 大（接近 1 侧）。
#[test]
fn wilcoxon_signed_rank_directionality() {
    // 8 个全正差分（不同绝对值避免全结）→ 秩和显著偏离零
    let diffs: Vec<f64> = (1..=8).map(|i| i as f64 * 0.1).collect();
    let p_strong = wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验");
    assert!(p_strong < 0.05, "单向效应 p 应小，实际 {p_strong}");
    // 正负各半抵消 → p 大
    let mixed = vec![0.2, -0.3, 0.4, -0.5, 0.6, -0.7];
    let p_mixed = wilcoxon_signed_rank_p(&mixed).expect("n≥5 应可检验");
    assert!(p_mixed > 0.1, "符号混合 p 应大，实际 {p_mixed}");
    // 样本过小（n<5）→ None
    assert!(wilcoxon_signed_rank_p(&[0.1, 0.2, 0.3]).is_none());
}

/// Cohen's d：零方差非零均值 → ±10 标记；零均值 → 0。
#[test]
fn cohens_d_edge_cases() {
    assert_eq!(cohens_d_paired(&[1.0, 1.0, 1.0, 1.0]), 10.0);
    assert_eq!(cohens_d_paired(&[-0.5, -0.5]), -10.0);
    assert_eq!(cohens_d_paired(&[1.0, -1.0]), 0.0);
    assert!((cohens_d_paired(&[1.0, 2.0]) - 2.121).abs() < 0.01);
    assert_eq!(cohens_d_paired(&[]), 0.0);
}

/// 学生氏 t 分布 CDF 关键值（与 t 表比对）。
#[test]
fn student_t_cdf_key_values() {
    assert!((student_t_cdf(0.0, 29.0) - 0.5).abs() < 1e-6);
    // 单侧 0.025 分位：t(29, 0.975) = 2.045
    assert!((student_t_cdf(2.045, 29.0) - 0.975).abs() < 5e-4);
    assert!((student_t_cdf(-2.045, 29.0) - 0.025).abs() < 5e-4);
    // 大样本趋近正态
    assert!((student_t_cdf(1.96, 1_000_000.0) - 0.975).abs() < 5e-3);
    // 退化输入不 panic
    assert!((student_t_cdf(f64::NAN, 10.0) - 0.5).abs() < 1e-9);
    assert!((student_t_cdf(1.0, 0.0) - 0.5).abs() < 1e-9);
}

/// TOST：近零效应 → 可判定等效；大效应 → 拒绝等效。
///
/// 关键对照：同一份"近零效应"数据在显著性框架下只能得到"不显著"，
/// 只有 TOST 才能给出"等效（无实质净增量）"结论——这正是该检验的用途。
#[test]
fn tost_declares_equivalence_for_near_zero_effect() {
    // 两档位分数几乎一致：得分本身宽幅分布（合并 SD 大），差分仅为 ±0.01 级微扰，
    // 即"零净增量"的典型形态；等效边界（0.3×合并SD）远大于差分抽样误差。
    let base: Vec<f64> = (0..30).map(|i| 0.2 + (i % 10) as f64 * 0.08).collect();
    let ablated: Vec<f64> = base
        .iter()
        .enumerate()
        .map(|(i, b)| b + if i % 3 == 0 { 0.01 } else { -0.005 })
        .collect();
    let diffs: Vec<f64> = ablated.iter().zip(&base).map(|(a, b)| a - b).collect();
    let t = tost_equivalence(&diffs, &base, &ablated, 0.3).expect("n≥2 应可检验");
    assert!(t.p < 0.05, "近零效应应判定等效，实际 tost_p={}", t.p);
    assert!(t.equivalent);
    assert!(t.bound > 0.0);
    // 显著性框架下同一数据只能得到"不显著"（符号混合）
    assert!(
        wilcoxon_signed_rank_p(&diffs).expect("n≥5 应可检验") > 0.05,
        "该数据不应出现显著差异"
    );
    // 同集合（差分恒 0）→ 合并 SD 口径的 d 为 0
    assert_eq!(cohens_d_pooled(&base, &base), 0.0);

    // 大效应：ablated 系统性高于 base（差值 ≈0.5，远超等效边界）
    let ablated_big: Vec<f64> = base
        .iter()
        .enumerate()
        .map(|(i, b)| b + 0.5 + (i % 4) as f64 * 0.01)
        .collect();
    let diffs_big: Vec<f64> = ablated_big.iter().zip(&base).map(|(a, b)| a - b).collect();
    let t2 = tost_equivalence(&diffs_big, &base, &ablated_big, 0.3).expect("n≥2 应可检验");
    assert!(t2.p > 0.05, "大效应不应判定等效，实际 tost_p={}", t2.p);
    assert!(!t2.equivalent);

    // 样本不足 → None（调用方按不可判定处理）
    assert!(tost_equivalence(&[0.1], &[0.0], &[0.1], 0.3).is_none());
    // 差分为常数（sd=0）→ 无抽样波动，不做 t 检验
    assert!(tost_equivalence(&[0.5; 10], &[0.0; 10], &[0.5; 10], 0.3).is_none());
}

/// BH FDR：单调校正且首尾正确。
#[test]
fn bh_fdr_adjust_monotonic() {
    let p = vec![0.01, 0.04, 0.2];
    let q = bh_fdr_adjust(&p);
    // 预期: [0.03, 0.06, 0.2]
    assert!((q[0] - 0.03).abs() < 1e-12);
    assert!((q[1] - 0.06).abs() < 1e-12);
    assert!((q[2] - 0.2).abs() < 1e-12);
    // 空输入
    assert!(bh_fdr_adjust(&[]).is_empty());
}

/// 构造一个合成评分数值档位（纯 fact 维度，给定逐题分数）。
fn eval_variant_scores(id: &str, scores: &[f64]) -> VariantEvaluation {
    let items = scores
        .iter()
        .enumerate()
        .map(|(i, s)| ItemEvaluation {
            item_id: format!("fact-{:04}", i + 1),
            dimension: "fact".to_string(),
            question: String::new(),
            reference: None,
            reply_preview: String::new(),
            fact: Some(FactItemScore {
                cosine: Some(*s),
                keyword_hit: *s,
                score: *s,
                keyword_hit_norm: Some(*s),
                fact_point: Some(*s),
                score_norm: Some(*s),
                score_point: Some(*s),
            }),
            tone: None,
            emotion: None,
            error: None,
        })
        .collect();
    VariantEvaluation {
        variant_id: id.to_string(),
        description: format!("{id} 档位"),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: Some(id.to_string()),
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: None,
        failed_count: 0,
        items,
    }
}

/// 集成：F0（高分）vs F1（同题低分）→ F1/fact 行显著且方向 down。
#[test]
fn build_ablation_report_marks_removal_effect() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("F0", &[0.9, 0.9, 0.9, 0.9, 0.9]),
            eval_variant_scores("F1", &[0.5, 0.5, 0.5, 0.5, 0.5]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "F0");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "F1" && r.dimension == "fact")
        .expect("应有 F1/fact 行");
    assert_eq!(row.n_pairs, 5);
    assert!(row.significant, "F1 移除行为层后应显著下降");
    assert_eq!(row.direction, "down");
    assert!(row.mean_diff < 0.0);
    assert!(row.p_fdr < 0.05);
    assert!(row.ci95_high < 0.0, "CI 不含 0");

    // 事实维两个重算口径同样纳入对照，供新旧判据分栏核对。
    let dims_of_f1: Vec<&str> = report
        .rows
        .iter()
        .filter(|r| r.ablation_variant == "F1")
        .map(|r| r.dimension.as_str())
        .collect();
    assert!(
        dims_of_f1.contains(&"fact_norm"),
        "F1 应含 fact_norm 行，实际 {dims_of_f1:?}"
    );
    assert!(
        dims_of_f1.contains(&"fact_point"),
        "F1 应含 fact_point 行，实际 {dims_of_f1:?}"
    );
}

/// S 组：B1（低分基座）vs S_behavior（高分单层）→ up 方向，类型=替代对照。
#[test]
fn build_ablation_report_s_group_positive() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
            eval_variant_scores("S_behavior", &[0.8, 0.8, 0.8, 0.8, 0.8]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);
    assert_eq!(report.baseline_variant, "B1");
    let row = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "S_behavior" && r.dimension == "fact")
        .expect("应有 S_behavior/fact 行");
    assert!(row.significant, "S_behavior 单层注入应显著正向");
    assert_eq!(row.direction, "up");
    assert!(row.mean_diff > 0.0);
    assert_eq!(row.comparison_type, "substitution", "S 组应标注为替代对照");
    assert_eq!(row.base_variant, "B1", "S 组基线为 B1");
}

/// I 组（净增量对照）：B1（低分基座）vs I_behavior（B1 基座 + 行为层，高分）
/// → up 方向、comparison_type=increment（与 S 组替代对照可区分）。
#[test]
fn build_ablation_report_i_group_marks_increment() {
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B1", &[0.4, 0.4, 0.4, 0.4, 0.4]),
            eval_variant_scores("I_behavior", &[0.75, 0.75, 0.75, 0.75, 0.75]),
            eval_variant_scores("I_narrative", &[0.3, 0.3, 0.3, 0.3, 0.3]),
        ],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let report = build_ablation_report(&exp, &eval);

    let ib = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "I_behavior" && r.dimension == "fact")
        .expect("应有 I_behavior/fact 行");
    assert_eq!(ib.comparison_type, "increment", "I 组应标注为净增量对照");
    assert_eq!(ib.base_variant, "B1", "I 组基线为 B1");
    assert!(ib.significant, "I_behavior 叠加应显著正向");
    assert_eq!(ib.direction, "up");
    assert!(ib.mean_diff > 0.0, "B1 基座 + 行为层高于 B1 → 净增为正");

    let inn = report
        .rows
        .iter()
        .find(|r| r.ablation_variant == "I_narrative" && r.dimension == "fact")
        .expect("应有 I_narrative/fact 行");
    assert_eq!(inn.comparison_type, "increment");
    assert_eq!(inn.direction, "down", "叠加后低于 B1 → 负向净增");
    assert!(inn.significant);
}

/// 报告局限字段（D-V20-005 必出）：单 persona 局限 + 可用性标注。
#[test]
fn report_limitations_always_contain_external_validity_note() {
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let lim = super::report::build_limitations(&exp, false, true);
    assert!(
        lim.iter().any(|l| l.contains("单 persona")),
        "局限声明必须包含单 persona 外部效度说明"
    );
    assert!(
        lim.iter().any(|l| l.contains("语气维")),
        "judge 不可用时应标注语气维缺失"
    );
    // embedding 可用时不出现降级说明
    assert!(!lim.iter().any(|l| l.contains("embedding 不可用")));
}

// =========================================================
// M2-005 辅助指标四件套（产物可复算近似）
// =========================================================

/// 构造带 fact/emotion 明细与跨轮聚合的评分数值档位（辅助指标测试用）。
fn eval_variant_mixed() -> VariantEvaluation {
    use super::evaluate::{DimensionScoreAgg, EmotionItemScore};
    let item =
        |id: &str, dim: &str, fact: Option<f64>, emo: Option<EmotionItemScore>| -> ItemEvaluation {
            ItemEvaluation {
                item_id: id.to_string(),
                dimension: dim.to_string(),
                question: "q".to_string(),
                reference: None,
                reply_preview: String::new(),
                fact: fact.map(|score| FactItemScore {
                    cosine: Some(score),
                    keyword_hit: score,
                    score,
                    keyword_hit_norm: Some(score),
                    fact_point: Some(score),
                    score_norm: Some(score),
                    score_point: Some(score),
                }),
                tone: None,
                emotion: emo,
                error: None,
            }
        };
    let items = vec![
        // fact 两条：0.9 可追溯 / 0.2 不可追溯 → 可追溯率 0.5
        item("fact-0001", "fact", Some(0.9), None),
        item("fact-0002", "fact", Some(0.2), None),
        // emotion 两条：恰当 1.0（负面情境） / 不当 0.0（正面情境）→ 命中 0.5、误用 0.5
        item(
            "emotion-0001",
            "emotion",
            None,
            Some(EmotionItemScore {
                score: 1.0,
                situation_negative: true,
                situation_positive: false,
                marker_hit: 3,
            }),
        ),
        item(
            "emotion-0002",
            "emotion",
            None,
            Some(EmotionItemScore {
                score: 0.0,
                situation_negative: false,
                situation_positive: true,
                marker_hit: 0,
            }),
        ),
    ];
    VariantEvaluation {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: Some(vec![
            DimensionScoreAgg {
                dimension: "fact".to_string(),
                mean: 0.5,
                std: 0.1,
                ci95_low: 0.4,
                ci95_high: 0.6,
                n: 3,
            },
            DimensionScoreAgg {
                dimension: "emotion".to_string(),
                mean: 0.5,
                std: 0.2,
                ci95_low: 0.3,
                ci95_high: 0.7,
                n: 3,
            },
        ]),
        failed_count: 0,
        items,
    }
}

/// 四件套计算：可追溯率 / 规则命中 / 路由误用 / 画像回归均值可从构造产物复算。
#[test]
fn auxiliary_metrics_recomputable_from_product() {
    let evaluation = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: true,
        generated_at: "t".into(),
        variants: vec![eval_variant_mixed()],
    };
    let m = compute_auxiliary_metrics(&evaluation);

    // 证据链可追溯率：2 条 fact 中 1 条 score≥0.5 → 0.5
    let t = m.evidence_traceability_rate.expect("有 fact 题应可算");
    assert!((t - 0.5).abs() < 1e-9, "可追溯率应 0.5，实际 {t}");
    // 行为规则命中率：2 条 emotion 中 1 条恰当 → 0.5
    let h = m.behavior_rule_hit_rate.expect("有 emotion 题应可算");
    assert!((h - 0.5).abs() < 1e-9, "规则命中率应 0.5，实际 {h}");
    // 情境路由误用率：2 条有极性中 1 条 0 分 → 0.5
    let u = m.situation_route_misuse_rate.expect("有极性样本应可算");
    assert!((u - 0.5).abs() < 1e-9, "路由误用率应 0.5，实际 {u}");
    // 画像回归：档位跨轮 std 均值 = (0.1+0.2)/2 = 0.15
    let p = m
        .profile_regression_output_stability
        .expect("有 dimension_scores 应可算");
    assert!(
        (p - 0.15).abs() < 1e-9,
        "画像回归 std 均值应 0.15，实际 {p}"
    );
    assert!(m.annotation.contains("可复算"), "annotation 应说明口径");
}

/// 画像回归口径固定为 fact/tone/emotion 三维：新增事实维重算维度（fact_norm /
/// fact_point）不计入，故 `dimension_scores` 额外含这两维时数值不变。
#[test]
fn profile_regression_ignores_fact_recalc_dimensions() {
    use super::evaluate::DimensionScoreAgg;

    let agg = |dim: &str, std: f64| DimensionScoreAgg {
        dimension: dim.to_string(),
        mean: 0.5,
        std,
        ci95_low: 0.4,
        ci95_high: 0.6,
        n: 3,
    };
    let variant = |dims: Vec<DimensionScoreAgg>| VariantEvaluation {
        variant_id: "v1".to_string(),
        description: "d".to_string(),
        params: VariantParams {
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        fact_score: None,
        fact_score_norm: None,
        fact_score_point: None,
        tone_score: None,
        emotion_score: None,
        dimension_scores: Some(dims),
        failed_count: 0,
        items: Vec::new(),
    };
    let eval_with = |dims: Vec<DimensionScoreAgg>| ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![variant(dims)],
    };

    let base = eval_with(vec![
        agg("fact", 0.1),
        agg("tone", 0.2),
        agg("emotion", 0.3),
    ]);
    // 额外插入两个重算维度（std 明显不同），验证不参与既有口径的均值。
    let with_recalc = eval_with(vec![
        agg("fact", 0.1),
        agg("fact_norm", 9.0),
        agg("fact_point", 9.0),
        agg("tone", 0.2),
        agg("emotion", 0.3),
    ]);

    let base_std = compute_auxiliary_metrics(&base)
        .profile_regression_output_stability
        .expect("三维应有画像回归值");
    let recalc_std = compute_auxiliary_metrics(&with_recalc)
        .profile_regression_output_stability
        .expect("三维应有画像回归值");
    assert!(
        (base_std - 0.2).abs() < 1e-12,
        "三维 std 均值应为 (0.1+0.2+0.3)/3=0.2，实际 {base_std}"
    );
    assert!(
        (recalc_std - base_std).abs() < 1e-12,
        "新增事实维重算维度不应改变画像回归口径：{recalc_std} vs {base_std}"
    );
}

/// 空评分数值（无 fact/emotion/聚合）→ 各指标 None（标注缺项而非报错）。
#[test]
fn auxiliary_metrics_empty_variants_all_none() {
    let evaluation = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![],
    };
    let m = compute_auxiliary_metrics(&evaluation);
    assert!(m.evidence_traceability_rate.is_none());
    assert!(m.behavior_rule_hit_rate.is_none());
    assert!(m.situation_route_misuse_rate.is_none());
    assert!(m.profile_regression_output_stability.is_none());
    assert!(!m.annotation.is_empty());
}

/// markdown 渲染快照断言（M2-006 验收：I/S 分栏 + 局限字段必出 + 辅助指标节）。
///
/// 构造一个带消融报告（含 I/S 行）与局限/辅助指标的 `ProbeReport`，
/// 断言渲染文本包含三类对照小节、净增量/替代标注与局限声明节。
#[test]
fn render_report_markdown_sections_cover_i_s_columns_and_limitations() {
    use super::report::{
        AblationComparisonRow, AblationReport, AuxiliaryMetrics, KnowledgeQualityReport,
        ProbeReport, Recommendation, VariantAuxMetrics, VariantStyleMetrics,
    };
    // 手工构造最小报告（重点校验渲染分段，不依赖完整评分明细）。
    let report = ProbeReport {
        results_file: "r.json".into(),
        evaluation_file: Some("e.json".into()),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![],
        recommendation: Recommendation {
            per_dimension: vec![],
            overall: "无".into(),
        },
        calibration: None,
        knowledge_quality: Some(KnowledgeQualityReport {
            primary: KnowledgeQualityScope {
                scope: "memory_injected".to_string(),
                description: "主口径：含记忆注入档位".to_string(),
                variant_ids: vec!["B1".to_string()],
                sample_count: 1,
                fact_hit_count: 1,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: true,
                judge_rates: vec![
                    KnowledgeJudgeRates {
                        judge: "legacy".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                    KnowledgeJudgeRates {
                        judge: "norm".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                    KnowledgeJudgeRates {
                        judge: "point".to_string(),
                        sample_count: 1,
                        hit_rate: 1.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: true,
                    },
                ],
            },
            pooled: KnowledgeQualityScope {
                scope: "pooled_all".to_string(),
                description: "对照口径：全部档位池化".to_string(),
                variant_ids: vec!["B1".to_string(), "B0".to_string()],
                sample_count: 2,
                fact_hit_count: 1,
                false_positive_rate: 0.0,
                false_negative_rate: 0.5,
                miss_target_met: false,
                judge_rates: vec![
                    KnowledgeJudgeRates {
                        judge: "legacy".to_string(),
                        sample_count: 2,
                        hit_rate: 0.5,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.5,
                        miss_target_met: false,
                    },
                    KnowledgeJudgeRates {
                        judge: "norm".to_string(),
                        sample_count: 0,
                        hit_rate: 0.0,
                        false_positive_rate: 0.0,
                        false_negative_rate: 0.0,
                        miss_target_met: false,
                    },
                ],
            },
            annotation: "双口径说明".to_string(),
        }),
        ablation: Some(AblationReport {
            baseline_variant: "B1".into(),
            rows: vec![
                AblationComparisonRow {
                    ablation_variant: "F1".into(),
                    description: "移除".into(),
                    comparison_type: "removal".into(),
                    base_variant: "F0".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.8,
                    ablated_mean: 0.4,
                    mean_diff: -0.4,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.88,
                    equiv_bound: 0.09,
                    tost_p: 0.41,
                    equivalent: false,
                    verdict: "significant_down".into(),
                    ci95_low: -0.7,
                    ci95_high: -0.1,
                    significant: true,
                    direction: "down".into(),
                    annotation: "移除显著".into(),
                },
                AblationComparisonRow {
                    ablation_variant: "S_behavior".into(),
                    description: "替代".into(),
                    comparison_type: "substitution".into(),
                    base_variant: "B1".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.4,
                    ablated_mean: 0.8,
                    mean_diff: 0.4,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.88,
                    equiv_bound: 0.09,
                    tost_p: 0.44,
                    equivalent: false,
                    verdict: "significant_up".into(),
                    ci95_low: 0.1,
                    ci95_high: 0.7,
                    significant: true,
                    direction: "up".into(),
                    annotation: "替代对照显著".into(),
                },
                AblationComparisonRow {
                    ablation_variant: "I_behavior".into(),
                    description: "净增量".into(),
                    comparison_type: "increment".into(),
                    base_variant: "B1".into(),
                    dimension: "fact".into(),
                    n_pairs: 5,
                    base_mean: 0.4,
                    ablated_mean: 0.75,
                    mean_diff: 0.35,
                    wilcoxon_p: 0.01,
                    p_fdr: 0.02,
                    cohens_d: 0.9,
                    cohens_d_pooled: 0.86,
                    equiv_bound: 0.08,
                    tost_p: 0.46,
                    equivalent: false,
                    verdict: "significant_up".into(),
                    ci95_low: 0.1,
                    ci95_high: 0.6,
                    significant: true,
                    direction: "up".into(),
                    annotation: "净增量显著".into(),
                },
            ],
            aux: vec![VariantAuxMetrics {
                variant_id: "B1".into(),
                description: "基线".into(),
                reply_chars_mean: 80.0,
                elapsed_ms_mean: 1000.0,
                empty_reply_rate: 0.0,
                success_count: 30,
                total_count: 30,
            }],
            judgment_dimensions: vec!["fact".to_string(), "tone".to_string()],
            descriptive_dimensions: vec!["emotion".to_string()],
            dimension_scope_note: "情感维未校准".to_string(),
            equivalence_note: "等效性检验：TOST（双单侧 t 检验），等效边界取 |d_av|=0.3；\
                tost_p<0.05 判定「等效（无实质净增量）」。"
                .to_string(),
        }),
        limitations: vec![
            "外部效度局限：基于单 persona".into(),
            "统计法样本：单次运行".into(),
        ],
        descriptive_metrics: vec!["情感维口径未校准，为描述性指标".to_string()],
        auxiliary: AuxiliaryMetrics {
            evidence_traceability_rate: Some(0.5),
            behavior_rule_hit_rate: Some(0.5),
            situation_route_misuse_rate: Some(0.5),
            profile_regression_output_stability: Some(0.15),
            annotation: "产物可复算近似".into(),
        },
        style_metrics: vec![VariantStyleMetrics {
            variant_id: "B1".into(),
            description: "基线".into(),
            reply_count: 30,
            len_mean: 23.1,
            len_median: 22.0,
            len_le_30_rate: 0.8,
            len_ref_overlap: Some(0.567),
            tone_particle_rate: 0.878,
            question_rate: 0.122,
            exclaim_rate: 0.0,
            repeat_rate: 0.0,
            assistant_marker_rate: 0.011,
            ref_len_mean: Some(15.9),
        }],
    };
    let md = super::report::render_report_markdown(&report);

    // 三类对照小节标题分栏
    assert!(md.contains("移除对照（F 组 vs F0）"), "应渲染移除对照小节");
    assert!(md.contains("替代对照（S 组 vs B1）"), "应渲染替代对照小节");
    assert!(
        md.contains("净增量对照（I 组 vs B1）"),
        "应渲染净增量对照小节"
    );
    assert!(md.contains("F1"), "移除行应出现");
    assert!(md.contains("S_behavior"), "替代行应出现");
    assert!(md.contains("I_behavior"), "净增量行应出现");
    assert!(md.contains("等效性检验"), "消融小节应渲染等效性口径说明");
    // 局限声明节必出（含两条局限文本）
    assert!(md.contains("数据特性与外部效度局限"), "局限节必出");
    assert!(md.contains("基于单 persona"), "单 persona 局限文本应出现");
    assert!(md.contains("统计法样本"), "repeat 局限文本应出现");
    // 辅助指标四件套节
    assert!(md.contains("辅助指标（产物可复算）"), "辅助指标节必出");
    assert!(md.contains("证据链可追溯率"), "证据链可追溯率项应出现");
    assert!(md.contains("画像回归"), "画像回归项应出现");
    assert!(
        md.contains("描述性指标（不参与层价值判定）"),
        "描述性指标小节必出"
    );
    assert!(
        md.contains("知识层抽取质量评估（双口径）"),
        "知识层双口径小节必出"
    );
    assert!(md.contains("| legacy |"), "知识层判据分栏应含 legacy 行");
    assert!(md.contains("| norm |"), "知识层判据分栏应含 norm 行");
    assert!(md.contains("| point |"), "知识层判据分栏应含 point 行");
    // 客观风格形态指标节
    assert!(
        md.contains("风格形态指标（客观口径，对照语气 judge）"),
        "风格形态指标节必出"
    );
    assert!(md.contains("参考重合"), "长度重合度列应出现");
    assert!(md.contains("助手腔"), "助手腔列应出现");
}

// =========================================================
// 测量口径收口：情感维描述性降级 / 知识层双口径 / run 有效性自检
// =========================================================

/// 情感维口径未校准 → 报告明示为描述性指标，且消融判定维度不含情感维。
#[test]
fn ablation_judgment_excludes_uncalibrated_emotion() {
    use super::evaluate::{EmotionItemScore, ToneItemScore};
    use super::report::{
        AblationReport, AuxiliaryMetrics, KnowledgeQualityReport, ProbeReport, Recommendation,
        VariantAuxMetrics,
    };

    // 评测含 fact / tone / emotion 三维的 F0 与 F1
    fn variant_with_dims(id: &str) -> VariantEvaluation {
        let mk = |dim: &str, idx: usize, score: f64| ItemEvaluation {
            item_id: format!("{dim}-{idx:04}"),
            dimension: dim.to_string(),
            question: String::new(),
            reference: None,
            reply_preview: String::new(),
            fact: (dim == "fact").then_some(FactItemScore {
                cosine: Some(score),
                keyword_hit: score,
                score,
                keyword_hit_norm: Some(score),
                fact_point: Some(score),
                score_norm: Some(score),
                score_point: Some(score),
            }),
            tone: (dim == "tone").then_some(ToneItemScore {
                score: score as u32,
                reason: None,
            }),
            emotion: (dim == "emotion").then_some(EmotionItemScore {
                score,
                situation_negative: true,
                situation_positive: false,
                marker_hit: 0,
            }),
            error: None,
        };
        VariantEvaluation {
            variant_id: id.to_string(),
            description: format!("{id} 档位"),
            params: VariantParams {
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: Some(id.to_string()),
            },
            fact_score: None,
            fact_score_norm: None,
            fact_score_point: None,
            tone_score: None,
            emotion_score: None,
            dimension_scores: None,
            failed_count: 0,
            items: vec![
                mk("fact", 1, 0.9),
                mk("fact", 2, 0.8),
                mk("tone", 1, 5.0),
                mk("tone", 2, 4.0),
                mk("emotion", 1, 1.0),
                mk("emotion", 2, 0.5),
            ],
        }
    }

    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![variant_with_dims("F0"), variant_with_dims("F1")],
    };
    let exp = ProbeExperiment {
        dataset_file: String::new(),
        dataset_seed: 1,
        persona_uid: "char-0001".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: None,
        generated_at: "t".into(),
    };
    let ab = build_ablation_report(&exp, &eval);
    assert_eq!(
        ab.judgment_dimensions,
        vec![
            "fact".to_string(),
            "fact_norm".to_string(),
            "fact_point".to_string(),
            "tone".to_string()
        ],
        "判定维度为事实维三口径（fact / fact_norm / fact_point）+ 语气维"
    );
    assert_eq!(ab.descriptive_dimensions, vec!["emotion".to_string()]);
    assert!(
        ab.rows.iter().all(|r| r.dimension != "emotion"),
        "情感维不得出现在层价值判定行中"
    );
    assert!(ab.rows.iter().any(|r| r.dimension == "fact"));
    // 两个事实维重算口径也成行（与旧 fact 口径并列，供新旧判据核对）。
    assert!(ab.rows.iter().any(|r| r.dimension == "fact_norm"));
    assert!(ab.rows.iter().any(|r| r.dimension == "fact_point"));
    assert!(ab.dimension_scope_note.contains("未校准"));

    // 渲染：描述性小节必出（用最小报告）
    let report = ProbeReport {
        results_file: "r.json".into(),
        evaluation_file: None,
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![],
        recommendation: Recommendation {
            per_dimension: vec![],
            overall: "无".into(),
        },
        calibration: None,
        knowledge_quality: Some(KnowledgeQualityReport {
            primary: KnowledgeQualityScope {
                scope: "memory_injected".to_string(),
                description: "主口径".to_string(),
                variant_ids: vec![],
                sample_count: 0,
                fact_hit_count: 0,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: false,
                judge_rates: vec![],
            },
            pooled: KnowledgeQualityScope {
                scope: "pooled_all".to_string(),
                description: "对照口径".to_string(),
                variant_ids: vec![],
                sample_count: 0,
                fact_hit_count: 0,
                false_positive_rate: 0.0,
                false_negative_rate: 0.0,
                miss_target_met: false,
                judge_rates: vec![],
            },
            annotation: "双口径".to_string(),
        }),
        ablation: Some(AblationReport {
            baseline_variant: "F0".into(),
            rows: vec![],
            aux: Vec::<VariantAuxMetrics>::new(),
            judgment_dimensions: ab.judgment_dimensions.clone(),
            descriptive_dimensions: ab.descriptive_dimensions.clone(),
            dimension_scope_note: ab.dimension_scope_note.clone(),
            equivalence_note: ab.equivalence_note.clone(),
        }),
        limitations: vec!["单 persona".into()],
        descriptive_metrics: vec![super::report::EMOTION_DESCRIPTIVE_NOTE.to_string()],
        auxiliary: AuxiliaryMetrics {
            evidence_traceability_rate: None,
            behavior_rule_hit_rate: None,
            situation_route_misuse_rate: None,
            profile_regression_output_stability: None,
            annotation: "无".into(),
        },
        style_metrics: vec![],
    };
    let md = super::report::render_report_markdown(&report);
    assert!(md.contains("描述性指标（不参与层价值判定）"));
    assert!(md.contains("口径未校准"));
    assert!(md.contains("判定维度：fact / fact_norm / fact_point / tone"));
}

/// 知识层质量双口径：主口径只统计含记忆注入档位（B1/F0/I_*），
/// 对照口径池化全部档位（含无记忆基线 B0/S_*）。
#[test]
fn knowledge_quality_splits_memory_and_pooled_scopes() {
    // 4 个档位、每档 2 条 fact 题：B0（低）/ B1（高）/ S_behavior（低）/ I_behavior（高）。
    // `eval_variant_scores` 已把 `params.ablation` 设为档位 id，口径判定按该名解析。
    let eval = ProbeEvaluation {
        results_file: String::new(),
        persona_uid: "char-0001".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: false,
        generated_at: "t".into(),
        variants: vec![
            eval_variant_scores("B0", &[0.1, 0.1]),
            eval_variant_scores("B1", &[0.9, 0.9]),
            eval_variant_scores("S_behavior", &[0.2, 0.2]),
            eval_variant_scores("I_behavior", &[0.8, 0.8]),
        ],
    };
    let kq = super::report::assess_knowledge_quality(&eval);
    // 主口径：仅 B1 + I_behavior（各 2 题，全部 ≥0.5 命中）
    assert_eq!(kq.primary.scope, "memory_injected");
    assert_eq!(kq.primary.sample_count, 4);
    assert_eq!(kq.primary.fact_hit_count, 4);
    assert!((kq.primary.false_negative_rate - 0.0).abs() < 1e-9);
    assert!(kq.primary.miss_target_met);
    let mut ids = kq.primary.variant_ids.clone();
    ids.sort();
    assert_eq!(ids, vec!["B1".to_string(), "I_behavior".to_string()]);
    // 对照口径：全部 4 档 8 题
    assert_eq!(kq.pooled.scope, "pooled_all");
    assert_eq!(kq.pooled.sample_count, 8);
    assert_eq!(kq.pooled.fact_hit_count, 4);
    assert!((kq.pooled.false_negative_rate - 0.5).abs() < 1e-9);
    assert_eq!(kq.pooled.variant_ids.len(), 4);
    // 口径说明同时提到两者
    assert!(kq.annotation.contains("含记忆注入"));
    assert!(kq.annotation.contains("全部档位池化"));
    // 判据分栏：每口径恒有 legacy/norm/point 三行，legacy 行与扁平字段一致
    let judges: Vec<&str> = kq
        .primary
        .judge_rates
        .iter()
        .map(|r| r.judge.as_str())
        .collect();
    assert_eq!(judges, vec!["legacy", "norm", "point"]);
    let legacy = &kq.primary.judge_rates[0];
    assert_eq!(legacy.sample_count, kq.primary.sample_count);
    assert!((legacy.false_negative_rate - kq.primary.false_negative_rate).abs() < 1e-9);
    assert!((legacy.hit_rate - 1.0).abs() < 1e-9);
    assert!(legacy.miss_target_met);
    // 合成题三口径同分 → 三行数值一致
    for r in &kq.primary.judge_rates {
        assert_eq!(r.sample_count, 4);
        assert!((r.hit_rate - 1.0).abs() < 1e-9, "{r:?}");
    }
    // 对照口径（含 0.2 低分档）legacy 行与扁平字段一致
    let pooled_legacy = &kq.pooled.judge_rates[0];
    assert_eq!(pooled_legacy.sample_count, 8);
    assert!((pooled_legacy.false_negative_rate - 0.5).abs() < 1e-9);
    assert!((pooled_legacy.hit_rate - 0.5).abs() < 1e-9);
    assert!(!pooled_legacy.miss_target_met);
    // 判据附注：主口径逐判据漏报（三口径同分 → 均标为达标）
    assert!(kq.annotation.contains("legacy 漏报"));
    assert!(kq.annotation.contains("norm 漏报"));
    assert!(kq.annotation.contains("point 漏报"));
}

/// 旧评分数值（缺 score_norm/score_point 字段）→ norm/point 口径样本为 0、不达标，
/// legacy 行仍从扁平字段回填，渲染不 panic。
#[test]
fn knowledge_judge_rates_backcompat_without_new_judges() {
    let mut variant = eval_variant_scores("B1", &[0.8, 0.2]);
    for item in &mut variant.items {
        if let Some(f) = item.fact.as_mut() {
            f.score_norm = None;
            f.score_point = None;
        }
    }
    let eval = ProbeEvaluation {
        results_file: "r".into(),
        persona_uid: "u".into(),
        dataset_seed: 1,
        judge_used: false,
        embedding_used: true,
        generated_at: "t".into(),
        variants: vec![variant],
    };
    let kq = super::report::assess_knowledge_quality(&eval);
    // legacy：2 题（0.8 命中、0.2 漏报）→ 命中率 50% / 漏报率 50%
    let legacy = &kq.primary.judge_rates[0];
    assert_eq!(legacy.sample_count, 2);
    assert!((legacy.hit_rate - 0.5).abs() < 1e-9);
    assert!((legacy.false_negative_rate - 0.5).abs() < 1e-9);
    assert!(!legacy.miss_target_met);
    // norm/point：字段缺失 → 样本 0、率 0、不达标
    for r in &kq.primary.judge_rates[1..] {
        assert_eq!(r.sample_count, 0, "{r:?}");
        assert_eq!(r.hit_rate, 0.0);
        assert_eq!(r.false_negative_rate, 0.0);
        assert!(!r.miss_target_met);
    }
    // 扁平字段仍取 legacy
    assert_eq!(kq.primary.sample_count, 2);
    assert_eq!(kq.primary.fact_hit_count, 1);
}

/// 检索器空载 → 本轮无效且告警；文档数 > 0 且通道有命中 → 有效无告警。
#[test]
fn run_validity_flags_empty_retriever() {
    let (valid, warnings) = run_validity(0, 0, true);
    assert!(!valid, "检索器文档数为 0 应判定无效");
    assert!(warnings.iter().any(|w| w.contains("检索器文档数为 0")));

    let (valid, warnings) = run_validity(129, 12, true);
    assert!(valid);
    assert!(warnings.is_empty(), "正常轮次不应有告警: {warnings:?}");

    // 文档数 > 0 但通道全空 → 有效但告警
    let (valid, warnings) = run_validity(129, 0, true);
    assert!(valid);
    assert!(warnings.iter().any(|w| w.contains("四通道命中均为 0")));

    // embedding 不可用 → 追加告警
    let (_valid, warnings) = run_validity(129, 12, false);
    assert!(warnings.iter().any(|w| w.contains("embedding 不可用")));
}

/// ProbeExperiment.diagnostics 向后兼容：旧产物无该字段 → None；新产物 roundtrip 保留。
#[test]
fn probe_experiment_diagnostics_serde_backcompat() {
    let old = r#"{"dataset_file":"d","dataset_seed":1,"persona_uid":"p","rebuild_utt":false,
        "variants":[],"generated_at":"t"}"#;
    let parsed: ProbeExperiment = serde_json::from_str(old).expect("旧产物应可反序列化");
    assert!(parsed.diagnostics.is_none());
    // None 时序列化省略该键
    let s = serde_json::to_string(&parsed).unwrap();
    assert!(!s.contains("diagnostics"), "None diagnostics 应省略: {s}");

    let with = ProbeExperiment {
        dataset_file: "d".into(),
        dataset_seed: 1,
        persona_uid: "p".into(),
        rebuild_utt: false,
        variants: vec![],
        repeat: None,
        diagnostics: Some(ProbeRunDiagnostics {
            retriever_doc_count: 129,
            utt_doc_count: 167,
            keyword_doc_count: 125,
            keyword_pool_len: 125,
            embeddings_available: true,
            probe_queries: 5,
            bm25_hits: 20,
            vector_hits: 15,
            graph_hits: 0,
            keyword_hits: 8,
            fused_hits: 25,
            valid: true,
            warnings: vec![],
        }),
        generated_at: "t".into(),
    };
    let roundtrip: ProbeExperiment =
        serde_json::from_str(&serde_json::to_string(&with).unwrap()).unwrap();
    let d = roundtrip.diagnostics.expect("diagnostics 应保留");
    assert_eq!(d.retriever_doc_count, 129);
    assert!(d.valid);
}

// =========================================================
// 风格形态指标（客观口径，对照短回复下区分力不足的语气 judge）
// =========================================================

/// 客观风格形态指标：长度形态 / 参考长度分布重合 / 语气词 / 疑问感叹 / 复读 / 助手腔，
/// 且失败题与空回复不进样本；无评分数值（无 persona 参考）时重合度与参考均长为 None。
#[test]
fn style_metrics_compute_covers_length_and_marks() {
    use super::report::compute_style_metrics;

    // 1 档 5 题：4 条有效回复（含 1 条重复）+ 1 条失败（不计入）
    let experiment: ProbeExperiment = serde_json::from_str(
        r#"{
          "dataset_file": "ds.json",
          "dataset_seed": 1,
          "persona_uid": "char-0001",
          "rebuild_utt": false,
          "variants": [{
            "variant_id": "B1",
            "description": "基线",
            "params": {"theta_gap_minutes": 30, "max_msgs_per_block": 5, "retrieve_top_k": 5},
            "failed_count": 1,
            "runs": [
              {"item_id":"tone-0001","dimension":"tone","question":"q","reply":"哦哦",
               "metrics":{"reply_chars":2,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0002","dimension":"tone","question":"q","reply":"哦哦",
               "metrics":{"reply_chars":2,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0003","dimension":"tone","question":"q","reply":"我找一下，你先看看？",
               "metrics":{"reply_chars":11,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0004","dimension":"tone","question":"q","reply":"总的来说，我建议你这样做。",
               "metrics":{"reply_chars":13,"elapsed_ms":1},"error":null},
              {"item_id":"tone-0005","dimension":"tone","question":"q","reply":"",
               "metrics":{"reply_chars":0,"elapsed_ms":1},"error":"失败"}
            ]
          }],
          "generated_at": "t"
        }"#,
    )
    .expect("实验产物反序列化");

    // persona 参考（tone 题 reference）长度 [2, 4] → 均长 3.0
    let evaluation: ProbeEvaluation = serde_json::from_str(
        r#"{
          "results_file": "r.json",
          "persona_uid": "char-0001",
          "dataset_seed": 1,
          "judge_used": false,
          "embedding_used": false,
          "generated_at": "t",
          "variants": [{
            "variant_id": "B1",
            "description": "基线",
            "params": {"theta_gap_minutes": 30, "max_msgs_per_block": 5, "retrieve_top_k": 5},
            "fact_score": null,
            "tone_score": null,
            "failed_count": 1,
            "items": [
              {"item_id":"tone-0001","dimension":"tone","question":"q","reference":"哦哦",
               "reply_preview":"哦哦","fact":null,"tone":null,"emotion":null,"error":null},
              {"item_id":"tone-0002","dimension":"tone","question":"q","reference":"我找一下",
               "reply_preview":"哦哦","fact":null,"tone":null,"emotion":null,"error":null}
            ]
          }]
        }"#,
    )
    .expect("评分产物反序列化");

    let metrics = compute_style_metrics(&experiment, Some(&evaluation));
    assert_eq!(metrics.len(), 1);
    let m = &metrics[0];
    assert_eq!(m.variant_id, "B1");
    assert_eq!(m.reply_count, 4, "失败题与空回复不应计入");
    // 长度 [2, 2, 10, 13] → 均长 6.75 / 中位 (2+10)/2 = 6.0
    assert!((m.len_mean - 6.75).abs() < 1e-9, "{m:?}");
    assert!((m.len_median - 6.0).abs() < 1e-9);
    assert!((m.len_le_30_rate - 1.0).abs() < 1e-9);
    // "哦哦" 出现 2 次 → 复读率 0.25
    assert!((m.repeat_rate - 0.25).abs() < 1e-9);
    // 语气词：仅 2 条 "哦哦" 命中（"我找一下，你先看看？" 不含语气词字符）
    assert!((m.tone_particle_rate - 0.5).abs() < 1e-9);
    assert!(
        (m.question_rate - 0.25).abs() < 1e-9,
        "仅 1 条以 ? / ？ 结尾"
    );
    assert_eq!(m.exclaim_rate, 0.0);
    assert!(
        (m.assistant_marker_rate - 0.25).abs() < 1e-9,
        "含「总的来说」1 条"
    );
    // 长度直方图：回复 {0..4: 0.5, 10..14: 0.5} vs 参考 {0..4: 1.0} → 重合 0.5
    assert!((m.ref_len_mean.expect("参考均长") - 3.0).abs() < 1e-9);
    assert!((m.len_ref_overlap.expect("长度重合度") - 0.5).abs() < 1e-9);

    // 无评分数值（无 persona 参考）→ 长度重合度与参考均长不可得，其余指标仍产出
    let no_ref = compute_style_metrics(&experiment, None);
    assert_eq!(no_ref[0].reply_count, 4);
    assert!(no_ref[0].len_ref_overlap.is_none());
    assert!(no_ref[0].ref_len_mean.is_none());
    assert!((no_ref[0].len_mean - 6.75).abs() < 1e-9);
}
