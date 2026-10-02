//! crates/ramaria-service/src/engine/usecases_ops.rs - Ramaria 运维域用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在各自用例模块（export / utt / keyword / settings / privacy /
//!   config / setup / model / diagnostics），本层只做入口挂载
//! - 配置纪律：config.toml 为权威源（双写以文件为准）；保存 / 重载成功后整体替换内存快照，
//!   路径字段由装配持有、热重载时保留现快照值
//! - 宿主后台任务：空闲检查循环与生命周期容器由入口层按需拉起（同一份空闲检查实现）
//! - 首次配置：密钥入 keychain → 后端配置落库 → provider 热替换 → 健康探测 → 状态机推进
//! - API key 不经配置用例：密钥始终由 OS keychain 管理，配置结构本身不含密钥

use std::sync::Arc;

use ramaria_core::config::RamariaConfig;
use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::lock::write_recover;
use ramaria_core::types::{AppState, BackendConfig};

use crate::config::{ConfigWriter, SyncOutcome, SyncWriteResult};
use crate::diagnostics::{DiagnosticsReport, DiagnosticsRequest};
use crate::export::{ExportData, ExportDataRequest};
use crate::idle::{IdleLoop, IdleLoopOptions};
use crate::lifecycle::{Lifecycle, LifecycleOptions};
use crate::privacy::PrivacyStatus;
use crate::types::{
    AliasResolveOutcome, AliasResolveRequest, DegradedReason, EmbeddingModelView,
    EmbeddingValidation, KeywordPoolView, KeywordSeedOutcome, KeywordSuggestionOutcome,
    PendingAliasView, SetupRequest, SetupStatus,
};
use crate::utt::UttRebuildOutcome;

use super::Engine;

// =========================================================
// 导出与 utt 重建用例
// =========================================================

impl Engine {
    /// 会话导出数据装配用例（会话集合 + 消息 + 人格 L1 摘要段）。
    ///
    /// 说明:
    /// - 只做数据装配；JSON / Markdown 文本生成与文件写出属入口能力；
    /// - `total_sessions` 为过滤前全部会话数，`sessions` 为过滤并分页后的装配结果。
    pub async fn export_sessions(&self, req: ExportDataRequest) -> RamariaResult<ExportData> {
        crate::export::collect(self, req).await
    }

    /// utt 话语块重建用例（可选 `--force` 全量重切，完成后刷新检索索引）。
    ///
    /// 说明:
    /// - 以当前生效配置的 `[utt]` 组为切分参数；配置未启用时 `rebuilt = false`；
    /// - `force = true` 先清空全部旧块再全量重建（切分参数变更后必须使用）。
    pub async fn rebuild_utt_blocks(&self, force: bool) -> RamariaResult<UttRebuildOutcome> {
        crate::utt::rebuild(self, force).await
    }
}

// =========================================================
// 关键词用例
// =========================================================

impl Engine {
    /// 关键词池列表用例（三态计数 + 全量词条）。
    pub async fn keyword_list(&self) -> RamariaResult<KeywordPoolView> {
        crate::keyword::list(self).await
    }

    /// 待确认别名列表用例（pending，别名 → 建议规范词）。
    pub async fn keyword_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasView>> {
        crate::keyword::pending_aliases(self).await
    }

    /// 别名裁决用例：确认合并（pending → alias）/ 驳回晋升（pending → canonical）。
    ///
    /// 说明:
    /// - confirm 且已是 alias 时按 `already_applied_ok` 选择幂等成功或报错
    ///   （调用入口各自口径）。
    pub async fn keyword_resolve_alias(
        &self,
        req: AliasResolveRequest,
    ) -> RamariaResult<AliasResolveOutcome> {
        crate::keyword::resolve_alias(self, req).await
    }

    /// 关键词 seed 用例：幂等手工注入规范词（已存在保持现状）。
    ///
    /// 说明:
    /// - 整体校验（任一非法即报错、不部分写入）后去重，保留首次出现顺序；
    ///   新词条 use_count 从 0 起，已存在词条不递增 use_count、不改别名状态。
    pub async fn keyword_seed(&self, keywords: &[String]) -> RamariaResult<KeywordSeedOutcome> {
        crate::keyword::seed(self, keywords).await
    }

    /// 关键词别名建议用例：扫描词池与内存镜像使用量，把相似词对登记为待确认别名。
    ///
    /// 说明:
    /// - `min_use` 为 None 时取服务层默认阈值（过滤仅出现 1-2 次的偶然用词）；
    /// - 单次运行最多登记固定条数，超出部分本轮不写（结果中携带截断计数）；
    /// - 调用入口按 best-effort 处理错误（建议生成不阻塞列表 / 主流程）。
    pub async fn keyword_suggest_pending_aliases(
        &self,
        min_use: Option<u32>,
    ) -> RamariaResult<KeywordSuggestionOutcome> {
        crate::keyword::suggest_pending_aliases(self, min_use).await
    }
}

// =========================================================
// 宿主后台任务
// =========================================================

impl Engine {
    /// 启动进程内空闲检查循环（超时会话按 `[session].l1_idle_minutes` 触发封存）。
    ///
    /// 用法:
    /// - 仅需"超时会话自动封存"的轻量宿主（MCP 服务端等）启动时拉起，退出时
    ///   [`IdleLoop::shutdown`] 优雅关停；
    /// - 需要活跃指针与 L2/L3 调度的宿主改用 [`Engine::start_lifecycle`]（同一份空闲检查实现）；
    /// - 封存消耗 LLM 并改变记忆状态：入口层可自行按配置门禁决定是否拉起（如 `[mcp].allow_seal`）。
    ///
    /// 返回:
    /// - 循环句柄；drop 或 [`IdleLoop::shutdown`] 均会置停止位。
    pub fn spawn_idle_loop(self: &Arc<Self>) -> IdleLoop {
        let options = IdleLoopOptions::from_config(self.config().as_ref());
        IdleLoop::spawn(Arc::clone(self), options)
    }

    /// 以显式选项启动空闲检查循环（测试与需要非配置间隔的宿主使用）。
    ///
    /// 参数:
    /// - `options`: 循环选项（间隔秒数）。生产路径应走
    ///   [`IdleLoopOptions::from_config`]（含下限夹取，避免热循环）。
    pub fn spawn_idle_loop_with(self: &Arc<Self>, options: IdleLoopOptions) -> IdleLoop {
        IdleLoop::spawn(Arc::clone(self), options)
    }

    /// 拉起会话生命周期（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）。
    ///
    /// 用法:
    /// - 长驻宿主启动时按选项拉起（见 [`LifecycleOptions`]：桌面 / MCP / 单次执行），
    ///   退出时调用 [`Lifecycle::shutdown`] 优雅关停；
    /// - 仅需空闲封存的轻量宿主可继续使用 [`Engine::spawn_idle_loop`]。
    ///
    /// 返回:
    /// - 生命周期容器句柄（持有引擎，引擎不反向持有容器，避免引用环）。
    pub fn start_lifecycle(self: &Arc<Self>, options: LifecycleOptions) -> Arc<Lifecycle> {
        Lifecycle::start(Arc::clone(self), options)
    }
}

// =========================================================
// 设置与元信息用例
// =========================================================

impl Engine {
    /// 设置列表用例：读取全部设置项（`settings` 表键值对）。
    ///
    /// 返回:
    /// - 空库返回空列表（非错误）；返回键集合与过滤口径保持现状。
    pub async fn settings_list(&self) -> RamariaResult<Vec<(String, String)>> {
        crate::settings::list(self).await
    }

    /// 设置读取用例：读取单个设置项（缺失键返回 None，不报错）。
    pub async fn setting_get(&self, key: &str) -> RamariaResult<Option<String>> {
        crate::settings::get(self, key).await
    }

    /// 设置写入用例：写入单个设置项（已存在键覆盖写）。
    ///
    /// 返回:
    /// - 空键返回 `Validation` 错误（文案与桌面现状一致）。
    pub async fn setting_set(&self, key: &str, value: &str) -> RamariaResult<()> {
        crate::settings::set(self, key, value).await
    }

    /// 读取 DB 侧后端配置（`backend_config` 表）。
    ///
    /// 返回:
    /// - `Ok(None)`: 无记录（回退口径由调用方按各自现状决定）。
    pub async fn backend_config(&self) -> RamariaResult<Option<BackendConfig>> {
        crate::settings::backend_config(self).await
    }

    /// 读取数据库 schema 版本（`schema_meta` 表；键缺失按 1，非法值报错）。
    pub async fn schema_version(&self) -> RamariaResult<i32> {
        crate::settings::schema_version(self).await
    }
}

// =========================================================
// 隐私确认用例
// =========================================================

impl Engine {
    /// 检查当前后端的隐私确认状态。
    ///
    /// 说明:
    /// - 判定输入（provider / base_url）取 DB 侧后端配置，与桌面 / CLI 现状同源；
    /// - 无后端配置记录时按本地 provider 默认值判定（无需确认）。
    pub async fn check_privacy(&self) -> RamariaResult<PrivacyStatus> {
        crate::privacy::check(self).await
    }

    /// 记录当前后端的隐私确认。
    ///
    /// 参数:
    /// - `persistent`: 是否跨重启持久化（勾选"下次不再提醒"）。
    pub async fn confirm_privacy(&self, persistent: bool) -> RamariaResult<()> {
        crate::privacy::confirm(self, persistent).await
    }
}

// =========================================================
// 配置用例（双写同步与热重载）
// =========================================================

impl Engine {
    /// 只读加载完整配置（config.toml 与 DB 侧合并，无写副作用）。
    ///
    /// 说明:
    /// - 等价 `ConfigWriter::load_config_only`：文件缺失 / 解析失败时以 DB 侧为准；
    /// - 不更新内存快照、不写任何一侧（设置页回显等只读场景）。
    pub async fn load_full_config(&self) -> RamariaResult<RamariaConfig> {
        let writer = self.config_writer()?;
        writer.load_config_only().await
    }

    /// 重新加载配置：一致性校验（文件为准）→ 回写 DB → 热重载内存快照。
    ///
    /// 说明:
    /// - 校验规则见 `ConfigWriter::load`（文件缺失 / 损坏路径以 DB 为准且不回写 DB）；
    /// - 成功后以合并结果整体替换内存快照（后续用例读取生效）；
    /// - 热重载范围：仅配置快照；后台循环阈值（如空闲分钟数）由宿主持有的
    ///   生命周期容器热更新，本用例不联动；行为待定池（`PendingPool`）保持既有内存态。
    pub async fn reload_config(&self) -> RamariaResult<SyncOutcome> {
        let writer = self.config_writer()?;
        let mut outcome = writer.load().await?;
        // 路径字段由装配持有（config.toml 的 paths 组只表达展示性空值）：热重载保留现快照值，
        // 避免日志目录 / 配置目录随重载丢失（诊断导出等功能依赖这些路径）
        outcome.config.paths = self.config().paths.clone();
        self.replace_config_snapshot(outcome.config.clone());
        Ok(outcome)
    }

    /// 保存完整配置：文件与 DB 双写（settings / backend_config 表），成功后热重载内存快照。
    ///
    /// 说明:
    /// - 单侧写失败降级不阻塞：结果经 `SyncWriteResult` 回传（调用方展示提示）；
    /// - 双侧全部成功时替换内存快照（后续用例读取生效），失败时保持原快照；
    /// - API key 不经本用例：密钥始终由 OS keychain 管理，配置结构本身不含密钥。
    pub async fn save_config(&self, cfg: &RamariaConfig) -> RamariaResult<SyncWriteResult> {
        let writer = self.config_writer()?;
        let result = writer.save_config(cfg).await;
        if result.is_ok() {
            // 路径字段由装配持有（保存的配置不含本机路径）：热重载时保留现快照值
            let mut next = cfg.clone();
            next.paths = self.config().paths.clone();
            self.replace_config_snapshot(next);
        }
        Ok(result)
    }

    /// 同步后端配置到文件侧 `[backend]` 组（保留文件侧其它字段与未知键）。
    ///
    /// 说明:
    /// - 仅文件侧：DB 侧由调用方（后端配置用例）先行写入，保持表 / 文件一致；
    /// - 文件损坏时拒绝覆盖（保留现场，返回失败明细）。
    pub async fn sync_backend_config(
        &self,
        backend: &BackendConfig,
    ) -> RamariaResult<SyncWriteResult> {
        let writer = self.config_writer()?;
        Ok(writer.sync_backend_config(backend).await)
    }

    /// 构造配置用例句柄（`config_path` 为空时返回显式错误）。
    fn config_writer(&self) -> RamariaResult<ConfigWriter> {
        if self.config_path.as_os_str().is_empty() {
            return Err(RamariaError::config(
                "引擎未设置 config_path，无法执行配置读写用例",
            ));
        }
        Ok(ConfigWriter::new(
            Arc::clone(&self.storage),
            self.config_path.clone(),
        ))
    }

    /// 整体替换内存配置快照（仅由配置用例调用；不重建行为待定池等既有内存态）。
    fn replace_config_snapshot(&self, cfg: RamariaConfig) {
        let mut guard = write_recover(&self.config, "engine.config");
        *guard = Arc::new(cfg);
    }
}

// =========================================================
// 首次配置用例（状态机推进与缺项诊断）
// =========================================================

impl Engine {
    /// 读取首次配置缺项诊断（后端配置 / 模型选择 / 索引 / 嵌入四项）。
    pub async fn check_setup_status(&self) -> RamariaResult<SetupStatus> {
        crate::setup::check(self).await
    }

    /// 执行首次配置：密钥入 keychain → 后端配置落库 → provider 热替换 → 健康探测 → 推进状态机。
    ///
    /// 返回:
    /// - 探测通过时返回按缺项诊断判定的状态；全部失败返回 `Degraded`（不报错）。
    pub async fn run_setup(&self, req: &SetupRequest) -> RamariaResult<AppState> {
        crate::setup::run(self, req).await
    }

    /// 刷新应用状态（索引构建完成 / 嵌入热加载 / 配置变更后调用）。
    pub async fn refresh_setup_state(&self) -> RamariaResult<AppState> {
        crate::setup::refresh(self).await
    }

    /// 探测当前 LLM 后端可达性（最多 3 次、间隔 2 秒）。
    ///
    /// 返回:
    /// - `true`: 至少一次探测通过；`false`: 全部失败。
    ///
    /// 用途:
    /// - 入口层的「测试连接」动作；与首次配置使用的探测实现同一份（重试口径一致）。
    pub async fn probe_llm_health(&self) -> bool {
        let llm = self.llm_ref();
        crate::setup::probe_health_with_retry(
            llm.as_ref(),
            crate::setup::HEALTH_PROBE_ATTEMPTS,
            crate::setup::HEALTH_PROBE_INTERVAL_SECONDS,
        )
        .await
    }
}

// =========================================================
// 模型管理用例（后端配置 / 嵌入模型）
// =========================================================

impl Engine {
    /// 更新 LLM 后端配置并热加载 provider（密钥入 keychain → 配置落库 → provider 热替换
    /// → 文件侧 `[backend]` 组同步）。
    ///
    /// 参数:
    /// - `config`: 新的后端配置（provider / base_url / model / 嵌入路径等）。
    /// - `api_key`: 可选的线上 provider 密钥；`None` 或空白表示不更新密钥。
    ///
    /// 返回:
    /// - 成功时返回 `Ok(())`，此后读取路径取到新 provider。
    pub async fn update_backend_config(
        &self,
        config: &BackendConfig,
        api_key: Option<&str>,
    ) -> RamariaResult<()> {
        crate::model::update_backend_config(self, config, api_key).await
    }

    /// 校验指定目录能否作为嵌入模型使用（无副作用探测）。
    ///
    /// 返回:
    /// - `valid=false` + `reason` 表达目录缺失 / 加载失败 / 推理失败，不抛错。
    pub async fn validate_embedding_model(&self, path: &str) -> RamariaResult<EmbeddingValidation> {
        crate::model::validate_embedding_model(path, self.config().embedding.device).await
    }

    /// 保存嵌入模型配置并热加载（`None` 或空白路径 = 卸载）。
    ///
    /// 返回:
    /// - 成功时内存 provider 与持久化路径同时生效；加载失败时保持原状态不变。
    pub async fn save_embedding_model(&self, path: Option<&str>) -> RamariaResult<()> {
        crate::model::save_embedding_model(self, path).await
    }

    /// 读取当前嵌入模型配置（已加载 → 维度 / 可用性；未加载 → 配置中的路径）。
    pub async fn embedding_model(&self) -> RamariaResult<Option<EmbeddingModelView>> {
        crate::model::embedding_model(self).await
    }

    /// 读取当前降级原因（非 `Degraded` 状态返回 `None`）。
    pub async fn degraded_reason(&self) -> RamariaResult<Option<DegradedReason>> {
        crate::model::degraded_reason(self).await
    }
}

// =========================================================
// 诊断导出用例
// =========================================================

impl Engine {
    /// 导出诊断信息为 .zip（日志 / 配置 / 系统信息；敏感内容先脱敏再打包）。
    ///
    /// 说明:
    /// - 配置快照在本方法内读取，收集与打包在锁外进行；
    /// - 收集阶段错误不阻塞导出；写入经同目录临时文件原子替换。
    pub async fn export_diagnostics(
        &self,
        req: DiagnosticsRequest,
    ) -> RamariaResult<DiagnosticsReport> {
        crate::diagnostics::export(self, req).await
    }
}
