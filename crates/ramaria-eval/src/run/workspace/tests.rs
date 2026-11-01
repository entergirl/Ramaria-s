//! crates/ramaria-eval/src/run/workspace/tests.rs - Ramaria 评测工作区单元测试
//!
//! 设计特点:
//! - 路径断言覆盖默认与严格两种布局，严格目录名锁定标识替换规则
//! - 工作库复制只做首次拷贝，二次调用保留工作库现状
//! - 配置覆盖逐项断言路径、通道、主动对话、记忆条数与 utt 开关
//! - 快照写入后可原样解析，临时文件不残留

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::assembly::EVAL_CONFIG_FILE;
use crate::run::{AnswerModel, Pipeline};

use super::*;

// =========================================================
// 夹具
// =========================================================

fn temp_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-workspace-{tag}-{nanos}"))
}

fn cleanup(dir: &Path) {
    let _ = std::fs::remove_dir_all(dir);
}

fn condition() -> Condition {
    Condition {
        id: "u1-m1/l1-l2/800/0".to_string(),
        tier: "u1-m1".to_string(),
        pipeline: Pipeline::L1L2,
        budget_chars: 800,
        answer_model: AnswerModel {
            kind: AnswerModelKind::Local,
            base_url: "http://127.0.0.1:1234/v1".to_string(),
            model: "local-model".to_string(),
        },
    }
}

fn write_baseline(dir: &Path) -> PathBuf {
    std::fs::create_dir_all(dir).expect("基线目录应可创建");
    let db = dir.join("ramaria.db");
    std::fs::write(&db, b"baseline").expect("基线库夹具应可写");
    db
}

// =========================================================
// 目录与路径
// =========================================================

/// 创建工作区时同时建运行根与工作目录，重复调用不报错。
#[test]
fn new_creates_run_and_work_dirs() {
    let dir = temp_dir("new");
    let workspace = RunWorkspace::new(dir.clone()).expect("工作区应可创建");

    assert_eq!(workspace.root(), dir.as_path());
    assert!(dir.join(WORK_DIR).is_dir(), "工作目录应存在");

    let again = RunWorkspace::new(dir.clone()).expect("重复创建应成功");
    assert_eq!(again.root(), dir.as_path());
    cleanup(&dir);
}

/// 默认布局共用一个库，严格布局按条件标识分目录。
#[test]
fn work_db_uses_shared_or_condition_path() {
    let dir = temp_dir("paths");
    let workspace = RunWorkspace::new(dir.clone()).expect("工作区应可创建");
    let condition = condition();

    assert_eq!(
        workspace.work_db(&condition, false),
        dir.join(WORK_DIR).join("ramaria.db")
    );
    assert_eq!(
        workspace.work_db(&condition, true),
        dir.join(WORK_DIR)
            .join("u1-m1_l1-l2_800_0")
            .join("ramaria.db"),
        "严格模式目录名为条件标识替换分隔符"
    );
    cleanup(&dir);
}

// =========================================================
// 工作库复制
// =========================================================

/// 首次调用复制基线库，二次调用不覆盖工作库内容。
#[test]
fn prepare_work_db_copies_once() {
    let dir = temp_dir("copy");
    let baseline = write_baseline(&dir.join("baseline"));
    let workspace = RunWorkspace::new(dir.join("run")).expect("工作区应可创建");
    let condition = condition();

    let target = workspace
        .prepare_work_db(&condition, true, &baseline)
        .expect("复制应成功");
    assert_eq!(std::fs::read(&target).expect("工作库应可读"), b"baseline");
    assert_eq!(target, workspace.work_db(&condition, true));
    assert!(target.parent().expect("应有父目录").is_dir());

    std::fs::write(&target, b"local-state").expect("工作库应可写");
    let again = workspace
        .prepare_work_db(&condition, true, &baseline)
        .expect("二次调用应成功");
    assert_eq!(again, target);
    assert_eq!(
        std::fs::read(&again).expect("工作库应可读"),
        b"local-state",
        "已存在的工作库不应被覆盖"
    );
    cleanup(&dir);
}

/// 基线库缺失时复制失败，错误带源与目标路径。
#[test]
fn prepare_work_db_requires_baseline() {
    let dir = temp_dir("missing");
    let workspace = RunWorkspace::new(dir.join("run")).expect("工作区应可创建");
    let missing = dir.join("baseline").join("ramaria.db");

    let error = workspace
        .prepare_work_db(&condition(), false, &missing)
        .expect_err("基线缺失应报错");
    assert!(
        matches!(&error, RamariaError::Io { .. }),
        "应为 Io，实际 {error:?}"
    );
    assert!(error.to_string().contains("复制基线库失败"));
    assert!(error.to_string().contains("ramaria.db"));
    cleanup(&dir);
}

// =========================================================
// 配置快照
// =========================================================

/// 覆盖项把路径指向工作库目录，并关闭通道与主动对话。
#[test]
fn overrides_point_paths_to_work_dir() {
    let work_dir = PathBuf::from("work-dir");
    let mut config = RamariaConfig::default();
    config.channel.enabled = true;
    config.channel.qq_enabled = true;
    config.channel.telegram_enabled = true;
    config.channel.proactive_delivery_enabled = true;
    config.proactive.enabled = true;
    config.utt.enabled = false;

    apply_condition_overrides(&mut config, &work_dir, Pipeline::Layered);

    assert_eq!(config.paths.data_dir, "work-dir");
    assert_eq!(config.paths.config_dir, "work-dir");
    assert_eq!(
        config.paths.log_dir,
        work_dir.join("logs").display().to_string()
    );
    assert_eq!(
        config.paths.vector_index_dir,
        work_dir.join("vectors").display().to_string()
    );
    assert!(!config.channel.enabled);
    assert!(!config.channel.qq_enabled);
    assert!(!config.channel.telegram_enabled);
    assert!(!config.channel.proactive_delivery_enabled);
    assert!(!config.proactive.enabled);
    assert_eq!(config.retrieval.rag_max_memories, 20);
    assert!(config.utt.enabled);
}

/// 快照写到指定目录，临时文件不残留，重新解析后逐键一致。
#[test]
fn config_snapshot_roundtrips() {
    let dir = temp_dir("snapshot");
    std::fs::create_dir_all(&dir).expect("目录应可创建");
    let mut config = RamariaConfig::default();
    apply_condition_overrides(&mut config, &dir, Pipeline::Layered);

    let path = write_condition_config(&dir, &config).expect("写快照应成功");
    assert_eq!(path, dir.join(EVAL_CONFIG_FILE));
    assert!(path.is_file());
    assert!(
        !dir.join(format!("{EVAL_CONFIG_FILE}.tmp")).exists(),
        "临时文件应已改名"
    );

    let parsed = read_baseline_config(&path).expect("快照应可解析");
    let expected = toml::Value::try_from(&config).expect("配置应可转为 TOML 值");
    let actual = toml::Value::try_from(&parsed).expect("配置应可转为 TOML 值");
    assert_eq!(actual, expected, "快照应与写入配置逐键一致");
    cleanup(&dir);
}
