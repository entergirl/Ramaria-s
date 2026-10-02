//! crates/ramaria-cli/src/commands/probe/tests/dataset.rs - 探针 probe 数据集构建与档位/样例 单元测试
//!
//! 设计特点:
//! - 确定性抽样复现性、内置夹具兜底与档位代表配对
//! - 人格筛选（白名单过滤对方 / 显式优先 / 兜底）
//! - 数据集序列化往返、题项上文与语域向后兼容
//! - 数据源文件解析与档位过滤

use super::super::dataset::tone_pairs_from_messages;
use super::super::evaluate::is_local_backend;
use super::super::evaluate::load_golden_references;
use super::super::evaluate::read_experiment;
use super::super::evaluate::tone_judge_system_prompt;
use super::super::run::STATEMENT_REGISTER_LEAD;
use super::super::run::aggregate_repeat_stats;
use super::super::run::effective_question;
use super::super::run::filter_variants;
use super::super::run::metric_stat;
use super::super::run::seed_history_from_context;
use super::super::run::t_critical_975;
use super::super::types::ContextTurn;
use super::super::types::DATASET_SCHEMA_VERSION;
use super::super::types::ItemRegister;
use super::super::*;
use ramaria_core::error::RamariaError;
use ramaria_core::types::MessageRole;
use ramaria_core::types::PersonaKind;
use std::path::Path;

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
    // rounds 保留该档位每一轮的完整结果明细（逐轮全量 reply）
    assert_eq!(stats[0].rounds.len(), 2, "应保留两轮的完整结果");
    // round1 item chars=10 / round2 item chars=14
    assert_eq!(stats[0].rounds[0].runs[0].metrics.reply_chars, 10);
    assert_eq!(stats[0].rounds[1].runs[0].metrics.reply_chars, 14);
    assert_eq!(stats[0].rounds[0].runs.len(), 2);
    assert_eq!(stats[0].rounds[1].runs.len(), 2);
}

/// 向后兼容：旧 repeat 聚合 JSON 无 `rounds` 字段时反序列化为空，
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
    use super::super::types::{DatasetItem, ProbeDataset};
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

/// 本地 judge 判定（隐私口径）：本地 LM Studio（localhost:1234）
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

/// 语气维 judge 口径：必须显式声明"不按长短判分"，且 few-shot 给出
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
// 消融档位 Profile
// =========================================================
