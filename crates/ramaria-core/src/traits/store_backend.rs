//! crates/ramaria-core/src/traits/store_backend.rs - Ramaria 存储后端基础设施抽象模块
//!
//! 设计特点:
//! - 承载关键词倒排、隐私确认、后端配置、schema/索引版本与后台任务队列
//! - 覆盖全局设置、L2 聚类去重指纹、事件去重查询与行为规则/反馈日志
//! - 与 StoreCrud 分离，避免单一巨 trait 承载全部表能力
//! - 未接线方法显式返回 Unsupported，存量 mock 走默认实现即可编译
//! - StorageBackend 聚合两个子 trait，由 blanket impl 自动满足

use async_trait::async_trait;

use super::store_crud::StoreCrud;
use super::store_version::{
    BM25_INDEX_VERSION_LEGACY, IndexCorpusStamp, SETTING_BM25_INDEX_VERSION,
};
use crate::behavior::{BehaviorRule, FeedbackLog};
use crate::error::RamariaResult;
use crate::types::{BackendConfig, MemoryEvent, PrivacyConsent};

/// 存储后端抽象 trait（基础设施/系统分组）。
///
/// 职责:
/// - 承载关键词倒排索引、隐私确认、后端配置、schema/索引版本、后台任务队列、全局设置、
///   L2 聚类去重指纹、事件去重查询、行为规则与反馈日志等非核心业务对象的表能力。
/// - 与 [`StoreCrud`]（核心业务 CRUD）分离，避免单一巨 trait。
///
/// 实现要求:
/// - 具体实现位于 `ramaria-storage`。
/// - 所有可恢复错误应转换为 `RamariaError::Storage` 或更精确分类。
#[async_trait]
pub trait StoreInfrastructure: Send + Sync {
    // -- Keyword Refs --
    /// 插入一条关键词引用记录。
    async fn insert_keyword_ref(
        &self,
        keyword_id: &str,
        doc_type: &str,
        doc_id: &str,
        persona_uid: &str,
        weight: f64,
    ) -> RamariaResult<()>;

    // -- Privacy Consent --
    async fn save_privacy_consent(&self, consent: &PrivacyConsent) -> RamariaResult<()>;
    async fn get_privacy_consent(
        &self,
        provider: &str,
        base_url: &str,
    ) -> RamariaResult<Option<PrivacyConsent>>;

    // -- Backend Config --
    async fn save_backend_config(&self, config: &BackendConfig) -> RamariaResult<()>;
    async fn get_backend_config(&self) -> RamariaResult<Option<BackendConfig>>;

    // -- 索引一致性 --
    async fn get_schema_version(&self) -> RamariaResult<i32>;
    async fn get_index_version(&self) -> RamariaResult<i32>;
    async fn set_index_version(&self, version: i32) -> RamariaResult<()>;

    /// 读取记忆语料统计戳（跨进程索引刷新检测）。
    ///
    /// 用途:
    /// - 长驻进程（MCP 服务端等）在召回前比对"库内语料是否变化"，
    ///   决定是否刷新内存检索索引（其他进程写入后新记忆不能漏检索）。
    ///
    /// 返回:
    /// - `Ok(Some(stamp))`: 存储提供统计（SQLite 后端）。
    /// - `Ok(None)`: 后端不提供统计（内存 / mock），调用方回退同进程脏标记语义。
    ///
    /// 说明:
    /// - 查询为常数级聚合（COUNT / MAX），可承受每次召回前调用。
    async fn index_corpus_stamp(&self) -> RamariaResult<Option<IndexCorpusStamp>> {
        Ok(None)
    }

    // -- Background Jobs --
    async fn create_background_job(
        &self,
        job_type: &str,
        payload: Option<&str>,
    ) -> RamariaResult<i64>;
    async fn update_job_status(
        &self,
        id: i64,
        status: &str,
        error: Option<&str>,
    ) -> RamariaResult<()>;
    async fn list_pending_jobs(&self) -> RamariaResult<Vec<(i64, String, Option<String>)>>;

    /// 原子抢占 pending 任务（`pending` → `running`）。
    ///
    /// 用途:
    /// - 多个消费方（桌面生命周期线程 / MCP 宿主空闲检查）并发补扫同一批任务时，
    ///   只有抢占成功者执行该任务，避免重复加工（如重复生成同一份 L1 摘要）。
    ///
    /// 参数:
    /// - `id`: 任务 id。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次调用抢占成功（调用方应执行该任务）。
    /// - `Ok(false)`: 任务已被其他调用方抢占 / 已不在 pending（调用方应跳过）。
    ///
    /// 默认实现:
    /// - 退化为「无条件置 running 并视为抢占成功」——不具备并发去重语义，
    ///   仅适用于单消费方或测试 mock；SQLite 后端覆写为条件更新（`status = 'pending'` 才生效）。
    async fn claim_pending_job(&self, id: i64) -> RamariaResult<bool> {
        self.update_job_status(id, "running", None).await?;
        Ok(true)
    }

    // -- Settings --
    async fn get_setting(&self, key: &str) -> RamariaResult<Option<String>>;
    async fn set_setting(&self, key: &str, value: &str) -> RamariaResult<()>;
    async fn list_settings(&self) -> RamariaResult<Vec<(String, String)>>;

    // -- BM25 词典增强分词版本 --
    /// 读取 BM25 分词版本（settings 键 `bm25_index_version`）。
    ///
    /// 说明:
    /// - 键缺失 / 值不可解析均视为旧版本 [`BM25_INDEX_VERSION_LEGACY`]（=1），
    ///   表示索引仍为纯 bigram 口径，需要迁移到词典增强。
    async fn get_bm25_index_version(&self) -> RamariaResult<i32> {
        Ok(self
            .get_setting(SETTING_BM25_INDEX_VERSION)
            .await?
            .and_then(|raw| raw.parse::<i32>().ok())
            .unwrap_or(BM25_INDEX_VERSION_LEGACY))
    }

    /// 写入 BM25 分词版本（settings 键 `bm25_index_version`）。
    async fn set_bm25_index_version(&self, version: i32) -> RamariaResult<()> {
        self.set_setting(SETTING_BM25_INDEX_VERSION, &version.to_string())
            .await
    }

    // =========================================================
    // L2 聚类去重指纹
    // =========================================================
    //
    // 记录"已聚类且无产出"的 L1 集合指纹（SHA-256 集合指纹），
    // 同集合未吸收 L1 不重复聚类；集合变更（新 L1 加入）后指纹变化自动重聚类。
    // 默认实现返回"不存在/不记录"，存量 mock 无需改动即可编译。

    /// 判断指定 persona 是否已记录过该 L1 集合指纹。
    ///
    /// 默认实现返回 `Ok(false)`（未记录，正常聚类）。
    async fn l2_fingerprint_exists(
        &self,
        _persona_uid: &str,
        _fingerprint: &str,
    ) -> RamariaResult<bool> {
        Ok(false)
    }

    /// 记录一次"已聚类且无产出"的 L1 集合指纹。
    ///
    /// 默认实现返回 `Ok(())`（不持久化，仅保证可编译）。
    async fn save_l2_fingerprint(
        &self,
        _persona_uid: &str,
        _fingerprint: &str,
    ) -> RamariaResult<()> {
        Ok(())
    }

    // -- 事件去重（L2 维护） --
    /// 查询 persona 最近的事件（供新事件相似度去重比对）。
    ///
    /// 参数:
    /// - `persona_uid`: 目标人格。
    /// - `limit`: 最多返回条数。
    ///
    /// 默认实现返回 `Unsupported`：显式区分"未接线"与"确实无事件"，
    /// 调用方（事件相似度去重 / 事实互证）拿到错误后按未接线降级并记日志。
    async fn list_recent_events(
        &self,
        _persona_uid: &str,
        _limit: u32,
    ) -> RamariaResult<Vec<MemoryEvent>> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现最近事件查询（事件去重比对需覆写）",
        ))
    }

    // =========================================================
    // 行为规则
    // =========================================================
    //
    // behavior_rules 表 CRUD 与 enabled 过滤。默认实现返回 Unsupported / 空，
    // 存量 mock 无需改动即可编译；SqliteStorage 覆写为真实实现。

    /// 插入一条行为规则，返回自增 id。
    ///
    /// 默认实现返回 `Unsupported` 错误（存量 mock 无需实现即可编译）。
    async fn save_behavior_rule(&self, _rule: &BehaviorRule) -> RamariaResult<i64> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现 behavior_rules 写入",
        ))
    }

    /// 按 id 查询行为规则。
    ///
    /// 默认实现返回 `Ok(None)`。
    async fn get_behavior_rule(&self, _id: i64) -> RamariaResult<Option<BehaviorRule>> {
        Ok(None)
    }

    /// 按 persona 查询全部行为规则（含 enabled=false，管理端需要看到禁用项）。
    ///
    /// 默认实现返回空列表。
    async fn list_behavior_rules_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<BehaviorRule>> {
        Ok(Vec::new())
    }

    /// 整体更新一条行为规则（edit 命令；reaction/params/avoid/situation 全部覆盖）。
    ///
    /// 默认实现返回 `Unsupported` 错误。
    async fn update_behavior_rule(&self, _rule: &BehaviorRule) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现 behavior_rules 更新",
        ))
    }

    /// 删除一条行为规则（delete 命令，需确认）。
    ///
    /// 默认实现返回 `Unsupported` 错误。
    async fn delete_behavior_rule(&self, _id: i64) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现 behavior_rules 删除",
        ))
    }

    /// 启用/禁用一条行为规则（disable/enable 命令）。
    ///
    /// 默认实现返回 `Unsupported` 错误。
    async fn set_rule_enabled(&self, _id: i64, _enabled: bool) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现 behavior_rules enabled 切换",
        ))
    }

    // =========================================================
    // 反馈日志（S2/S3 复用同表）
    // =========================================================

    /// 写入一条反馈日志，返回自增 id。
    ///
    /// 默认实现返回 `Unsupported` 错误。
    async fn save_feedback_log(&self, _log: &FeedbackLog) -> RamariaResult<i64> {
        Err(crate::error::RamariaError::unsupported(
            "StoreInfrastructure 未实现 feedback_log 写入",
        ))
    }

    /// 按 persona 查询反馈日志（审计/证据链展示）。
    ///
    /// 默认实现返回空列表。
    async fn list_feedback_logs_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<FeedbackLog>> {
        Ok(Vec::new())
    }
}

// =========================================================
// 存储后端聚合超级 trait
// =========================================================

/// 存储后端聚合 trait。
///
/// 职责:
/// - 聚合 [`StoreCrud`]（业务对象 CRUD）与 [`StoreInfrastructure`]（基础设施），
///   为调用方提供单一 bound / trait object 以引用完整存储能力，避免在泛型签名上
///   罗列多个子 trait。
///
/// 实现说明:
/// - 本 trait 无自身方法；对任何同时实现 `StoreCrud + StoreInfrastructure` 的类型
///   由 blanket impl 自动满足，实现方无需再写 `impl StorageBackend for T {}`。
/// - `dyn StorageBackend` 可通过 supertrait 调用 `StoreCrud`/`StoreInfrastructure`
///   中的全部方法。
pub trait StorageBackend: StoreCrud + StoreInfrastructure {}

impl<T: StoreCrud + StoreInfrastructure> StorageBackend for T {}
