//! crates/ramaria-service/src/import/vision_flow_tests.rs - Ramaria 导入图片理解链路端到端用例
//!
//! 设计特点:
//! - 端到端：含图导出 → L0 写入 → 图片理解 → L1 批量生成（真实临时库与图片文件 + mock LLM）
//! - 调用顺序断言：图片理解调用先于 L1 生成调用（先理解后 L1）
//! - 读取口注入：L1 请求的会话文本含 `[图片: 描述]`；理解失败时按占位符继续
//!
//! 安全约束:
//! - 全部数据为合成样例；不访问 OS keychain、不连网。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_importer::qq::ImportSide;

use super::*;
use crate::test_support::{L1_JSON_REPLY, MockLlm, engine_with_shared_llm};

// ---- 测试脚手架 ----

/// 样例图片 md5（小写）。
const IMAGE_MD5: &str = "fccb86f2a1b695df3c37a1042a967a44";
/// 样例图片落盘文件名（`{md5}_{大写 md5}.{ext}`，与导出器命名一致）。
const IMAGE_FILE: &str = "fccb86f2a1b695df3c37a1042a967a44_FCCB86F2A1B695DF3C37A1042A967A44.jpg";
/// mock 图片理解回复（断言注入文本时引用）。
const IMAGE_CAPTION: &str = "一只橘猫趴在窗台上。";

/// 含图私聊导出（单 session：导出者发图 + 对方文本回复）。
fn qq_export_with_image() -> String {
    let base = 1_700_300_000_000i64;
    let second = base + 60_000;
    format!(
        r#"{{"chatInfo":{{"selfUid":"u_self","selfName":"小明","selfUin":"10001","name":"小红","type":"private","peerUid":"u_peer","peerUin":"90002"}},"messages":[
        {{"id":"i_0","timestamp":{base},"type":"text","recalled":false,"system":false,"content":{{"text":"看这张图 [图片:FCCB86F2A1B695DF3C37A1042A967A44.jpg]","elements":[{{"type":"image","data":{{"filename":"FCCB86F2A1B695DF3C37A1042A967A44.jpg","size":487719,"width":640,"height":400,"md5":"{IMAGE_MD5}","url":"resources/images/{IMAGE_FILE}","subType":"photo","localPath":"images/{IMAGE_FILE}"}}}}]}},"sender":{{"uid":"u_self","name":"小明"}}}},
        {{"id":"i_1","timestamp":{second},"type":"text","recalled":false,"system":false,"content":{{"text":"真可爱","elements":[]}},"sender":{{"uid":"u_peer","name":"小红"}}}}
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

/// 私聊 L1 批量计划（双方 persona 各生成一份、无级联与节流）。
fn l1_plan(outcome: &ImportL0Outcome) -> ImportL1Plan {
    let mut targets: Vec<Option<String>> = Vec::new();
    if let Some(uid) = &outcome.persona_uid {
        targets.push(Some(uid.clone()));
    }
    if let Some(uid) = &outcome.other_persona_uid {
        targets.push(Some(uid.clone()));
    }
    ImportL1Plan {
        targets,
        cascade: false,
        throttle_ms: 0,
        group_fanout: false,
    }
}

/// 装配"含图导出 + 真实图片文件 + 共享 mock LLM"的链路环境。
///
/// 返回 (engine, mock, 导出文件路径, 临时目录)；调用方负责清理目录。
async fn vision_flow_env(
    tag: &str,
    config: RamariaConfig,
) -> (Engine, Arc<MockLlm>, PathBuf, PathBuf) {
    let llm = Arc::new(MockLlm::with_reply(L1_JSON_REPLY).with_vision_reply(IMAGE_CAPTION));
    let (engine, _storage, dir) = engine_with_shared_llm(tag, Arc::clone(&llm), config, None).await;

    let image_path = dir.join("resources").join("images").join(IMAGE_FILE);
    std::fs::create_dir_all(image_path.parent().expect("图片文件应有父目录"))
        .expect("创建图片目录应成功");
    std::fs::write(&image_path, b"fake-jpeg-bytes").expect("写入图片应成功");

    let export_path = dir.join("export.json");
    std::fs::write(&export_path, qq_export_with_image()).expect("写入导出文件应成功");

    (engine, llm, export_path, dir)
}

// ---- 端到端 ----

/// 含图导入：图片理解先于 L1 执行，L1 会话文本注入图片描述。
#[tokio::test]
async fn import_vision_flow_runs_before_l1_and_injects_description() {
    let (engine, llm, export_path, dir) =
        vision_flow_env("vision-flow-order", vision_on_config()).await;

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");
    assert_eq!(outcome.chat_type, "private");

    let stat = engine
        .understand_import_attachments(&outcome.session_ids, &dir)
        .await
        .expect("图片理解应成功");
    assert_eq!(stat.scanned, 1, "应扫描到一条待处理附件");
    assert_eq!(stat.done, 1, "本地图片应完成理解");

    let l1 = engine
        .generate_import_l1(&outcome.session_ids, l1_plan(&outcome), None)
        .await
        .expect("L1 批量生成应成功");
    assert!(l1.l1_success > 0, "L1 应至少成功一次");

    // 调用顺序：全部图片理解调用（含探测）先于全部 L1 文本生成调用
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

    // 读取口注入：L1 请求的会话文本包含图片描述
    let l1_requests = llm.requests();
    let injected = format!("[图片: {IMAGE_CAPTION}]");
    assert!(
        l1_requests
            .iter()
            .any(|request| request.user_message.contains(&injected)),
        "L1 会话文本应注入图片描述: {}",
        l1_requests.len()
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 理解失败：附件批量跳过，L1 按占位符继续生成。
#[tokio::test]
async fn import_vision_failure_keeps_placeholder_and_l1_continues() {
    let (engine, llm, export_path, dir) =
        vision_flow_env("vision-flow-failure", vision_on_config()).await;
    // 探测即失败（剩余次数充足，内容理解不再发起）
    llm.set_vision_failures(10);

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let stat = engine
        .understand_import_attachments(&outcome.session_ids, &dir)
        .await
        .expect("图片理解应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1, "探测失败应批量跳过");
    assert_eq!(stat.done, 0);

    let l1 = engine
        .generate_import_l1(&outcome.session_ids, l1_plan(&outcome), None)
        .await
        .expect("L1 批量生成应成功");
    assert!(l1.l1_success > 0, "理解失败不应阻塞 L1");

    // 占位符继续：L1 会话文本保留指纹占位符
    let l1_requests = llm.requests();
    assert!(
        l1_requests
            .iter()
            .any(|request| request.user_message.contains("[图片#fccb86f2]")),
        "L1 应按占位符继续"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// 声明关闭（默认配置）：理解零调用、附件跳过，L1 正常生成。
#[tokio::test]
async fn import_vision_declared_off_skips_without_calls() {
    let (engine, llm, export_path, dir) =
        vision_flow_env("vision-flow-off", RamariaConfig::default()).await;

    let outcome = engine
        .import_qq_l0(fast_import_request(&export_path))
        .await
        .expect("L0 导入应成功");

    let stat = engine
        .understand_import_attachments(&outcome.session_ids, &dir)
        .await
        .expect("图片理解应成功");
    assert_eq!(stat.scanned, 1);
    assert_eq!(stat.skipped, 1, "声明关应批量跳过");
    assert_eq!(llm.vision_call_count(), 0, "声明关不应发起理解调用");

    let l1 = engine
        .generate_import_l1(&outcome.session_ids, l1_plan(&outcome), None)
        .await
        .expect("L1 批量生成应成功");
    assert!(l1.l1_success > 0, "声明关不应影响 L1");

    let _ = std::fs::remove_dir_all(&dir);
}
