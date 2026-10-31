//! crates/ramaria-eval/src/baseline/manifest/tests.rs - Ramaria 评测基线清单测试
//!
//! 设计特点:
//! - serde 往返与落盘往返各断言一次，字段命名变化会被用例挡住
//! - 顶层键集合固定断言，防止悄悄加入路径类字段
//! - 代码指纹断言稳定、长度与对输入敏感
//! - 每个用例独立临时目录，结束即删

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// =========================================================
// 夹具
// =========================================================

/// 构造一份字段齐全的清单。
fn fixture() -> BaselineManifest {
    BaselineManifest {
        artifact_kind: ARTIFACT_KIND.to_string(),
        schema_version: ARTIFACT_SCHEMA_VERSION,
        baseline_id: "lifebench-2.0-zh-u1-y1".to_string(),
        created_at: "2026-10-29T08:00:00Z".to_string(),
        corpus: ManifestCorpus {
            name: "lifebench".to_string(),
            version: "2.0".to_string(),
            language: "zh".to_string(),
            file_name: "lifebench.json".to_string(),
            sha256: "67eb".to_string(),
            size: 27_088_496,
            qa_total: 3380,
            sample_ids: vec!["于晓薇".to_string()],
        },
        tier: ManifestTier {
            id: "u1-y1".to_string(),
            users: 1,
            memory_start: "2025-01-01".to_string(),
            memory_end: "2025-12-31".to_string(),
            query_end: "2026-01-15".to_string(),
            span_days: 365,
            sample_ids: vec!["于晓薇".to_string()],
        },
        qa: ManifestQa {
            total: 340,
            in_window: 338,
        },
        pipeline: ManifestPipeline {
            crate_version: "3.0.0".to_string(),
            schema_version: 4,
            prompt_template_version: "20261017-v2.9.2".to_string(),
            pipeline_version: "m7-v1".to_string(),
            code_fingerprint: code_fingerprint("3.0.0", 4, "20261017-v2.9.2", "m7-v1"),
        },
        memory_model: ManifestMemoryModel {
            kind: "local".to_string(),
            model: "google/gemma-4-e4b".to_string(),
            temperature: 0.3,
            max_tokens: 2048,
        },
        embedding: ManifestEmbedding {
            fingerprint: "Qwen3-Embedding-0.6B".to_string(),
            dimension: Some(1024),
        },
        index: ManifestIndex {
            status: "ready".to_string(),
            loaded: 1234,
            total: 1234,
        },
        counts: Counts {
            personas: 1,
            sessions: 365,
            messages: 7097,
            l1: 400,
            l2: 73,
            l3: 12,
            unabsorbed_l1: 0,
        },
        timing: ManifestTiming {
            build_elapsed_ms: 1234,
        },
        warnings: Vec::new(),
    }
}

/// 临时目录路径。
fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-manifest-{tag}-{nanos}"))
}

// =========================================================
// 序列化与落盘
// =========================================================

/// serde 往返保持全部字段。
#[test]
fn manifest_round_trips_through_json() {
    let manifest = fixture();
    let text = serde_json::to_string_pretty(&manifest).expect("序列化应成功");
    let parsed: BaselineManifest = serde_json::from_str(&text).expect("反序列化应成功");
    assert_eq!(parsed, manifest);
}

/// 顶层键集合固定，不夹带路径类字段。
#[test]
fn manifest_keys_are_stable_and_path_free() {
    let manifest = fixture();
    let text = serde_json::to_string(&manifest).expect("序列化应成功");
    let value: serde_json::Value = serde_json::from_str(&text).expect("反序列化应成功");
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("清单应为对象")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        vec![
            "artifact_kind",
            "baseline_id",
            "corpus",
            "counts",
            "created_at",
            "embedding",
            "index",
            "memory_model",
            "pipeline",
            "qa",
            "schema_version",
            "tier",
            "timing",
            "warnings",
        ]
    );
    assert!(!text.contains("C:"), "不得写盘符路径");
    assert!(!text.contains('\\'), "不得写 Windows 路径分隔符");
    assert!(!text.contains("/home/"), "不得写 POSIX 绝对路径");
}

/// 落盘后不留临时文件，内容可读回。
#[test]
fn write_replaces_atomically() {
    let dir = temp_dir("write");
    std::fs::create_dir_all(&dir).expect("临时目录应可创建");
    let manifest = fixture();

    write(&dir, &manifest).expect("写出应成功");
    let text = std::fs::read_to_string(dir.join(MANIFEST_FILE)).expect("清单应可读");
    let parsed: BaselineManifest = serde_json::from_str(&text).expect("清单应可解析");
    assert_eq!(parsed, manifest);
    assert!(
        !dir.join(format!("{MANIFEST_FILE}.tmp")).exists(),
        "不应残留临时文件"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 代码指纹
// =========================================================

/// 同一组版本口径得到同一指纹，长度固定为 64 位十六进制。
#[test]
fn code_fingerprint_is_stable() {
    let first = code_fingerprint("3.0.0", 4, "20261017-v2.9.2", "m7-v1");
    let second = code_fingerprint("3.0.0", 4, "20261017-v2.9.2", "m7-v1");
    assert_eq!(first, second);
    assert_eq!(first.len(), 64);
    assert!(first.chars().all(|ch| ch.is_ascii_hexdigit()));

    let changed = code_fingerprint("3.0.1", 4, "20261017-v2.9.2", "m7-v1");
    assert_ne!(first, changed, "任一段变化都应改变指纹");
}
