//! crates/ramaria-service/src/engine/usecases_persona.rs - Ramaria 人格域用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在 `persona` / `behavior` / `style` 模块，本层只做入口挂载
//! - 人格管理：列表（摘要 / 全字段）、卡片视图、信息更新、文件导入（目录 / 单文件）
//!   与系统用户人格幂等创建
//! - 行为规则：管理（增删改查 / 导入）、学习（聚类生成 Auto 规则）与增量更新（封存钩子核心）
//! - 表达风格：五维统计增量更新、注入侧规则读取与统计视图
//! - 空态语义：规则 / 统计不存在返回空态（非错误），由用例实现保证

use std::path::Path;

use ramaria_core::behavior::BehaviorRule;
use ramaria_core::error::RamariaResult;

use crate::behavior::{BehaviorLearnOutcome, RuleEvidenceItem};
use crate::persona::{PersonaLoadMode, PersonaRegenerateOutcome};
use crate::style::StyleStatsView;
use crate::types::{
    PersonaCardRequest, PersonaCardView, PersonaFileOutcome, PersonaFullView, PersonaSummaryView,
    PersonaUpdateRequest,
};

use super::Engine;

// =========================================================
// 人格用例
// =========================================================

impl Engine {
    /// 人格列表用例：列出全部人格摘要（uid / 名称 / 类型 / 来源 / 启用状态）。
    pub async fn persona_list(&self) -> RamariaResult<Vec<PersonaSummaryView>> {
        crate::persona::list(self).await
    }

    /// 人格卡片用例：性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度。
    pub async fn persona_card(&self, req: PersonaCardRequest) -> RamariaResult<PersonaCardView> {
        crate::persona::card(self, req).await
    }

    /// 人格全字段列表用例（含 ref_id / avatar / config / description / 更新时间）。
    pub async fn persona_list_full(&self) -> RamariaResult<Vec<PersonaFullView>> {
        crate::persona::list_full(self).await
    }

    /// 人格信息更新用例（名称 / 头像 / 描述；配置内容由文件导入通道管理）。
    ///
    /// 返回:
    /// - 更新后回读的完整视图；uid 为空 / 人格不存在返回 `Validation` 错误。
    pub async fn persona_update_info(
        &self,
        uid: &str,
        req: PersonaUpdateRequest,
    ) -> RamariaResult<PersonaFullView> {
        crate::persona::update_info(self, uid, req).await
    }

    /// 人格文件导入用例：从目录扫描 `.toml` 文件（文件名 = uid）创建或同步记录。
    ///
    /// 参数:
    /// - `dir`: 人格文件目录（目录解析由调用方负责；目录不可读返回 `Io` 错误）。
    /// - `uid_filter`: 只处理指定 uid（文件名 stem 精确匹配）；`None` 表示全部。
    /// - `mode`: 记录已存在时的处置模式（创建或更新 / 仅创建缺失跳过）。
    ///
    /// 返回:
    /// - 每个文件的处理结果（新建 / 更新 / 跳过 / 失败），单文件失败不中断其余文件。
    pub async fn persona_load_from_dir(
        &self,
        dir: &Path,
        uid_filter: Option<&str>,
        mode: PersonaLoadMode,
    ) -> RamariaResult<Vec<PersonaFileOutcome>> {
        crate::persona::load_from_dir(self, dir, uid_filter, mode).await
    }

    /// 人格文件导入用例（单个文件；旧单文件布局兼容路径）。
    ///
    /// 参数:
    /// - `path`: 人格文件路径（旧布局的文件名不携带 uid，uid 由调用方显式给出）。
    /// - `uid`: 目标人格 uid（旧单文件布局使用固定的 `rama-0001`）。
    /// - `fallback_name`: 文件缺少 `assistant_name` 时的名称兜底。
    /// - `mode`: 记录已存在时的处置模式（创建或更新 / 仅创建缺失跳过）。
    ///
    /// 返回:
    /// - 本文件的处理结果（新建 / 更新 / 跳过 / 失败）；失败转为结果条目（不上抛）。
    pub async fn persona_load_file(
        &self,
        path: &Path,
        uid: &str,
        fallback_name: &str,
        mode: PersonaLoadMode,
    ) -> PersonaFileOutcome {
        crate::persona::load_file(self, path, uid, fallback_name, mode).await
    }

    /// 确保系统用户人格（user-0001）存在（幂等）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次创建；`Ok(false)`: 已存在（未做任何写入）。
    pub async fn persona_ensure_user(&self) -> RamariaResult<bool> {
        crate::persona::ensure_user(self).await
    }

    /// 重生成某人格在导入会话中的 L1 摘要（不含 L2/L3 级联，宿主按需触发）。
    ///
    /// 返回:
    /// - 逐会话重生成计数与提示文案；级联（L2/L3）由宿主在拿到结果后自行触发。
    pub async fn regenerate_persona_l1(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<PersonaRegenerateOutcome> {
        crate::persona::regenerate_import_l1(self, persona_uid).await
    }
}

// =========================================================
// 行为规则与表达风格用例
// =========================================================

impl Engine {
    /// 行为规则列表用例：按 persona 列出全部规则（含禁用项）。
    ///
    /// 返回:
    /// - 全量规则列表（存储层稳定排序）。
    pub async fn behavior_list_rules(&self, persona_uid: &str) -> RamariaResult<Vec<BehaviorRule>> {
        crate::behavior::list_rules(self, persona_uid).await
    }

    /// 行为规则详情用例：按 id 查询单条规则。
    ///
    /// 返回:
    /// - `Ok(Some(rule))`: 规则存在；`Ok(None)`: 规则不存在（空态，非错误）。
    pub async fn behavior_get_rule(&self, id: i64) -> RamariaResult<Option<BehaviorRule>> {
        crate::behavior::get_rule(self, id).await
    }

    /// 行为规则启停用例：禁用写 S1 反馈日志（启用不写，非干预信号）。
    ///
    /// 参数:
    /// - `id`: 规则 id。
    /// - `enabled`: true = 启用，false = 禁用。
    /// - `session_id`: 干预发生的会话（可选，审计关联）。
    pub async fn behavior_set_rule_enabled(
        &self,
        id: i64,
        enabled: bool,
        session_id: Option<&str>,
    ) -> RamariaResult<()> {
        crate::behavior::set_rule_enabled(self, id, enabled, session_id).await
    }

    /// 行为规则编辑用例：全量覆盖 + 转 Manual 强锚点 + 写编辑前后快照反馈。
    ///
    /// 参数:
    /// - `rule`: 编辑后的完整规则（id 定位）。
    /// - `session_id`: 干预发生的会话（可选，审计关联）。
    pub async fn behavior_edit_rule(
        &self,
        rule: &mut BehaviorRule,
        session_id: Option<&str>,
    ) -> RamariaResult<()> {
        crate::behavior::edit_rule(self, rule, session_id).await
    }

    /// 行为规则删除用例（破坏性操作，调用方负责确认）。
    pub async fn behavior_delete_rule(&self, id: i64) -> RamariaResult<()> {
        crate::behavior::delete_rule(self, id).await
    }

    /// 行为规则导入用例：宽松 JSON 校验（非法拒绝），导入规则 source=Manual。
    ///
    /// 参数:
    /// - `persona_uid`: 规则所属人格。
    /// - `json`: 规则 JSON（含 situation / reaction / params / avoid 字段）。
    ///
    /// 返回:
    /// - 新规则 id（Manual，自动生效）。
    pub async fn behavior_import_rule(&self, persona_uid: &str, json: &str) -> RamariaResult<i64> {
        crate::behavior::import_rule(self, persona_uid, json).await
    }

    /// 行为规则证据链用例：规则 → 事件 → 脱敏视图（权重降序，脏引用跳过）。
    ///
    /// 返回:
    /// - 证据项列表；规则不存在时返回业务校验错误。
    pub async fn behavior_rule_evidence(&self, id: i64) -> RamariaResult<Vec<RuleEvidenceItem>> {
        crate::behavior::rule_evidence(self, id).await
    }

    /// 行为规则全量学习用例：事件 → 聚类（含 Manual 锚点）→ 规则生成 → 替换旧 Auto。
    ///
    /// 返回:
    /// - 学习统计；`[behavior].enabled=false` 时返回空统计。
    pub async fn behavior_learn(&self, persona_uid: &str) -> RamariaResult<BehaviorLearnOutcome> {
        crate::behavior::learn(self, persona_uid).await
    }

    /// 行为规则增量更新用例（封存钩子核心，供宿主手动触发）。
    ///
    /// 说明:
    /// - 处理 persona 未吸收事件：归簇 / 待定池推进 / 证据衰减 / 漂移检测并落库；
    /// - `[behavior].enabled=false` 时直接返回。
    pub async fn behavior_incremental_update(&self, persona_uid: &str) -> RamariaResult<()> {
        crate::behavior::incremental_update(self, persona_uid).await
    }

    /// 风格统计增量更新用例（封存钩子核心，供宿主手动补跑）。
    ///
    /// 说明:
    /// - 全量消息 → 五维统计 → 基线显著性 → 规则文本生成 / 替换落库（幂等）；
    /// - LLM 不可用 / 失败由核心静默降级为模板生成；开关由调用方判断。
    pub async fn style_incremental_update(&self, persona_uid: &str) -> RamariaResult<()> {
        crate::style::incremental_update(self, persona_uid).await
    }

    /// 自动风格规则读取用例（注入侧）：仅 Ready 状态返回非空规则文本。
    ///
    /// 返回:
    /// - `Ok(Some(rule))`: 可注入的规则文本；
    /// - `Ok(None)`: 数据不足 / 无显著项 / 未统计（静默跳过）。
    pub async fn style_load_rule(&self, persona_uid: &str) -> RamariaResult<Option<String>> {
        crate::style::load_style_rule(self.storage_ref().as_ref(), persona_uid).await
    }

    /// 说话风格统计读取用例：单行统计视图。
    ///
    /// 返回:
    /// - `Ok(Some(view))`: 样本量 / 状态与标签 / 规则来源与标签 / 规则文本 / 统计 JSON / 更新时间；
    /// - `Ok(None)`: 该人格未统计过（空态，非错误）。
    pub async fn style_stats(&self, persona_uid: &str) -> RamariaResult<Option<StyleStatsView>> {
        crate::style::stats(self, persona_uid).await
    }
}
