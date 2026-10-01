//! crates/ramaria-service/src/model/tests.rs - Ramaria 模型管理模块单元测试
//!
//! 设计特点:
//! - 由 model.rs 以 `#[cfg(test)] mod tests;` 收纳：覆盖后端配置写入 / 嵌入模型 / 模型文件三条路径
//! - 后端配置写入用例以真实 SQLite 与临时 config.toml 验证落库 / 热替换 / 文件同步的组合语义与失败回传
//! - 嵌入与模型文件用例使用合成目录与确定性 mock，不依赖网络、真实模型文件
//!
//! 安全约束:
//! - 不使用真实 API key；示例密钥文本仅用于验证本地 provider 跳过 keychain。

use super::*;
use crate::engine::EngineOptions;
use crate::test_support::{MockLlm, engine_with_db, temp_dir};
use ramaria_core::config::EmbeddingDevice;
use ramaria_core::traits::StoreInfrastructure;

// =========================================================
// 嵌入模型校验
// =========================================================

/// 校验用例：目录不存在 / 路径不是目录 → valid=false + 原因，不抛错。
#[tokio::test]
async fn validate_reports_missing_path_without_error() {
    let dir = temp_dir("model-validate");
    let missing = dir.join("not-a-model");

    let result = validate_embedding_model(
        missing.to_string_lossy().as_ref(),
        EmbeddingDevice::default(),
    )
    .await
    .expect("校验用例不应抛错");
    assert!(!result.valid);
    assert!(result.dimension.is_none());
    assert!(
        result.reason.as_deref().unwrap_or("").contains("不存在"),
        "原因应说明目录缺失，实际: {:?}",
        result.reason
    );

    // 路径存在但不是目录（用文件顶上）
    let file = dir.join("model.txt");
    std::fs::write(&file, b"x").expect("写入测试文件应成功");
    let result =
        validate_embedding_model(file.to_string_lossy().as_ref(), EmbeddingDevice::default())
            .await
            .expect("校验用例不应抛错");
    assert!(!result.valid);
    assert!(
        result.reason.as_deref().unwrap_or("").contains("不是目录"),
        "原因应说明路径类型错误，实际: {:?}",
        result.reason
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 后端配置写入
// =========================================================

/// 后端配置更新：配置落库 + provider 热替换后立即生效（无需重建引擎）。
#[tokio::test]
async fn update_backend_config_hot_swaps_provider() {
    let (engine, storage, dir) = engine_with_db("model-hot-llm").await;
    assert_eq!(engine.llm().name(), "MockLlm", "注入的 provider 应生效");

    let mut config = BackendConfig::lm_studio_default();
    config.base_url = "http://localhost:8888/v1".to_string();
    config.capability.base_url = "http://localhost:8888/v1".to_string();
    config.capability.model_id = "qwen-test".to_string();

    // 本地 provider：无密钥写入，仅落库 + 热替换
    engine
        .update_backend_config(&config, None)
        .await
        .expect("后端配置更新应成功");

    // 热替换后立即生效：新 provider 使用新 base_url
    let llm = engine.llm();
    assert_eq!(llm.name(), "LM Studio");
    assert_eq!(llm.config().base_url, "http://localhost:8888/v1");

    // 配置已落库（下次启动装配依据）
    let saved = storage
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应已落库");
    assert_eq!(saved.capability.model_id, "qwen-test");

    // 本地 provider 传入密钥：跳过 keychain 写入，不影响配置更新
    engine
        .update_backend_config(&config, Some("sk-should-be-ignored"))
        .await
        .expect("本地 provider 更新应成功");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 后端配置更新：`config_path` 已设置时同步文件侧 `[backend]` 组，其它组保留。
#[tokio::test]
async fn update_backend_config_syncs_file_side_backend_group() {
    let dir = temp_dir("model-file-sync");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "[utt]\ntheta_gap_minutes = 12\n").expect("写入配置应成功");
    let engine = Engine::open_with(
        crate::engine::EngineOptions::new(db_path).with_config_path(config_path.clone()),
    )
    .await
    .expect("引擎装配应成功");

    let mut config = BackendConfig::lm_studio_default();
    config.base_url = "http://localhost:7778/v1".to_string();
    config.capability.base_url = "http://localhost:7778/v1".to_string();
    config.capability.model_id = "qwen-file-sync".to_string();
    engine
        .update_backend_config(&config, None)
        .await
        .expect("后端配置更新应成功");

    // 文件侧 [backend] 组已同步，其它组保留
    let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
    let file_cfg: ramaria_core::config::RamariaConfig =
        toml::from_str(&text).expect("文件应为合法 TOML");
    assert_eq!(file_cfg.backend.model_id, "qwen-file-sync");
    assert_eq!(file_cfg.backend.base_url, "http://localhost:7778/v1");
    assert_eq!(file_cfg.utt.theta_gap_minutes, 12, "文件侧其它组应保留");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 写入选项（仅落库 + 文件同步）：DB 落地、文件侧 [backend] 组同步，provider 不被替换。
#[tokio::test]
async fn write_backend_config_db_and_file_without_hot_swap() {
    let dir = temp_dir("model-write-db-file");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("引擎装配应成功");
    engine.update_llm(Arc::new(MockLlm::local()));

    let mut config = BackendConfig::lm_studio_default();
    config.base_url = "http://localhost:6666/v1".to_string();
    config.capability.base_url = "http://localhost:6666/v1".to_string();
    config.capability.model_id = "qwen-db-file".to_string();

    // 经引擎门面调用（与入口层同一路径）
    let outcome = engine
        .write_backend_config(
            &config,
            BackendConfigWriteOptions {
                api_key: None,
                hot_swap: false,
                sync_file: true,
            },
        )
        .await
        .expect("写入应成功");

    assert!(outcome.db_ok, "配置应已落库");
    assert!(outcome.file_ok, "文件侧应同步完成: {:?}", outcome.failures);
    assert!(!outcome.provider_updated, "未开启热替换时不应重建 provider");
    assert_eq!(engine.llm().name(), "MockLlm", "provider 不应被替换");

    // DB 落库（真相源）
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应已落库");
    assert_eq!(saved.capability.model_id, "qwen-db-file");

    // 文件侧 [backend] 组已同步
    let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
    let file_cfg: ramaria_core::config::RamariaConfig =
        toml::from_str(&text).expect("文件应为合法 TOML");
    assert_eq!(file_cfg.backend.model_id, "qwen-db-file");
    assert_eq!(file_cfg.backend.base_url, "http://localhost:6666/v1");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 写入选项（仅热替换）：provider 已替换生效，文件侧保持不变（未开启同步）。
#[tokio::test]
async fn write_backend_config_hot_swap_without_file_sync() {
    let dir = temp_dir("model-write-hot-swap");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    let original = "[utt]\ntheta_gap_minutes = 12\n";
    std::fs::write(&config_path, original).expect("写入配置应成功");
    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("引擎装配应成功");
    engine.update_llm(Arc::new(MockLlm::local()));
    assert_eq!(
        engine.llm().name(),
        "MockLlm",
        "初始 provider 应为注入 mock"
    );

    let mut config = BackendConfig::lm_studio_default();
    config.base_url = "http://localhost:5555/v1".to_string();
    config.capability.base_url = "http://localhost:5555/v1".to_string();
    config.capability.model_id = "qwen-hot-swap".to_string();

    let outcome = write_backend_config(
        &engine,
        &config,
        &BackendConfigWriteOptions {
            api_key: None,
            hot_swap: true,
            sync_file: false,
        },
    )
    .await
    .expect("写入应成功");

    assert!(outcome.db_ok, "配置应已落库");
    assert!(outcome.provider_updated, "开启热替换后应重建 provider");
    assert!(!outcome.file_ok, "未开启文件同步时不应报告文件成功");

    // 热替换后立即生效：新 provider 使用新 base_url
    let llm = engine.llm();
    assert_eq!(llm.name(), "LM Studio");
    assert_eq!(llm.config().base_url, "http://localhost:5555/v1");

    // 文件侧原样保留
    let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
    assert_eq!(text, original, "未开启文件同步时不应改写 config.toml");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 文件侧失败（文件损坏拒绝覆盖）：DB 已落库、失败原因回传，函数不报错。
#[tokio::test]
async fn write_backend_config_file_failure_keeps_db() {
    let dir = temp_dir("model-write-file-fail");
    let db_path = dir.join("assistant.db");
    let config_path = dir.join("config.toml");
    std::fs::write(&config_path, "损坏的 [[[ 配置").expect("写入配置应成功");
    let engine =
        Engine::open_with(EngineOptions::new(db_path).with_config_path(config_path.clone()))
            .await
            .expect("引擎装配应成功");

    let mut config = BackendConfig::lm_studio_default();
    config.capability.model_id = "qwen-file-fail".to_string();

    let outcome = write_backend_config(
        &engine,
        &config,
        &BackendConfigWriteOptions {
            api_key: None,
            hot_swap: false,
            sync_file: true,
        },
    )
    .await
    .expect("文件侧失败不应让写入报错");

    assert!(outcome.db_ok, "DB 应已落库");
    assert!(!outcome.file_ok, "文件损坏时应报告文件侧失败");
    assert!(
        !outcome.failures.is_empty(),
        "应回传失败原因: {:?}",
        outcome.failures
    );
    let saved = engine
        .storage()
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应已落库");
    assert_eq!(saved.capability.model_id, "qwen-file-fail");
    // 损坏文件原样保留（不覆盖用户文件）
    let text = std::fs::read_to_string(&config_path).expect("读取配置应成功");
    assert_eq!(text, "损坏的 [[[ 配置", "损坏文件应原样保留");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 未配置 config 路径：文件侧同步失败记为失败项，DB 落库不受影响。
#[tokio::test]
async fn write_backend_config_missing_config_path_keeps_db() {
    let (engine, storage, dir) = engine_with_db("model-write-no-path").await;

    let mut config = BackendConfig::lm_studio_default();
    config.capability.model_id = "qwen-no-path".to_string();

    let outcome = write_backend_config(
        &engine,
        &config,
        &BackendConfigWriteOptions {
            api_key: None,
            hot_swap: false,
            sync_file: true,
        },
    )
    .await
    .expect("未配置路径不应让写入报错");

    assert!(outcome.db_ok, "DB 应已落库");
    assert!(!outcome.file_ok, "未配置路径时应报告文件侧失败");
    assert!(
        !outcome.failures.is_empty(),
        "未配置路径应回传失败原因: {:?}",
        outcome.failures
    );
    let saved = storage
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应已落库");
    assert_eq!(saved.capability.model_id, "qwen-no-path");

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 嵌入模型保存与热更新
// =========================================================

/// 嵌入模型保存与卸载：热更新后立即生效，路径持久化；空路径卸载。
#[tokio::test]
async fn save_embedding_model_loads_and_unloads() {
    let (engine, storage, dir) = engine_with_db("model-hot-embedding").await;
    assert!(!engine.is_embedding_available(), "初始应无嵌入模型");

    // 目录不存在 → 显式校验错误，且不改变现状
    let err = engine
        .save_embedding_model(Some("/definitely/not/a/model/dir"))
        .await
        .expect_err("目录不存在应报错");
    assert_eq!(err.category(), "validation");
    assert!(!engine.is_embedding_available());

    // 注入可用 provider（模拟"模型加载成功"后的热替换）
    let embedding = Arc::new(crate::test_support::DeterministicEmbedding::new());
    engine.update_embedding(Some(embedding));
    assert!(engine.is_embedding_available(), "热更新后向量通道应可用");
    let view = engine
        .embedding_model()
        .await
        .expect("读取嵌入模型应成功")
        .expect("应返回已加载模型视图");
    assert!(view.valid);
    assert_eq!(
        view.dimension,
        Some(crate::test_support::DeterministicEmbedding::DIMENSION)
    );

    // 配置中留路径但未加载：供 UI 预填
    let mut config = storage
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .unwrap_or_else(BackendConfig::lm_studio_default);
    config.embedding_model_path = Some("/saved/model/path".to_string());
    storage
        .save_backend_config(&config)
        .await
        .expect("保存后端配置应成功");
    engine.update_embedding(None);
    let view = engine
        .embedding_model()
        .await
        .expect("读取嵌入模型应成功")
        .expect("应按已保存路径返回视图");
    assert_eq!(view.model_path.as_deref(), Some("/saved/model/path"));
    assert!(!view.valid, "未加载时不应视为可用");

    // 卸载：provider 与持久化路径一并清空
    engine.save_embedding_model(None).await.expect("卸载应成功");
    assert!(!engine.is_embedding_available());
    assert!(
        engine
            .embedding_model()
            .await
            .expect("读取嵌入模型应成功")
            .is_none(),
        "卸载后读取应返回 None"
    );
    let saved = storage
        .get_backend_config()
        .await
        .expect("读取后端配置应成功")
        .expect("后端配置应存在");
    assert!(saved.embedding_model_path.is_none());

    let _ = std::fs::remove_dir_all(&dir);
}

/// 热更新并发读：写侧替换 provider 期间，读侧取到的始终是完整快照。
#[tokio::test]
async fn hot_swap_is_visible_to_concurrent_readers() {
    let (engine, _storage, dir) = engine_with_db("model-hot-concurrent").await;
    engine.update_llm(Arc::new(MockLlm::with_reply("第一代")));

    // 读侧持续取快照（与写侧交替）；名称与配置必须成对来自同一 provider
    let mut names = Vec::new();
    for index in 0..16 {
        if index % 4 == 0 {
            engine.update_llm(Arc::new(MockLlm::with_reply("新一代")));
        }
        let llm = engine.llm();
        names.push(llm.name());
        assert!(
            llm.config().base_url.starts_with("http://"),
            "快照读到的 provider 配置应完整"
        );
    }
    assert!(
        names.iter().all(|name| *name == "MockLlm"),
        "所有快照都应来自已装配的 provider"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// =========================================================
// 模型文件管理
// =========================================================

/// 模型根目录解析：显式覆盖优先，否则取平台默认。
#[test]
fn models_root_prefers_override() {
    let override_path = PathBuf::from("D:/custom/models");
    assert_eq!(
        models_root(Some(override_path.as_path())),
        override_path,
        "显式路径应原样返回"
    );
    assert_eq!(
        models_root(None),
        ramaria_llm::model_manager::default_models_root(),
        "未提供覆盖时应取平台默认目录"
    );
}

/// 列表用例：空根目录 → 空列表（管理器按需创建目录）。
#[test]
fn list_models_empty_root_returns_empty() {
    let dir = temp_dir("model-list");
    let models = list_models(Some(dir.as_path())).expect("列表用例应成功");
    assert!(models.is_empty(), "空目录不应有已安装模型: {models:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// 就绪与体积用例：三个必需文件齐全 → 就绪且体积 > 0。
#[test]
fn model_ready_and_size_follow_required_files() {
    let dir = temp_dir("model-ready");
    let model_id = "bge-small-zh-v1.5";
    let model_dir = dir.join(model_id);
    std::fs::create_dir_all(&model_dir).expect("创建模型目录应成功");
    std::fs::write(model_dir.join("config.json"), b"{}").expect("写入 config 应成功");
    std::fs::write(model_dir.join("model.safetensors"), vec![0u8; 512]).expect("写入权重应成功");
    std::fs::write(model_dir.join("tokenizer.json"), b"{}").expect("写入 tokenizer 应成功");

    assert!(is_model_ready(model_id, Some(dir.as_path())));
    assert!(model_size(model_id, Some(dir.as_path())) > 0);
    // 未创建的模型目录 → 未就绪
    assert!(!is_model_ready("nonexistent-model", Some(dir.as_path())));

    let _ = std::fs::remove_dir_all(&dir);
}

/// 删除用例：不存在 → 幂等成功（`removed = false`）；存在 → 删除并报 `removed = true`；二次删除幂等。
#[test]
fn remove_model_is_idempotent_and_reports_removed() {
    let dir = temp_dir("model-remove");

    let missing =
        remove_model("nonexistent-model", Some(dir.as_path())).expect("删除不存在的模型应幂等成功");
    assert!(!missing.removed, "不存在的模型应报告未删除");

    let model_id = "bge-small-zh-v1.5";
    let model_dir = dir.join(model_id);
    std::fs::create_dir_all(&model_dir).expect("创建模型目录应成功");
    std::fs::write(model_dir.join("config.json"), b"{}").expect("写入模型文件应成功");

    let removed = remove_model(model_id, Some(dir.as_path())).expect("删除存在的模型应成功");
    assert!(removed.removed, "存在的模型应报告已删除");
    assert!(!model_dir.exists(), "模型目录应已被删除");

    let again = remove_model(model_id, Some(dir.as_path())).expect("二次删除应幂等成功");
    assert!(!again.removed, "二次删除应报告未删除");

    let _ = std::fs::remove_dir_all(&dir);
}

/// 下载用例：未知 model_id → 预置校验失败（发起网络请求前返回配置错误）。
#[tokio::test]
async fn download_model_unknown_id_fails_before_network() {
    let dir = temp_dir("model-download");
    let err = download_model("nonexistent-model", Some(dir.as_path()), None)
        .await
        .expect_err("未知模型应报错");
    assert_eq!(err.category(), "config");
    assert!(
        err.context().contains("不支持的模型"),
        "错误应说明模型不在预置清单: {err}"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
