//! crates/ramaria-eval/src/sandbox/backend/tests.rs - Ramaria 评测沙盒后端解析单元测试
//!
//! 设计特点:
//! - 继承来源只测无宿主库的回退：读库正例由服务层用例覆盖，避免评测层重复造库
//! - 独立来源覆盖本地补默认地址、云端缺项拒绝与合法云端配置
//! - 应用用例走真实评测装配，验证降级告警与报告状态，不依赖真实模型服务
//!
//! 安全约束:
//! - 不发起真实模型请求；连通性探测仅一次本机地址尝试，结果只做枚举断言。

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

// =========================================================
// 夹具
// =========================================================

/// 独立临时沙盒根：同一进程内并行用例互不干扰。
fn temp_root(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or_default();
    std::env::temp_dir().join(format!("ramaria-eval-backend-{tag}-{nanos}"))
}

/// 删除临时目录；清理失败不掩盖断言结果。
fn cleanup(root: &Path) {
    let _ = std::fs::remove_dir_all(root);
}

// =========================================================
// 继承来源
// =========================================================

/// 无宿主库路径：回退产品默认本地模型并给出降级原因。
#[tokio::test]
async fn inherit_without_user_db_falls_back_to_local_default() {
    let resolved = resolve_backend(&SandboxBackendChoice::Inherit, None)
        .await
        .expect("继承解析不应报错");

    assert_eq!(resolved.mode, SandboxBackendMode::Inherit);
    assert_eq!(resolved.kind, SandboxModelKind::Local);
    assert_eq!(resolved.source, "产品默认");
    assert_eq!(resolved.config.base_url, "http://localhost:1234/v1");
    let fallback = resolved.fallback.expect("应记录降级原因");
    assert!(
        fallback.contains("未提供用户库路径"),
        "降级原因应说明缺库路径，实际: {fallback}"
    );

    // 缺库文件同样回退（只读读取不建库）
    let missing = temp_root("inherit-missing").join("ramaria.db");
    let resolved = resolve_backend(&SandboxBackendChoice::Inherit, Some(&missing))
        .await
        .expect("缺库文件不应报错");
    assert_eq!(resolved.source, "产品默认");
    assert!(
        resolved
            .fallback
            .as_deref()
            .unwrap_or("")
            .contains("未找到用户后端配置"),
        "降级原因应说明未找到配置"
    );
}

// =========================================================
// 独立来源
// =========================================================

/// 本地独立配置：空地址补默认地址并留降级原因，模型标识可为空。
#[tokio::test]
async fn independent_local_fills_default_base_url() {
    let resolved = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Local,
            base_url: "   ".to_string(),
            model_id: String::new(),
            embedding_model_path: Some(PathBuf::from("models/bge")),
        },
        None,
    )
    .await
    .expect("本地独立配置应解析成功");

    assert_eq!(resolved.mode, SandboxBackendMode::Local);
    assert_eq!(resolved.kind, SandboxModelKind::Local);
    assert_eq!(resolved.source, "独立配置");
    assert_eq!(resolved.config.base_url, "http://localhost:1234/v1");
    assert!(resolved.config.capability.model_id.is_empty());
    assert_eq!(
        resolved.config.embedding_model_path.as_deref(),
        Some("models/bge")
    );
    assert!(
        resolved
            .fallback
            .as_deref()
            .unwrap_or("")
            .contains("未填写地址"),
        "空地址补默认值时应记录降级原因"
    );

    // 地址非空：原样保留且不产生降级原因
    let resolved = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Local,
            base_url: "http://localhost:7777/v1".to_string(),
            model_id: "qwen-local".to_string(),
            embedding_model_path: None,
        },
        None,
    )
    .await
    .expect("本地独立配置应解析成功");
    assert_eq!(resolved.config.base_url, "http://localhost:7777/v1");
    assert_eq!(resolved.config.capability.model_id, "qwen-local");
    assert!(resolved.config.embedding_model_path.is_none());
    assert!(resolved.fallback.is_none(), "地址齐全时不应降级");
}

/// 云端独立配置：缺地址或模型标识按校验错误拒绝，文案不出现厂商名。
#[tokio::test]
async fn independent_cloud_rejects_missing_fields() {
    let missing_url = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Cloud,
            base_url: String::new(),
            model_id: "cloud-model".to_string(),
            embedding_model_path: None,
        },
        None,
    )
    .await
    .expect_err("缺地址应报错");
    assert_eq!(missing_url.category(), "validation");
    assert!(
        missing_url.to_string().contains("云端模型必须填写地址"),
        "错误应说明缺地址，实际: {missing_url}"
    );

    let missing_model = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Cloud,
            base_url: "https://example.invalid/v1".to_string(),
            model_id: "  ".to_string(),
            embedding_model_path: None,
        },
        None,
    )
    .await
    .expect_err("缺模型标识应报错");
    assert_eq!(missing_model.category(), "validation");
    assert!(
        missing_model
            .to_string()
            .contains("云端模型必须填写模型标识"),
        "错误应说明缺模型标识，实际: {missing_model}"
    );
}

/// 云端独立配置合法时：类别为云端、来源为独立配置、无降级原因。
#[tokio::test]
async fn independent_cloud_keeps_kind_and_source() {
    let resolved = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Cloud,
            base_url: "https://example.invalid/v1".to_string(),
            model_id: "cloud-model".to_string(),
            embedding_model_path: None,
        },
        None,
    )
    .await
    .expect("合法云端配置应解析成功");

    assert_eq!(resolved.mode, SandboxBackendMode::Cloud);
    assert_eq!(resolved.kind, SandboxModelKind::Cloud);
    assert_eq!(resolved.source, "独立配置");
    assert_eq!(resolved.config.base_url, "https://example.invalid/v1");
    assert_eq!(resolved.config.capability.model_id, "cloud-model");
    assert!(resolved.fallback.is_none());
}

// =========================================================
// 应用
// =========================================================

/// 应用独立本地配置：配置生效、向量模型缺失只降级向量通道，摘要不含厂商名。
#[tokio::test]
async fn apply_backend_keeps_running_when_embedding_missing() {
    let root = temp_root("apply");
    let engine = crate::assembly::open_engine(&root)
        .await
        .expect("装配应成功");

    let resolved = resolve_backend(
        &SandboxBackendChoice::Independent {
            kind: SandboxModelKind::Local,
            base_url: String::new(),
            model_id: String::new(),
            embedding_model_path: Some(PathBuf::from("models/not-installed")),
        },
        None,
    )
    .await
    .expect("本地独立配置应解析成功");

    let (report, warnings) = apply_backend(&engine, resolved).await;

    assert_eq!(report.source, "独立配置", "配置应用应成功");
    assert_eq!(report.kind, SandboxModelKind::Local);
    assert_eq!(report.base_url, "http://localhost:1234/v1");
    assert_eq!(report.model_text(), "自动选择");
    assert_eq!(report.embedding_status, "加载失败");
    assert!(
        warnings.iter().any(|item| item.contains("未填写地址")),
        "地址降级应写入告警: {warnings:?}"
    );
    assert!(
        warnings
            .iter()
            .any(|item| item.contains("向量模型加载失败")),
        "向量模型失败应写入告警: {warnings:?}"
    );
    assert!(
        matches!(report.llm_status.as_str(), "可达" | "不可达"),
        "连通性只取两态，实际: {}",
        report.llm_status
    );

    let text = summary(&report);
    for needle in [
        "后端: 独立配置（本地模型）",
        "模型: 自动选择",
        "向量: 加载失败",
    ] {
        assert!(text.contains(needle), "摘要应包含 {needle}，实际: {text}");
    }
    for vendor in [
        "LM Studio",
        "DeepSeek",
        "OpenAI",
        "lm_studio",
        "deepseek",
        "openai",
    ] {
        assert!(
            !text.contains(vendor),
            "摘要不应出现厂商名 {vendor}，实际: {text}"
        );
    }

    if let Some(pool) = engine.sqlite_pool() {
        pool.close().await;
    }
    cleanup(&root);
}
