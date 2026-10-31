//! crates/ramaria-eval/src/corpus/tests.rs - Ramaria 评测语料模块单元测试
//!
//! 设计特点:
//! - 全部用小夹具构造，不读取真实语料文件
//! - 解析、校验与对齐各自断言，失败文案按定位字段断言
//! - 哈希用例在测试内另行计算摘要，独立于实现
//! - 每个用例独立临时目录，读取后立即清理

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ramaria_core::error::RamariaError;
use ramaria_service::ChatRole;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::verify::check_file;
use super::{
    AlignmentReport, CorpusFileSpec, CorpusLanguage, CorpusSpec, FileCheck, VerifyReport, align,
    lifebench, load,
};

// =========================================================
// 夹具
// =========================================================

/// 独立临时目录：同一进程内并行用例互不干扰。
fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-corpus-{tag}-{nanos}"))
}

/// 删除临时目录；清理失败不掩盖断言结果。
fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

/// 一份最小语料：1 sample、2 session（乱序声明）、3 消息、2 QA（1 普通 + 1 UA）。
fn fixture(tag: &str) -> Value {
    json!([
        {
            "sample_id": format!("{tag} sample"),
            "conversation": {
                "speaker_a": "阿甲",
                "speaker_b": "阿乙",
                "session_2_date_time": "2025-02-03",
                "session_2": [
                    {
                        "speaker": "阿甲",
                        "dia_id": "2025-02-03_note1",
                        "text": format!("{tag} 换了工作")
                    }
                ],
                "session_1_date_time": "2025-01-02",
                "session_1": [
                    {
                        "speaker": "阿甲",
                        "dia_id": "2025-01-02_chat1",
                        "text": format!("{tag} 今天加班")
                    },
                    {
                        "speaker": "阿乙",
                        "dia_id": "2025-01-02_chat1",
                        "text": format!("{tag} 辛苦了")
                    }
                ]
            },
            "qa": [
                {
                    "question_id": "lifebench_v5_yuxiaowei_qa1",
                    "ask_time": "2025-02-10",
                    "question": format!("{tag} 他最近换工作了吗？"),
                    "answer": "换了",
                    "evidence": ["note1"],
                    "category": [0, 2],
                    "question_type": ["Single_hop", "Multi_hop"],
                    "score_points": [
                        {"description": "提到换工作", "score": 7},
                        {"description": "说明时间", "score": 3}
                    ]
                },
                {
                    "question_id": "lifebench_v5_yuxiaowei_qa2",
                    "ask_time": "2025-02-11",
                    "question": "他喜欢什么颜色？",
                    "answer": "无法回答",
                    "evidence": [],
                    "category": [4],
                    "question_type": ["Unanswerable"],
                    "score_points": [
                        {"description": "明确表示不知道", "score": 10}
                    ]
                }
            ]
        }
    ])
}

/// 写出夹具并返回路径。
fn write_fixture(dir: &Path, value: &Value, name: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("临时目录应可创建");
    let path = dir.join(name);
    let bytes = serde_json::to_vec(value).expect("夹具应可序列化");
    std::fs::write(&path, bytes).expect("夹具应可写");
    path
}

/// 加载夹具，失败即 panic（错误信息带定位）。
fn load_fixture(dir: &Path, value: &Value, name: &str) -> Vec<super::Sample> {
    let path = write_fixture(dir, value, name);
    load(&path).expect("夹具应可解析")
}

/// 加载夹具并返回错误（用例期望失败）。
fn load_error(dir: &Path, value: &Value, name: &str) -> RamariaError {
    let path = write_fixture(dir, value, name);
    load(&path).expect_err("夹具应被拒绝")
}

/// 测试内独立计算的 SHA-256 十六进制摘要。
fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

/// 构造单文件注册项，摘要与大小由调用方给定。
fn fixture_spec(size: u64, sha256: String) -> CorpusSpec {
    let files: &'static [CorpusFileSpec] = Box::leak(
        vec![CorpusFileSpec {
            language: CorpusLanguage::Zh,
            file_name: "fixture.json",
            source_path: "locomo_format/fixture.json",
            sha256: Box::leak(sha256.into_boxed_str()),
            size,
            qa_count: 2,
        }]
        .into_boxed_slice(),
    );
    CorpusSpec {
        name: "fixture",
        version: "1.0",
        license: "Apache-2.0",
        source_url: "https://example.test/dataset",
        files,
    }
}

// =========================================================
// 解析
// =========================================================

/// 夹具按序号排序 session，映射角色并保留 artifact 与 QA 字段。
#[test]
fn parse_fixture_sample() {
    let dir = temp_root("parse");
    let samples = load_fixture(&dir, &fixture("zh"), "fixture.json");
    cleanup(&dir);

    assert_eq!(samples.len(), 1, "应解析出 1 个 sample");
    let sample = &samples[0];
    assert_eq!(sample.user_key, "yuxiaowei");
    assert_eq!(sample.persona_uid(), "lifebench-yuxiaowei");
    assert_eq!(sample.sessions.len(), 2, "两个 session 都应在");
    assert_eq!(sample.sessions[0].index, 1, "session 按序号升序");
    assert_eq!(sample.sessions[0].date.to_string(), "2025-01-02");
    assert_eq!(sample.sessions[0].messages.len(), 2);
    assert_eq!(sample.sessions[0].messages[0].role, ChatRole::User);
    assert_eq!(sample.sessions[0].messages[1].role, ChatRole::Assistant);
    assert_eq!(sample.sessions[0].messages[0].artifact_id(), "chat1");
    assert_eq!(sample.sessions[1].index, 2);
    assert_eq!(sample.qa.len(), 2);
    assert_eq!(sample.qa[0].ask_time.to_string(), "2025-02-10");
    assert_eq!(sample.qa[0].category, vec![0, 2]);
    assert_eq!(sample.qa[0].evidence, vec!["note1"]);
    assert_eq!(
        sample.qa[0]
            .score_points
            .iter()
            .map(|point| point.score)
            .sum::<f64>(),
        10.0
    );
    assert!(sample.qa[1].evidence.is_empty(), "UA 题 evidence 为空");
}

/// evidence 指向不存在的 artifact 时拒绝。
#[test]
fn rejects_unknown_evidence() {
    let dir = temp_root("evidence");
    let mut value = fixture("zh");
    value[0]["qa"][0]["evidence"] = json!(["missing"]);
    let error = load_error(&dir, &value, "fixture.json");
    cleanup(&dir);

    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("qa1"),
        "文案应含 question_id，实际 {message}"
    );
    assert!(
        message.contains("missing"),
        "文案应含证据标识，实际 {message}"
    );
}

/// 分值合计不为 10 时拒绝。
#[test]
fn rejects_bad_score_sum() {
    let dir = temp_root("score");
    let mut value = fixture("zh");
    value[0]["qa"][0]["score_points"][0]["score"] = json!(6);
    let error = load_error(&dir, &value, "fixture.json");
    cleanup(&dir);

    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("score_points"),
        "文案应含字段名，实际 {error}"
    );
}

/// 分值为 0.5 步长时按合计 10 接受（真实语料存在 2.5 分项）。
#[test]
fn accepts_half_point_scores() {
    let dir = temp_root("half");
    let mut value = fixture("zh");
    value[0]["qa"][0]["score_points"] = json!([
        {"description": "事件一", "score": 2.5},
        {"description": "事件二", "score": 2.5},
        {"description": "事件三", "score": 2.5},
        {"description": "事件四", "score": 2.5}
    ]);
    let samples = load_fixture(&dir, &value, "fixture.json");
    cleanup(&dir);

    let scores: Vec<f64> = samples[0].qa[0]
        .score_points
        .iter()
        .map(|point| point.score)
        .collect();
    assert_eq!(scores, vec![2.5, 2.5, 2.5, 2.5]);
}

/// 不可回答题带 evidence 时拒绝。
#[test]
fn rejects_ua_with_evidence() {
    let dir = temp_root("ua");
    let mut value = fixture("zh");
    value[0]["qa"][1]["evidence"] = json!(["chat1"]);
    let error = load_error(&dir, &value, "fixture.json");
    cleanup(&dir);

    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("evidence"),
        "文案应含证据字段，实际 {error}"
    );
}

// =========================================================
// 对齐
// =========================================================

/// 中英同题同序可对齐；改一个 question_id 即报错。
#[test]
fn align_fixture_pair() {
    let dir = temp_root("align");
    let zh = load_fixture(&dir, &fixture("zh"), "zh.json");
    let en = load_fixture(&dir, &fixture("en"), "en.json");

    let report = align(&zh, &en).expect("同题同序应对齐");
    assert_eq!(
        report,
        AlignmentReport {
            samples: 1,
            qa_count: 2,
        }
    );

    let mut broken = fixture("en");
    broken[0]["qa"][1]["question_id"] = json!("lifebench_v5_yuxiaowei_qa9");
    let broken_en = load_fixture(&dir, &broken, "en-broken.json");
    let error = align(&zh, &broken_en).expect_err("题号不同应失败");
    cleanup(&dir);

    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    let message = error.to_string();
    assert!(
        message.contains("question_id"),
        "文案应含不一致字段，实际 {message}"
    );
    assert!(
        message.contains("QA #1"),
        "文案应含 QA 定位，实际 {message}"
    );
}

// =========================================================
// 校验
// =========================================================

/// 校验报告写出实际字节与摘要；大小、摘要与缺失各有独立错误。
#[test]
fn verify_file_reports_hash_and_size() {
    let dir = temp_root("verify");
    let value = fixture("zh");
    let path = write_fixture(&dir, &value, "fixture.json");
    let bytes = std::fs::read(&path).expect("夹具应可读");
    let digest = sha256_hex(&bytes);

    let spec = fixture_spec(bytes.len() as u64, digest.clone());
    let check = check_file(&spec, &spec.files[0], &path).expect("哈希与大小匹配应通过");
    assert_eq!(check.actual_size, bytes.len() as u64);
    assert_eq!(check.actual_sha256, digest);
    assert!(check.matched);

    let size_spec = fixture_spec(bytes.len() as u64 + 1, digest.clone());
    let error = check_file(&size_spec, &size_spec.files[0], &path).expect_err("大小不符应失败");
    let message = error.to_string();
    let path_text = path.display().to_string();
    assert!(message.contains("大小不符"), "实际 {message}");
    assert!(message.contains(&path_text), "实际 {message}");
    assert!(message.contains(spec.source_url), "实际 {message}");
    assert!(message.contains(spec.files[0].sha256), "实际 {message}");

    let hash_spec = fixture_spec(bytes.len() as u64, "0".repeat(64));
    let error = check_file(&hash_spec, &hash_spec.files[0], &path).expect_err("摘要不符应失败");
    let message = error.to_string();
    assert!(message.contains("SHA-256 不符"), "实际 {message}");
    assert!(
        message.contains(&digest),
        "文案应含实际摘要，实际 {message}"
    );

    let missing = dir.join("absent.json");
    let error = check_file(&spec, &spec.files[0], &missing).expect_err("缺文件应失败");
    let message = error.to_string();
    assert!(message.contains("缺失"), "实际 {message}");
    assert!(
        message.contains(&missing.display().to_string()),
        "文案应含期望路径，实际 {message}"
    );

    assert!(
        message.contains(spec.source_url),
        "文案应含来源，实际 {message}"
    );
    assert!(
        message.contains(spec.files[0].sha256),
        "文案应含期望摘要，实际 {message}"
    );
    cleanup(&dir);
}

/// 报告渲染列出语言、摘要与对齐状态。
#[test]
fn report_render_lists_files_and_alignment() {
    let check = FileCheck {
        language: CorpusLanguage::Zh,
        source_path: "locomo_format/fixture.json",
        path: PathBuf::from(".eval/corpus/lifebench/2.0/fixture.json"),
        expected_size: 12,
        actual_size: 12,
        expected_sha256: "abc",
        actual_sha256: "abc".to_string(),
        matched: true,
    };
    let mut report = VerifyReport {
        corpus: "fixture",
        version: "1.0",
        license: "Apache-2.0",
        source_url: "https://example.test/dataset",
        files: vec![check],
        alignment: None,
    };
    let rendered = report.render();
    assert!(rendered.contains("[zh]"), "实际 {rendered}");
    assert!(rendered.contains("匹配"), "实际 {rendered}");
    assert!(rendered.contains("未执行"), "实际 {rendered}");

    report.alignment = Some(AlignmentReport {
        samples: 1,
        qa_count: 2,
    });
    let rendered = report.render();
    assert!(rendered.contains("1 samples / 2 QA"), "实际 {rendered}");
}

/// 语言标签大小写不敏感，文件名与注册表一致。
#[test]
fn language_parse_and_file_names() {
    assert_eq!(
        CorpusLanguage::parse("ZH").expect("应可解析"),
        CorpusLanguage::Zh
    );
    assert_eq!(
        CorpusLanguage::parse(" en ").expect("应可解析"),
        CorpusLanguage::En
    );
    assert!(CorpusLanguage::parse("fr").is_err());

    let spec = lifebench();
    for file in spec.files {
        assert_eq!(file.file_name, file.language.file_name());
        assert_eq!(file.sha256.len(), 64, "摘要应为 64 位十六进制");
    }
}
