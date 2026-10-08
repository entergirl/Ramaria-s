//! crates/ramaria-service/src/import/post_tests.rs - Ramaria 导入后处理编排用例
//!
//! 设计特点:
//! - 端到端：真实临时库 + 含图导出 + 共享 mock LLM，覆盖 图片理解 → L1 → 深度触发 的顺序与计数
//! - 顺序断言：图片理解调用先于 L1 生成调用（经共享 mock 的调用序列）
//! - 触发断言：深度触发的进度事件回填 L1 总调用数；非深度模式零 l2/l3 事件
//! - 边界：未提供导出根零图片理解调用；空会话计数全零且不触发深度
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use ramaria_core::config::RamariaConfig;
use ramaria_importer::qq::ImportSide;

use super::*;
use crate::test_support::{L1_JSON_REPLY, MockLlm, engine_with_shared_llm};

// ---- 测试脚手架 ----

/// 样例图片 md5（小写）。
const IMAGE_MD5: &str = "fccb86f2a1b695df3c37a1042a967a44";
/// 样例图片落盘文件名（`{md5}_{大写 md5}.{ext}`，与导出器命名一致）。
const IMAGE_FILE: &str = "fccb86f2a1b695df3c37a1042a967a44_FCCB86F2A1B695DF3C37A1042A967A44.jpg";

/// 含图私聊导出（单 session：导出者发图 + 对方文本回复）。
fn qq_export_with_image() -> String {
    let base = 1_700_300_000_000i64;
    let second = base + 60_000;
    format!(
        r#"{{"chatInfo":{{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"}},"messages":[
        {{"id":"p_0","timestamp":{base},"type":"text","recalled":false,"system":false,"content":{{"text":"看这张图 [图片:FCCB86F2A1B695DF3C37A1042A967A44.jpg]","elements":[{{"type":"image","data":{{"filename":"FCCB86F2A1B695DF3C37A1042A967A44.jpg","size":487719,"width":640,"height":400,"md5":"{IMAGE_MD5}","url":"resources/images/{IMAGE_FILE}","subType":"photo","localPath":"images/{IMAGE_FILE}"}}}}]}},"sender":{{"uid":"u_self","name":"小明"}}}},
        {{"id":"p_1","timestamp":{second},"type":"text","recalled":false,"system":false,"content":{{"text":"真可爱","elements":[]}},"sender":{{"uid":"u_peer","name":"小红"}}}}
        ]}}"#
    )
}

/// 开启图片理解的配置（默认声明为关）。
fn vision_on_config() -> RamariaConfig {
    let mut config = RamariaConfig::default();
    config.vision.model_supports_vision = true;
    config
}

/// 私聊 L0 导入请求（快速模式、双方、切割间隔 10 分钟）。
fn fast_import_request(file_path: &Path) -> ImportRequest {
    ImportRequest {
        file_path: file_path.to_path_buf(),
        mode: ImportMode::Fast,
        gap_minutes: 10,
        side: ImportSide::Both,
        persona_name: None,
        self_persona_uid: None,
        other_persona_name: None,
        other_persona_uid: None,
    }
}

/// 私聊后处理计划（默认前缀、无逐次级联、无节流）。
fn post_plan(cascade_deep: bool, targets: Vec<Option<String>>) -> ImportPostPlan {
    ImportPostPlan {
        l1_targets: targets,
        l1_prefix: None,
        group_fanout: false,
        l1_cascade: false,
        cascade_deep,
        throttle_ms: 0,
    }
}

/// 记录进度事件的测试 sink。
struct RecordingSink {
    events: Mutex<Vec<ImportL1Progress>>,
}

impl RecordingSink {
    fn new() -> Self {
        Self {
            events: Mutex::new(Vec::new()),
        }
    }

    fn events(&self) -> Vec<ImportL1Progress> {
        self.events.lock().expect("进度事件锁不应中毒").clone()
    }
}

impl ImportProgressSink for RecordingSink {
    fn on_l1_progress(&self, p: &ImportL1Progress) {
        self.events
            .lock()
            .expect("进度事件锁不应中毒")
            .push(p.clone());
    }

    fn on_done(&self, _summary: &ImportDoneSummary) {}
}

/// 装配"含图导出环境 + 共享 mock LLM"的引擎（真实临时库 + 图片文件）。
///
/// 返回 (engine, mock, 临时目录)；调用方负责清理目录。
async fn post_env(tag: &str, config: RamariaConfig) -> (Engine, Arc<MockLlm>, PathBuf) {
    let llm =
        Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply("一只橘猫趴在窗台上。"));
    let (engine, _storage, dir) = engine_with_shared_llm(tag, Arc::clone(&llm), config, None).await;

    let image_path = dir.join("resources").join("images").join(IMAGE_FILE);
    std::fs::create_dir_all(image_path.parent().expect("图片文件应有父目录"))
        .expect("创建图片目录应成功");
    std::fs::write(&image_path, b"fake-jpeg-bytes").expect("写入图片应成功");

    (engine, llm, dir)
}

// ---- 用例 ----

/// 顺序闭环：导出根存在且含待理解附件时，图片理解先于 L1 生成。
#[tokio::test]
async fn post_runs_vision_before_l1() {
    let (engine, llm, dir) = post_env("post-vision-order", vision_on_config()).await;
    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_export_with_image()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: outcome.session_ids.clone(),
                export_root: Some(dir.clone()),
                plan: post_plan(false, vec![None]),
            },
            None,
        )
        .await
        .expect("后处理应成功");

    // 图片理解统计：扫描 1 条并完成
    let stat = post.vision.expect("应返回图片理解统计");
    assert_eq!(stat.scanned, 1, "应扫描到一条待处理附件");
    assert_eq!(stat.done, 1, "本地图片应完成理解");

    // 调用顺序：全部图片理解调用先于全部 L1 文本生成调用
    let log = llm.call_log();
    let first_chat = log
        .iter()
        .position(|kind| *kind == "chat")
        .expect("应存在文本生成调用");
    let last_vision = log
        .iter()
        .rposition(|kind| *kind == "chat_vision")
        .expect("应存在图片理解调用");
    assert!(last_vision < first_chat, "图片理解应先于 L1 生成: {log:?}");
    assert_eq!(post.l1.l1_success, 1, "L1 应生成成功");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 深度触发：cascade_deep 且 L1 成功 > 0 时触发，进度事件回填 L1 总调用数。
#[tokio::test]
async fn post_cascade_deep_triggers_with_l1_total() {
    let (engine, _llm, dir) = post_env("post-deep-trigger", vision_on_config()).await;
    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_export_with_image()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let sink = RecordingSink::new();
    // 两个目标 → L1 总调用数 2（单 session），与 session 数（1）可区分
    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: outcome.session_ids.clone(),
                export_root: Some(dir.clone()),
                plan: post_plan(true, vec![None, None]),
            },
            Some(&sink),
        )
        .await
        .expect("后处理应成功");

    assert!(post.l1.l1_success > 0, "L1 应至少成功一次");
    assert!(post.l2_triggered, "深度模式应触发 L2");
    assert!(post.l3_triggered, "深度模式应触发 L3");

    let events = sink.events();
    let l2 = events
        .iter()
        .find(|e| e.phase == "l2")
        .expect("应有 L2 阶段进度");
    assert_eq!(l2.l1_total, Some(2), "深度进度应回填 L1 批量生成的总调用数");
    let l3 = events
        .iter()
        .find(|e| e.phase == "l3")
        .expect("应有 L3 阶段进度");
    assert_eq!(l3.l1_total, Some(2));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 非深度模式：cascade_deep=false 不触发深度，进度只含 L1 阶段。
#[tokio::test]
async fn post_without_cascade_deep_skips_deep() {
    let (engine, _llm, dir) = post_env("post-no-deep", vision_on_config()).await;
    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_export_with_image()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let sink = RecordingSink::new();
    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: outcome.session_ids.clone(),
                export_root: Some(dir.clone()),
                plan: post_plan(false, vec![None]),
            },
            Some(&sink),
        )
        .await
        .expect("后处理应成功");

    assert!(
        !post.l2_triggered && !post.l3_triggered,
        "非深度模式不应触发级联"
    );
    assert!(
        sink.events().iter().all(|e| e.phase == "l1"),
        "非深度模式进度应只含 L1 阶段"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 未提供导出根：跳过图片理解，零图片理解调用，L1 正常生成。
#[tokio::test]
async fn post_without_export_root_skips_vision_calls() {
    let (engine, llm, dir) = post_env("post-no-root", vision_on_config()).await;
    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_export_with_image()).expect("写入导出文件应成功");

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: outcome.session_ids.clone(),
                export_root: None,
                plan: post_plan(false, vec![None]),
            },
            None,
        )
        .await
        .expect("后处理应成功");

    assert!(post.vision.is_none(), "未提供导出根时不应执行图片理解");
    assert_eq!(llm.vision_call_count(), 0, "未提供导出根时应零图片理解调用");
    assert_eq!(post.l1.l1_success, 1, "L1 应正常生成");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 边界：空会话计数全零且不触发深度（即使 cascade_deep=true）。
#[tokio::test]
async fn post_with_empty_sessions_counts_zero_and_skips_deep() {
    let (engine, _llm, dir) = post_env("post-empty", vision_on_config()).await;

    let post = engine
        .run_import_post(
            ImportPostRequest {
                session_ids: Vec::new(),
                export_root: None,
                plan: post_plan(true, vec![None]),
            },
            None,
        )
        .await
        .expect("空会话后处理应成功");

    assert_eq!(post.l1.l1_total, 0, "空会话计划调用总数为 0");
    assert_eq!(post.l1.l1_success, 0);
    assert_eq!(post.l1.l1_skipped, 0);
    assert_eq!(post.l1.l1_failed, 0);
    assert!(
        !post.l2_triggered && !post.l3_triggered,
        "空会话不应触发深度"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
