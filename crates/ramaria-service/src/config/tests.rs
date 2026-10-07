//! crates/ramaria-service/src/config/tests.rs - Ramaria 配置双写用例单元测试
//!
//! 设计特点:
//! - mock storage（settings / backend_config 表）与临时目录，确定性断言
//! - 覆盖首启（文件缺失）/ 文件损坏 / 文件优先回写 / 统一写入口 / 只读加载等路径
//! - 覆盖扁平化往返、显式键集 diff、模板与默认值逐键一致、原子写失败清理
//! - 断言以文件与表两侧状态为准，不依赖系统时钟
//!
//! 安全约束:
//! - 仅使用合成样例数据与临时目录，不涉及真实 API key / 网络调用 / 用户数据。

use super::*;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{StorageBackend, StoreInfrastructure};
use ramaria_core::types::{BackendConfig, LlmProvider, PersonaKind};
use serde_json::Value as JsonValue;
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use super::flatten::{config_to_flat_map, flat_map_to_config, json_value_to_setting};

/// 最小 mock storage：仅实现本模块用到的 settings / backend_config 方法。
#[derive(Default)]
struct MockStorage {
    settings: Mutex<BTreeMap<String, String>>,
    backend: Mutex<Option<BackendConfig>>,
}

#[async_trait::async_trait]
impl ramaria_core::traits::StoreCrud for MockStorage {
    async fn create_session(
        &self,
        _p: Option<&str>,
    ) -> RamariaResult<ramaria_core::types::Session> {
        Err(ramaria_core::error::RamariaError::unsupported("mock"))
    }
    async fn close_session(&self, _id: uuid::Uuid) -> RamariaResult<()> {
        Err(ramaria_core::error::RamariaError::unsupported("mock"))
    }
    async fn get_session(
        &self,
        _id: uuid::Uuid,
    ) -> RamariaResult<Option<ramaria_core::types::Session>> {
        Ok(None)
    }
    async fn list_active_sessions(&self) -> RamariaResult<Vec<ramaria_core::types::Session>> {
        Ok(vec![])
    }
    async fn list_sessions(&self) -> RamariaResult<Vec<ramaria_core::types::Session>> {
        Ok(vec![])
    }
    async fn delete_session(&self, _id: uuid::Uuid) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_message(&self, _m: &ramaria_core::types::Message) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_messages(
        &self,
        _id: uuid::Uuid,
    ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
        Ok(vec![])
    }
    async fn list_messages_by_persona(
        &self,
        _p: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::Message>> {
        Ok(vec![])
    }
    async fn save_memory_l1(&self, _m: &ramaria_core::types::MemoryL1) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_memory_l1(
        &self,
        _id: uuid::Uuid,
    ) -> RamariaResult<Vec<ramaria_core::types::MemoryL1>> {
        Ok(vec![])
    }
    async fn get_memory_l1(
        &self,
        _id: uuid::Uuid,
    ) -> RamariaResult<Option<ramaria_core::types::MemoryL1>> {
        Ok(None)
    }
    async fn mark_l1_absorbed(&self, _ids: &[uuid::Uuid]) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_unabsorbed_l1(
        &self,
        _p: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::MemoryL1>> {
        Ok(vec![])
    }
    async fn create_persona(&self, _p: &ramaria_core::types::Persona) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn get_persona_by_uid(
        &self,
        _u: &str,
    ) -> RamariaResult<Option<ramaria_core::types::Persona>> {
        Ok(None)
    }
    async fn list_personas(&self) -> RamariaResult<Vec<ramaria_core::types::Persona>> {
        Ok(vec![])
    }
    async fn update_persona(
        &self,
        _u: &str,
        _n: &str,
        _a: Option<&str>,
        _c: Option<&str>,
        _d: Option<&str>,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_event(&self, _e: &ramaria_core::types::MemoryEvent) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_events_by_persona(
        &self,
        _p: &str,
        _o: i64,
        _l: i64,
    ) -> RamariaResult<Vec<ramaria_core::types::MemoryEvent>> {
        Ok(vec![])
    }
    async fn list_unabsorbed_events(
        &self,
        _p: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::MemoryEvent>> {
        Ok(vec![])
    }
    async fn mark_events_absorbed(&self, _ids: &[i64]) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_event_relation(
        &self,
        _r: &ramaria_core::types::EventRelation,
    ) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn save_event_source(&self, _e: i64, _l: uuid::Uuid, _w: f64) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_fact(&self, _f: &ramaria_core::types::PersonaFact) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_facts_by_persona(
        &self,
        _p: &str,
        _f: ramaria_core::types::ProfileField,
    ) -> RamariaResult<Vec<ramaria_core::types::PersonaFact>> {
        Ok(vec![])
    }
    async fn save_trait(&self, _t: &ramaria_core::types::PersonalityTrait) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_traits_by_persona(
        &self,
        _p: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::PersonalityTrait>> {
        Ok(vec![])
    }
    async fn update_trait_confidence(
        &self,
        _id: i64,
        _c: f64,
        _e: f64,
        _s: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn update_trait_status(
        &self,
        _id: i64,
        _s: ramaria_core::types::TraitStatus,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_evidence(&self, _e: &ramaria_core::types::TraitEvidence) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_evidence_by_trait(
        &self,
        _t: i64,
    ) -> RamariaResult<Vec<ramaria_core::types::TraitEvidence>> {
        Ok(vec![])
    }
    async fn save_example(&self, _e: &ramaria_core::types::PersonaExample) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn list_selected_examples(
        &self,
        _p: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::PersonaExample>> {
        Ok(vec![])
    }
    async fn save_cluster_snapshot(
        &self,
        _s: &ramaria_core::types::ClusterSnapshot,
    ) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn get_current_snapshots(
        &self,
        _p: &str,
        _c: &str,
    ) -> RamariaResult<Vec<ramaria_core::types::ClusterSnapshot>> {
        Ok(vec![])
    }
    async fn upsert_keyword(&self, _k: &str) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(vec![])
    }
}

#[async_trait::async_trait]
impl ramaria_core::traits::StoreInfrastructure for MockStorage {
    async fn insert_keyword_ref(
        &self,
        _k: &str,
        _d: &str,
        _i: &str,
        _p: &str,
        _w: f64,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn save_privacy_consent(
        &self,
        _c: &ramaria_core::types::PrivacyConsent,
    ) -> RamariaResult<()> {
        Ok(())
    }
    async fn get_privacy_consent(
        &self,
        _p: &str,
        _b: &str,
    ) -> RamariaResult<Option<ramaria_core::types::PrivacyConsent>> {
        Ok(None)
    }
    async fn save_backend_config(&self, c: &BackendConfig) -> RamariaResult<()> {
        *self.backend.lock().unwrap() = Some(c.clone());
        Ok(())
    }
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        Ok(self.backend.lock().unwrap().clone())
    }
    async fn get_schema_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }
    async fn get_index_version(&self) -> RamariaResult<i32> {
        Ok(1)
    }
    async fn set_index_version(&self, _v: i32) -> RamariaResult<()> {
        Ok(())
    }
    async fn create_background_job(&self, _t: &str, _p: Option<&str>) -> RamariaResult<i64> {
        Ok(1)
    }
    async fn update_job_status(&self, _i: i64, _s: &str, _e: Option<&str>) -> RamariaResult<()> {
        Ok(())
    }
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>> {
        Ok(vec![])
    }
    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>> {
        Ok(self.settings.lock().unwrap().get(key).cloned())
    }
    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()> {
        self.settings
            .lock()
            .unwrap()
            .insert(key.to_string(), value.to_string());
        Ok(())
    }
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>> {
        Ok(self
            .settings
            .lock()
            .unwrap()
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect())
    }
}

/// 创建临时目录 + 用例实例。
fn temp_service(storage: Arc<dyn StorageBackend>) -> (ConfigWriter, PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "ramaria-config-writer-test-{}",
        uuid::Uuid::new_v4()
    ));
    let path = dir.join("config.toml");
    (ConfigWriter::new(storage, path.clone()), dir)
}

#[tokio::test]
async fn load_missing_file_generates_template() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    let outcome = service.load().await.expect("加载不应失败");

    // 文件缺失 → 生成模板 + 默认配置
    assert!(!outcome.file_existed);
    assert!(outcome.file_parse_errors.is_empty());
    assert!(service.config_path().exists(), "应生成模板文件");
    assert!(outcome.config.utt.enabled);
    assert_eq!(outcome.config.utt.theta_gap_minutes, 10);
    assert_eq!(outcome.config.examples.max_examples, 5);
    assert!(outcome.config.bridge.enabled);
    // DB 无差异（均为默认）→ 无 mismatch
    assert!(outcome.mismatches.is_empty());

    // 生成的模板应能反序列化回默认配置
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let parsed: RamariaConfig = toml::from_str(&text).expect("模板应为合法 TOML");
    assert!(parsed.utt.enabled);
    assert_eq!(parsed.utt.persona_kind_whitelist.len(), 4);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn first_startup_preserves_existing_db_backend() {
    // 文件缺失（无 config.toml）且 DB 中已有用户后端配置时，
    // 加载必须以 DB 为准，绝不能以默认值覆盖用户配置。
    let storage = Arc::new(MockStorage::default());
    let bc = BackendConfig::deepseek_default();
    storage.save_backend_config(&bc).await.unwrap();
    // DB 侧还有自定义设置
    storage
        .set_setting("config.utt.theta_gap_minutes", "45")
        .await
        .unwrap();

    let (service, dir) = temp_service(storage.clone());
    let outcome = service.load().await.unwrap();

    // 生效配置 = DB 侧值（不被默认值覆盖）
    assert_eq!(outcome.config.backend.provider, LlmProvider::DeepSeek);
    assert_eq!(outcome.config.backend.model_id, "deepseek-chat");
    assert_eq!(outcome.config.utt.theta_gap_minutes, 45);
    // 首启无 mismatch（不以文件为准回写 DB）
    assert!(outcome.mismatches.is_empty(), "首启不应产生 mismatch");

    // DB 侧未被覆盖（仍为 DeepSeek）
    let db_backend = storage.get_backend_config().await.unwrap().unwrap();
    assert_eq!(db_backend.provider, LlmProvider::DeepSeek);
    assert_eq!(db_backend.capability.model_id, "deepseek-chat");
    // settings 键未被覆盖
    let v = storage
        .get_setting("config.utt.theta_gap_minutes")
        .await
        .unwrap();
    assert_eq!(v.as_deref(), Some("45"));

    // 生成的文件应包含 DB 侧值（下次加载以文件为准时不会漂移）
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);
    assert_eq!(file_cfg.utt.theta_gap_minutes, 45);

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn corrupted_file_does_not_overwrite_db() {
    // 文件损坏 → 以 DB 为准合并，不向 DB 回写默认值（防止覆盖用户配置）
    let storage = Arc::new(MockStorage::default());
    let bc = BackendConfig::openai_default();
    storage.save_backend_config(&bc).await.unwrap();

    let (service, dir) = temp_service(storage.clone());
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&service.config_path, "损坏的 [[[ 配置").unwrap();

    let outcome = service.load().await.unwrap();
    assert_eq!(outcome.file_parse_errors.len(), 1);
    // 生效配置以 DB 为准
    assert_eq!(outcome.config.backend.provider, LlmProvider::OpenAI);
    // 不写回 DB（DB 仍是 OpenAI）
    let db_backend = storage.get_backend_config().await.unwrap().unwrap();
    assert_eq!(db_backend.provider, LlmProvider::OpenAI);
    // 损坏文件不被覆盖（保留现场）
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert!(text.contains("损坏"), "损坏文件应原样保留");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn load_file_wins_over_db_mismatch() {
    // 文件已存在且 DB 侧值不同 → 检测 mismatch 并回写 DB（以文件为准）
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    // 先首启生成文件（默认值 theta=10）
    service.load().await.unwrap();

    // 模拟外部修改 DB 侧值（绕过统一写入口直写 settings 表）
    storage
        .set_setting("config.utt.theta_gap_minutes", "60")
        .await
        .unwrap();

    // 再次加载：文件（10）vs DB（60）→ mismatch → 以文件为准写回 DB
    let outcome = service.load().await.unwrap();
    assert!(
        outcome
            .mismatches
            .iter()
            .any(|m| m.key == "config.utt.theta_gap_minutes"),
        "应检出 theta_gap_minutes 不一致: {:?}",
        outcome.mismatches
    );

    // DB 被回写为文件值 10
    let db_val = storage
        .get_setting("config.utt.theta_gap_minutes")
        .await
        .unwrap();
    assert_eq!(db_val.as_deref(), Some("10"), "DB 应以文件为准回写");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn load_merges_db_backend_config() {
    // 首启（无 config.toml）且 backend_config 表有值 → 以 DB 为准合并
    let storage = Arc::new(MockStorage::default());
    let bc = BackendConfig::deepseek_default();
    storage.save_backend_config(&bc).await.unwrap();
    let (service, dir) = temp_service(storage);

    let outcome = service.load().await.unwrap();
    assert_eq!(outcome.config.backend.provider, LlmProvider::DeepSeek);
    assert_eq!(outcome.config.backend.model_id, "deepseek-chat");
    // 首启以 DB 为准：不产生 mismatch、不覆盖 DB
    assert!(
        outcome.mismatches.is_empty(),
        "首启场景不应检出 mismatch: {:?}",
        outcome.mismatches
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn load_parse_error_falls_back_to_defaults() {
    // 文件损坏 → 解析失败回退默认值，不阻塞
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&service.config_path, "这不是合法的 TOML [[[").unwrap();

    let outcome = service.load().await.unwrap();
    assert_eq!(outcome.file_parse_errors.len(), 1, "应记录解析错误");
    assert!(outcome.config.utt.enabled, "应回退默认配置");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn save_config_writes_both_sides() {
    // 统一写入口：文件 + settings + backend_config 三处一致
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    let mut cfg = RamariaConfig::default();
    cfg.utt.theta_gap_minutes = 45;
    cfg.utt.enabled = false;
    cfg.examples.max_examples = 3;
    cfg.backend.provider = LlmProvider::DeepSeek;
    cfg.backend.model_id = "deepseek-chat".to_string();

    let result = service.save_config(&cfg).await;
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

    // 文件侧
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert_eq!(file_cfg.utt.theta_gap_minutes, 45);
    assert!(!file_cfg.utt.enabled);
    assert_eq!(file_cfg.examples.max_examples, 3);
    assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);

    // settings 表侧
    let v = storage
        .get_setting("config.utt.theta_gap_minutes")
        .await
        .unwrap();
    assert_eq!(v.as_deref(), Some("45"));
    let v = storage
        .get_setting("config.examples.max_examples")
        .await
        .unwrap();
    assert_eq!(v.as_deref(), Some("3"));
    // 数组（白名单）以 JSON 存储
    let v = storage
        .get_setting("config.utt.persona_kind_whitelist")
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_str(&v.unwrap()).unwrap();
    assert_eq!(parsed.as_array().unwrap().len(), 4);

    // backend_config 表侧
    let bc = storage.get_backend_config().await.unwrap().unwrap();
    assert_eq!(bc.provider, LlmProvider::DeepSeek);
    assert_eq!(bc.capability.model_id, "deepseek-chat");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn save_then_load_roundtrip() {
    // 热更新语义：save 后 load 应读到相同值
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);

    let mut cfg = RamariaConfig::default();
    cfg.utt.theta_gap_minutes = 55;
    cfg.bridge.enabled = false;
    cfg.session.l1_idle_minutes = 25;
    service.save_config(&cfg).await;

    let outcome = service.load().await.unwrap();
    assert_eq!(outcome.config.utt.theta_gap_minutes, 55);
    assert!(!outcome.config.bridge.enabled);
    assert_eq!(outcome.config.session.l1_idle_minutes, 25);
    assert!(
        outcome.mismatches.is_empty(),
        "save 后 load 不应再有 mismatch: {:?}",
        outcome.mismatches
    );

    let _ = std::fs::remove_dir_all(dir);
}

/// 历史窗口键纳入双写与逐键比对：save 后 settings 表可见、文件侧携带，加载读回一致。
#[tokio::test]
async fn max_history_chars_is_covered_by_full_sync() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    let mut cfg = RamariaConfig::default();
    cfg.session.max_history_chars = 9000;
    let result = service.save_config(&cfg).await;
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

    // settings 表：键存在且值一致
    let stored = storage
        .get_setting("config.session.max_history_chars")
        .await
        .unwrap();
    assert_eq!(stored.as_deref(), Some("9000"), "键应写入 settings");

    // 文件侧：[session] 组含该键且值一致
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert!(
        text.contains("max_history_chars"),
        "文件应携带该键:\n{text}"
    );
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert_eq!(file_cfg.session.max_history_chars, 9000);

    // 加载读回一致（双写完成后无不一致项）
    let outcome = service.load().await.unwrap();
    assert_eq!(outcome.config.session.max_history_chars, 9000);
    assert!(
        outcome.mismatches.is_empty(),
        "save 后 load 不应再有 mismatch: {:?}",
        outcome.mismatches
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn save_preserves_existing_backend_embedding_path() {
    // 写回 backend_config 表时保留既有 embedding_model_path / capability
    let storage = Arc::new(MockStorage::default());
    let mut existing = BackendConfig::lm_studio_default();
    existing.embedding_model_path = Some("D:/models/bge".to_string());
    storage.save_backend_config(&existing).await.unwrap();
    let (service, dir) = temp_service(storage.clone());

    let cfg = RamariaConfig::default(); // 文件侧 LM Studio
    let result = service.save_config(&cfg).await;
    assert!(result.is_ok());

    let bc = storage.get_backend_config().await.unwrap().unwrap();
    assert_eq!(
        bc.embedding_model_path.as_deref(),
        Some("D:/models/bge"),
        "embedding_model_path 不应被覆盖丢失"
    );

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn flat_map_roundtrip_preserves_types() {
    // 扁平化 → 合并回配置：类型保持一致（数字/布尔/数组/字符串）
    let cfg = RamariaConfig::default();
    let flat = config_to_flat_map(&cfg);

    // 关键键存在
    assert!(flat.contains_key("utt.theta_gap_minutes"));
    assert!(flat.contains_key("utt.enabled"));
    assert!(flat.contains_key("utt.persona_kind_whitelist"));
    assert!(flat.contains_key("session.l1_idle_minutes"));
    assert!(flat.contains_key("session.max_history_chars"));
    // 跳过组不在扁平 map 中
    assert!(!flat.contains_key("version"));
    assert!(!flat.contains_key("paths.data_dir"));
    assert!(!flat.contains_key("backend.provider"));

    let back = flat_map_to_config(&flat, &RamariaConfig::default()).unwrap();
    assert_eq!(back.utt.theta_gap_minutes, cfg.utt.theta_gap_minutes);
    assert_eq!(back.utt.enabled, cfg.utt.enabled);
    assert_eq!(
        back.utt.persona_kind_whitelist,
        cfg.utt.persona_kind_whitelist
    );
    assert_eq!(back.session.l1_idle_minutes, cfg.session.l1_idle_minutes);
    assert_eq!(
        back.session.max_history_chars,
        cfg.session.max_history_chars
    );
    // backend 组未被扁平 map 覆盖 → 保持基础值
    assert_eq!(back.backend.provider, cfg.backend.provider);
}

#[tokio::test]
async fn flat_map_ignores_unknown_keys() {
    // 未知键（当前 schema 不存在）合并时忽略，不报错
    let mut flat: BTreeMap<String, JsonValue> = BTreeMap::new();
    flat.insert("utt.theta_gap_minutes".to_string(), JsonValue::from(42));
    flat.insert("future.key".to_string(), JsonValue::from("x"));

    let back = flat_map_to_config(&flat, &RamariaConfig::default()).unwrap();
    assert_eq!(back.utt.theta_gap_minutes, 42);
    assert!(back.utt.enabled, "未涉及的键保持默认");
}

#[test]
fn whitelist_serializes_to_settings() {
    // 白名单数组 → settings 文本 → 读回可还原
    let cfg = RamariaConfig::default();
    let flat = config_to_flat_map(&cfg);
    let v = flat.get("utt.persona_kind_whitelist").unwrap();
    let text = json_value_to_setting(v);
    let parsed: serde_json::Value = serde_json::from_str(&text).unwrap();
    let kinds: Vec<PersonaKind> = serde_json::from_value(parsed).unwrap();
    assert_eq!(kinds.len(), 4);
    assert!(kinds.contains(&PersonaKind::Char));
    assert!(!kinds.contains(&PersonaKind::Rama));
}

#[tokio::test]
async fn sync_backend_config_updates_file_only() {
    // 后端配置更新用例：写表后同步文件（仅文件侧）
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    // 先有文件配置（默认）
    service.load().await.unwrap();

    let bc = BackendConfig::deepseek_default();
    let result = service.sync_backend_config(&bc).await;
    assert!(result.is_ok());

    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert_eq!(file_cfg.backend.provider, LlmProvider::DeepSeek);
    assert_eq!(file_cfg.backend.model_id, "deepseek-chat");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn sync_backend_config_corrupted_file_is_not_overwritten() {
    // 文件损坏时 sync_backend_config 必须拒绝覆盖（保留现场，防止毁掉用户文件）
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(&service.config_path, "损坏的 [[[ 配置").unwrap();

    let bc = BackendConfig::deepseek_default();
    let result = service.sync_backend_config(&bc).await;
    assert!(!result.file_ok, "损坏文件应拒绝同步");
    assert!(
        !result.failures.is_empty(),
        "应返回失败明细: {:?}",
        result.failures
    );

    // 文件原样保留（未被默认值或 merged 覆盖）
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert_eq!(text, "损坏的 [[[ 配置", "损坏文件应原样保留");

    let _ = std::fs::remove_dir_all(dir);
}

#[tokio::test]
async fn default_template_contains_expected_groups() {
    // 模板文件包含主要配置组（[utt] / [examples] / [bridge]）
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    service.load().await.unwrap();

    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert!(text.contains("[utt]"), "模板应含 [utt]");
    assert!(text.contains("theta_gap_minutes"));
    assert!(text.contains("[examples]"));
    assert!(text.contains("max_examples"));
    assert!(text.contains("[bridge]"));
    assert!(text.contains("persona_kind_whitelist"));
    assert!(text.contains("[vision]"), "模板应含 [vision]");
    assert!(text.contains("model_supports_vision"));
    assert!(text.contains("batch_limit"));

    let _ = std::fs::remove_dir_all(dir);
}

/// 默认配置模板必须与 `RamariaConfig::default()` 逐键一致。
///
/// 背景:
/// - 首启生成的 config.toml 直接复制本模板（`DEFAULT_CONFIG_TEMPLATE`），
///   模板缺段会让用户无法从文件发现新增配置开关（静默取结构默认）。
/// - 本用例按扁平键（跳过 version/schema_version/paths/backend）逐键比对
///   模板解析值与结构默认值；新增配置组时必须同步模板。
#[test]
fn default_template_matches_config_defaults_key_by_key() {
    let template: RamariaConfig =
        toml::from_str(DEFAULT_CONFIG_TEMPLATE).expect("模板应为合法 TOML");
    let template_flat = config_to_flat_map(&template);
    let default_flat = config_to_flat_map(&RamariaConfig::default());

    let missing: Vec<&String> = default_flat
        .keys()
        .filter(|key| !template_flat.contains_key(*key))
        .collect();
    assert!(missing.is_empty(), "模板缺少默认配置键: {missing:?}");

    let unknown: Vec<&String> = template_flat
        .keys()
        .filter(|key| !default_flat.contains_key(*key))
        .collect();
    assert!(
        unknown.is_empty(),
        "模板含当前 schema 不存在的键: {unknown:?}"
    );

    let drifted: Vec<String> = default_flat
        .iter()
        .filter_map(|(key, default_value)| {
            let template_value = template_flat.get(key)?;
            (template_value != default_value)
                .then(|| format!("{key}: 模板={template_value} 默认={default_value}"))
        })
        .collect();
    assert!(
        drifted.is_empty(),
        "模板默认值与结构默认值不一致: {drifted:?}"
    );
}

#[tokio::test]
async fn first_startup_empty_db_keeps_commented_template() {
    // 首启且 DB 为空 → 文件内容 = 带注释的默认模板（保留说明注释）
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    service.load().await.unwrap();

    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert_eq!(
        text, DEFAULT_CONFIG_TEMPLATE,
        "DB 为空时首启应保留带注释的模板原文"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// 同步 diff 只认"文件显式声明的键"（文件缺键不覆盖 DB 显式值）
// =========================================================

/// 文件缺键时，DB 中用户显式设置不得被文件默认值覆盖。
#[tokio::test]
async fn load_does_not_overwrite_db_keys_absent_from_file() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());
    std::fs::create_dir_all(&dir).unwrap();

    // 文件只显式声明 utt.theta_gap_minutes；其余键走反序列化默认值
    std::fs::write(&service.config_path, "[utt]\ntheta_gap_minutes = 10\n").unwrap();

    // DB 侧：theta 与文件不同（文件为准）；examples.max_examples 文件未声明（必须保留）
    storage
        .set_setting("config.utt.theta_gap_minutes", "45")
        .await
        .unwrap();
    storage
        .set_setting("config.examples.max_examples", "3")
        .await
        .unwrap();
    // DB 后端为 DeepSeek，文件无 [backend] → 后端配置必须保留（不被默认 LM Studio 覆盖）
    let bc = BackendConfig::deepseek_default();
    storage.save_backend_config(&bc).await.unwrap();

    let outcome = service.load().await.unwrap();

    // 文件显式键：以文件为准回写 DB，并记入 mismatch
    let theta = storage
        .get_setting("config.utt.theta_gap_minutes")
        .await
        .unwrap();
    assert_eq!(theta.as_deref(), Some("10"), "文件显式键应以文件为准");
    assert!(
        outcome
            .mismatches
            .iter()
            .any(|m| m.key == "config.utt.theta_gap_minutes"),
        "文件显式键不一致应记入 mismatch"
    );

    // 文件未声明的键：DB 显式值保留（默认值为 5，不得覆盖）
    let max_examples = storage
        .get_setting("config.examples.max_examples")
        .await
        .unwrap();
    assert_eq!(
        max_examples.as_deref(),
        Some("3"),
        "文件未声明的键不得被默认值覆盖"
    );

    // 文件未声明 [backend] → DB 后端配置保留
    let db_backend = storage.get_backend_config().await.unwrap().unwrap();
    assert_eq!(
        db_backend.provider,
        LlmProvider::DeepSeek,
        "文件未声明 [backend] 时后端配置应保留"
    );

    // 生效配置以文件为准
    assert_eq!(outcome.config.utt.theta_gap_minutes, 10);

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// [mcp] 组纳入双写同步（MCP 接入配置通道）
// =========================================================

/// [mcp] 组七键纳入统一写入口：save 后 settings 表逐键可见、文件侧携带其值。
#[tokio::test]
async fn mcp_group_keys_are_covered_by_full_sync() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    let mut cfg = RamariaConfig::default();
    cfg.mcp.enabled = true;
    cfg.mcp.allow_ingest = false;
    cfg.mcp.allow_seal = false;
    cfg.mcp.allowed_personas = vec!["rama-0001".to_string()];
    cfg.mcp.allow_raw_text = true;
    cfg.mcp.max_items = 8;
    cfg.mcp.max_chars = 999;

    let result = service.save_config(&cfg).await;
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

    // settings 表侧：标量键逐键断言（新增配置组必须自动纳入扁平化覆盖）
    for (key, want) in [
        ("config.mcp.enabled", "true"),
        ("config.mcp.allow_ingest", "false"),
        ("config.mcp.allow_seal", "false"),
        ("config.mcp.allow_raw_text", "true"),
        ("config.mcp.max_items", "8"),
        ("config.mcp.max_chars", "999"),
    ] {
        let got = storage.get_setting(key).await.unwrap();
        assert_eq!(got.as_deref(), Some(want), "键 {key} 应写入 settings");
    }
    // 白名单数组以 JSON 文本存储
    let personas = storage
        .get_setting("config.mcp.allowed_personas")
        .await
        .unwrap()
        .expect("白名单键应写入 settings");
    let parsed: JsonValue = serde_json::from_str(&personas).unwrap();
    assert_eq!(parsed, serde_json::json!(["rama-0001"]));

    // 文件侧：完整序列化应携带 [mcp] 全组
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert!(file_cfg.mcp.enabled);
    assert!(!file_cfg.mcp.allow_seal);
    assert_eq!(file_cfg.mcp.max_items, 8);
    assert_eq!(file_cfg.mcp.allowed_personas, vec!["rama-0001".to_string()]);

    let _ = std::fs::remove_dir_all(dir);
}

/// 文件与 DB 的 [mcp] 键不一致：以文件为准回写 DB 并记入 mismatch（既有不一致处理口径）。
#[tokio::test]
async fn mcp_mismatch_is_resolved_by_file_side() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());
    std::fs::create_dir_all(&dir).unwrap();

    // 文件显式声明 [mcp].enabled = true（其余键走反序列化默认值）
    std::fs::write(&service.config_path, "[mcp]\nenabled = true\n").unwrap();
    // DB 侧为相反残值 → 应被文件回写
    storage
        .set_setting("config.mcp.enabled", "false")
        .await
        .unwrap();

    let outcome = service.load().await.unwrap();

    let enabled = storage.get_setting("config.mcp.enabled").await.unwrap();
    assert_eq!(
        enabled.as_deref(),
        Some("true"),
        "文件显式键应以文件为准回写 DB"
    );
    assert!(
        outcome
            .mismatches
            .iter()
            .any(|m| m.key == "config.mcp.enabled"),
        "不一致应记入 mismatch: {:?}",
        outcome.mismatches
    );
    assert!(outcome.config.mcp.enabled, "生效配置应取文件侧值");

    let _ = std::fs::remove_dir_all(dir);
}

/// 文件未声明的 [mcp] 键：DB 显式值保留（不被模板默认值覆盖）。
#[tokio::test]
async fn mcp_db_keys_absent_from_file_are_preserved() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());
    std::fs::create_dir_all(&dir).unwrap();

    // 文件只声明 [mcp].enabled（其余键走反序列化默认值）
    std::fs::write(&service.config_path, "[mcp]\nenabled = true\n").unwrap();
    // DB 侧用户显式设置：max_items=9（文件未声明 → 不得被默认值 5 覆盖）
    storage
        .set_setting("config.mcp.max_items", "9")
        .await
        .unwrap();

    service.load().await.unwrap();

    let max_items = storage.get_setting("config.mcp.max_items").await.unwrap();
    assert_eq!(
        max_items.as_deref(),
        Some("9"),
        "文件未声明的键不得被默认值覆盖"
    );

    let _ = std::fs::remove_dir_all(dir);
}

// =========================================================
// [proactive] 组纳入双写同步（主动对话配置通道）
// =========================================================

/// [proactive] 组二十二键纳入统一写入口：save 后 settings 表逐键可见、文件侧携带其值。
#[tokio::test]
async fn proactive_group_keys_are_covered_by_full_sync() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage.clone());

    let mut cfg = RamariaConfig::default();
    cfg.proactive.enabled = false;
    cfg.proactive.check_interval_seconds = 120;
    cfg.proactive.min_idle_hours = 6;
    cfg.proactive.daily_limit = 2;
    cfg.proactive.daily_total_limit = 5;
    cfg.proactive.quiet_hours = "23:00-07:30".to_string();
    cfg.proactive.cooldown_hours = 24;
    cfg.proactive.judge_enabled = false;
    cfg.proactive.judge_interval_hours = 6;
    cfg.proactive.active_hours_weight = 0.5;
    cfg.proactive.active_hours_window_days = 14;
    cfg.proactive.active_hours_min_samples = 20;
    cfg.proactive.valence_weight = 0.8;
    cfg.proactive.confidence_floor = 0.7;
    cfg.proactive.light_touch_weight = 0.2;
    cfg.proactive.event_salience_threshold = 0.7;
    cfg.proactive.event_window_days = 7;
    cfg.proactive.unresolved_valence_threshold = -0.4;
    cfg.proactive.follow_up_days = 5;
    cfg.proactive.topic_cooldown_hours = 48;
    cfg.proactive.silence_backoff_days = 5;
    cfg.proactive.startup_grace_days = 1;

    let result = service.save_config(&cfg).await;
    assert!(result.is_ok(), "双侧写入应成功: {:?}", result.failures);

    // settings 表侧：标量键逐键断言（新增配置组必须自动纳入扁平化覆盖）
    for (key, want) in [
        ("config.proactive.enabled", "false"),
        ("config.proactive.check_interval_seconds", "120"),
        ("config.proactive.min_idle_hours", "6"),
        ("config.proactive.daily_limit", "2"),
        ("config.proactive.daily_total_limit", "5"),
        ("config.proactive.cooldown_hours", "24"),
        ("config.proactive.judge_enabled", "false"),
        ("config.proactive.judge_interval_hours", "6"),
        ("config.proactive.active_hours_weight", "0.5"),
        ("config.proactive.active_hours_window_days", "14"),
        ("config.proactive.active_hours_min_samples", "20"),
        ("config.proactive.valence_weight", "0.8"),
        ("config.proactive.confidence_floor", "0.7"),
        ("config.proactive.light_touch_weight", "0.2"),
        ("config.proactive.event_salience_threshold", "0.7"),
        ("config.proactive.event_window_days", "7"),
        ("config.proactive.unresolved_valence_threshold", "-0.4"),
        ("config.proactive.follow_up_days", "5"),
        ("config.proactive.topic_cooldown_hours", "48"),
        ("config.proactive.silence_backoff_days", "5"),
        ("config.proactive.startup_grace_days", "1"),
    ] {
        let got = storage.get_setting(key).await.unwrap();
        assert_eq!(got.as_deref(), Some(want), "键 {key} 应写入 settings");
    }
    // 字符串键以 JSON 引号文本存储（与既有扁平化口径一致）
    let quiet = storage
        .get_setting("config.proactive.quiet_hours")
        .await
        .unwrap()
        .expect("免打扰时段键应写入 settings");
    assert_eq!(quiet, "\"23:00-07:30\"");

    // 文件侧：完整序列化应携带 [proactive] 全组
    let text = std::fs::read_to_string(service.config_path()).unwrap();
    let file_cfg: RamariaConfig = toml::from_str(&text).unwrap();
    assert!(!file_cfg.proactive.enabled);
    assert_eq!(file_cfg.proactive.quiet_hours, "23:00-07:30");
    assert_eq!(file_cfg.proactive.daily_limit, 2);
    assert_eq!(file_cfg.proactive.daily_total_limit, 5);

    let _ = std::fs::remove_dir_all(dir);
}

/// 保存配置时保留文件头注释与未知键（全量序列化不丢用户手写内容）。
#[tokio::test]
async fn save_config_preserves_header_comments_and_unknown_keys() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        &service.config_path,
        "# 用户手写说明：不要删我\n# 第二行注释\n\n\
         [utt]\ntheta_gap_minutes = 10\nfuture_key = \"keep-me\"\n\n\
         [future_group]\nx = 1\n",
    )
    .unwrap();

    let mut cfg = RamariaConfig::default();
    cfg.utt.theta_gap_minutes = 25;
    let result = service.save_config(&cfg).await;
    assert!(result.file_ok, "文件写入应成功: {:?}", result.failures);

    let text = std::fs::read_to_string(service.config_path()).unwrap();
    assert!(
        text.starts_with("# 用户手写说明：不要删我\n# 第二行注释\n"),
        "文件头注释应保留:\n{text}"
    );
    assert!(
        text.contains("future_key = \"keep-me\""),
        "未知键应保留:\n{text}"
    );
    assert!(text.contains("[future_group]"), "未知分组应保留:\n{text}");
    assert!(
        !dir.join("config.toml.part").exists(),
        "成功后不应残留临时文件"
    );

    // 合并结果仍是合法 TOML 且写入值生效
    let parsed: RamariaConfig = toml::from_str(&text).expect("合并后应为合法 TOML");
    assert_eq!(parsed.utt.theta_gap_minutes, 25);

    let _ = std::fs::remove_dir_all(dir);
}

/// 原子写入失败（目标被目录占位）→ 返回错误、无 `.part` 残留、原目标保持原样。
#[tokio::test]
async fn write_file_config_failure_leaves_no_part_file() {
    let storage = Arc::new(MockStorage::default());
    let (service, dir) = temp_service(storage);
    std::fs::create_dir_all(service.config_path()).unwrap(); // 目标位置放目录 → rename 必失败

    let err = service
        .write_file_config(&RamariaConfig::default())
        .expect_err("目标为目录时写入应失败");
    assert!(!err.to_string().is_empty(), "错误信息不应为空");
    assert!(
        !dir.join("config.toml.part").exists(),
        "失败后不应残留 .part 临时文件"
    );
    assert!(service.config_path().is_dir(), "原目标应保持不变");

    let _ = std::fs::remove_dir_all(dir);
}
