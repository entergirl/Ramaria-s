//! crates/ramaria-service/src/persona.rs - 人格读取、管理与重生成用例
//!
//! 设计特点:
//! - 人格读取：人格摘要列表与人格卡片（性格画像 / 行为规则 / 表达风格 / 知识事实 / 数据成熟度）
//! - 人格管理：全字段列表、基本信息更新、人格文件导入（目录扫描文件名 = uid / 单文件显式 uid；单文件失败不中断）
//! - 人格重生成：导入失败后的离线重建路径（全量消息枚举 → 会话去重 → 逐会话 L1 重生成，含连续失败早停）
//! - 严格按 persona_uid 隔离：读取与重生成只处理目标人格的记录，不跨人格聚合
//! - 逐段独立降级：卡片任一段读取失败记 warn 并返回空段，不阻塞整张卡片；文件导入单文件失败转为结果条目
//! - 条目上限：卡片各段最多返回 [`MAX_CARD_ITEMS`] 条（避免大库把整张卡片撑爆）
//! - 隐私：卡片不含 utt 原文块；日志中的个人标识经 `mask_id` 脱敏

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::error::{RamariaError, RamariaResult};
use ramaria_core::privacy::mask_id;
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{Persona, PersonaKind, ProfileField, TraitStatus};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::{
    BehaviorRuleView, DataMaturityView, FactView, PersonaCardRequest, PersonaCardView,
    PersonaFileAction, PersonaFileOutcome, PersonaFullView, PersonaSection, PersonaSummaryView,
    PersonaUpdateRequest, StyleView, TraitView,
};

/// 人格卡片各段最多返回的条目数。
const MAX_CARD_ITEMS: usize = 20;

/// 数据成熟度计数查询上限（诊断用途，超量按上限计）。
const MATURITY_COUNT_LIMIT: u32 = 1_000;

// =========================================================
// persona_list
// =========================================================

/// 列出全部人格摘要（uid / 名称 / 类型 / 来源 / 简介 / 启用状态）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 按存储层顺序返回全部人格（含停用项，调用方可按 `active` 过滤）。
pub(crate) async fn list(engine: &Engine) -> RamariaResult<Vec<PersonaSummaryView>> {
    let personas = engine.storage_ref().list_personas().await?;
    let views = personas
        .into_iter()
        .map(|p| PersonaSummaryView {
            uid: p.uid,
            name: p.name,
            kind: p.kind,
            source: p.source,
            description: p.description,
            active: p.active,
        })
        .collect();
    Ok(views)
}

// =========================================================
// persona_get（人格卡片）
// =========================================================

/// 组装人格卡片（按 `sections` 选择分段；缺省全部分段）。
///
/// 流程:
/// 1. 读取人格行（不存在 → Validation 错误，错误可见）；
/// 2. 按分段读取关联数据（每段独立降级，失败记 warn 并置空）；
/// 3. 组装视图（各段受 [`MAX_CARD_ITEMS`] 截断；成熟度计数为全量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `req`: 卡片请求（uid 必填；sections 可选）。
///
/// 返回:
/// - 成功时返回人格卡片视图；人格不存在时返回 `Validation` 错误。
pub(crate) async fn card(
    engine: &Engine,
    req: PersonaCardRequest,
) -> RamariaResult<PersonaCardView> {
    let storage = engine.storage_ref();
    let uid = req.uid.trim().to_string();
    if uid.is_empty() {
        return Err(RamariaError::validation("uid 不能为空"));
    }

    let persona = match storage.get_persona_by_uid(&uid).await? {
        Some(persona) => persona,
        None => {
            return Err(RamariaError::validation(format!("人格不存在: {uid}")));
        }
    };

    let sections = req.effective_sections();
    let wants = |section: PersonaSection| sections.contains(&section);

    // ---- 性格画像（L3 三层标签，仅 Active 参与展示） ----
    let traits = if wants(PersonaSection::Traits) || wants(PersonaSection::Maturity) {
        match storage.list_traits_by_persona(&uid).await {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取性格标签失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let trait_views: Vec<TraitView> = if wants(PersonaSection::Traits) {
        traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .take(MAX_CARD_ITEMS)
            .map(|t| TraitView {
                layer: t.layer,
                label: t.trait_label.clone(),
                meaning: t.meaning.clone(),
                trigger: t.trigger.clone(),
                confidence: t.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 行为规则（含停用项，`enabled` 字段供调用方判断） ----
    let behaviors: Vec<BehaviorRuleView> = if wants(PersonaSection::Behaviors) {
        match storage.list_behavior_rules_by_persona(&uid).await {
            Ok(rules) => rules
                .iter()
                .take(MAX_CARD_ITEMS)
                .map(|rule| BehaviorRuleView {
                    id: rule.id,
                    situation: rule.situation.keywords.join("、"),
                    reaction: rule.reaction.clone(),
                    avoid: rule.avoid.clone(),
                    confidence: rule.confidence,
                    enabled: rule.enabled,
                })
                .collect(),
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取行为规则失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };

    // ---- 表达风格（自动风格规则，仅 Ready 有文本） ----
    let style = if wants(PersonaSection::Style) {
        match storage.get_style_stats(&uid).await {
            Ok(Some(stats)) => Some(StyleView {
                rule_text: stats.rule_text.clone().filter(|t| !t.trim().is_empty()),
                status: stats.status,
                sample_count: stats.sample_count,
            }),
            Ok(None) => None,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取风格统计失败，卡片该段为空");
                None
            }
        }
    } else {
        None
    };

    // ---- 知识事实（active；SpeakingStyle 由表达层单独呈现，此处不重复） ----
    let facts = if wants(PersonaSection::Facts) || wants(PersonaSection::Maturity) {
        match storage.list_active_facts_by_persona(&uid).await {
            Ok(facts) => facts,
            Err(e) => {
                tracing::warn!(uid, error = %e, "读取知识事实失败，卡片该段为空");
                Vec::new()
            }
        }
    } else {
        Vec::new()
    };
    let fact_views: Vec<FactView> = if wants(PersonaSection::Facts) {
        facts
            .iter()
            .take(MAX_CARD_ITEMS)
            .map(|f| FactView {
                field: f.field,
                content: f.content.clone(),
                tier: f.tier,
                confidence: f.confidence,
            })
            .collect()
    } else {
        Vec::new()
    };

    // ---- 数据成熟度（各层数据量计数；诊断用，受查询上限约束） ----
    let maturity = if wants(PersonaSection::Maturity) {
        maturity_view(engine, &uid, &traits, &facts).await
    } else {
        DataMaturityView::default()
    };

    tracing::debug!(
        uid = %uid,
        traits = trait_views.len(),
        behaviors = behaviors.len(),
        facts = fact_views.len(),
        "人格卡片已组装"
    );

    Ok(PersonaCardView {
        uid: persona.uid,
        name: persona.name,
        kind: persona.kind,
        source: persona.source,
        description: persona.description,
        active: persona.active,
        traits: trait_views,
        behaviors,
        style,
        facts: fact_views,
        maturity,
    })
}

/// 组装数据成熟度视图（各层数据量计数）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 目标人格。
/// - `traits` / `facts`: 已读取的画像数据（复用，避免重复查询）。
///
/// 返回:
/// - 计数视图；单项查询失败记 warn 并按 0 计（不阻塞卡片）。
async fn maturity_view(
    engine: &Engine,
    uid: &str,
    traits: &[ramaria_core::types::PersonalityTrait],
    facts: &[ramaria_core::types::PersonaFact],
) -> DataMaturityView {
    let storage = engine.storage_ref();

    // L1 摘要条数（按 persona 取最近 N 条计数，超量按上限计）
    let l1_count = match storage
        .list_recent_l1_by_persona(uid, MATURITY_COUNT_LIMIT)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取 L1 计数失败，按 0 计");
            0
        }
    };

    // L2 事件条数
    let event_count = match storage
        .list_events_by_persona(uid, 0, MATURITY_COUNT_LIMIT as i64)
        .await
    {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取事件计数失败，按 0 计");
            0
        }
    };

    // 对话示例条数（候选池全量）
    let example_count = match storage.list_all_examples(uid).await {
        Ok(list) => list.len(),
        Err(e) => {
            tracing::warn!(uid, error = %e, "成熟度：读取示例计数失败，按 0 计");
            0
        }
    };

    // 知识事实条数：排除 SpeakingStyle（表达层单独呈现，不计入知识卡片成熟度）
    let fact_count = facts
        .iter()
        .filter(|f| f.field != ProfileField::SpeakingStyle)
        .count();

    DataMaturityView {
        l1_count,
        event_count,
        trait_count: traits
            .iter()
            .filter(|t| t.status == TraitStatus::Active)
            .count(),
        fact_count,
        example_count,
    }
}

// =========================================================
// 人格管理用例（全字段列表 / 信息更新 / 文件导入 / 用户人格）
// =========================================================

/// 列出全部人格的完整信息（含 ref_id / avatar / config / description / updated_at）。
///
/// 参数:
/// - `engine`: 服务层引擎。
///
/// 返回:
/// - 按存储层顺序返回全部启用人格；空库返回空列表（非错误）。
pub(crate) async fn list_full(engine: &Engine) -> RamariaResult<Vec<PersonaFullView>> {
    let personas = engine.storage_ref().list_personas().await?;
    Ok(personas.into_iter().map(full_view).collect())
}

/// 更新人格基本信息（名称 / 头像 / 描述）。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `uid`: 人格 uid（不可变更）。
/// - `req`: 更新请求（各字段可选；`None` 保持旧值，空描述表达清空）。
///
/// 返回:
/// - 更新后回读的完整视图；
/// - uid 为空 / 人格不存在返回 `Validation` 错误。
///
/// 说明:
/// - 配置内容（`config`）不由本用例修改（由人格文件导入通道管理），显式保持旧值。
pub(crate) async fn update_info(
    engine: &Engine,
    uid: &str,
    req: PersonaUpdateRequest,
) -> RamariaResult<PersonaFullView> {
    if uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let storage = engine.storage_ref();
    let existing = match storage.get_persona_by_uid(uid).await? {
        Some(persona) => persona,
        None => {
            return Err(RamariaError::validation(format!("人格不存在: uid={uid}")));
        }
    };

    // 名称未提供时沿用旧值；头像 / 描述按"None 保持、Some 覆盖"语义传递
    let new_name = req.name.as_deref().unwrap_or(&existing.name);
    let new_avatar = req.avatar.as_deref();
    let new_config: Option<&str> = None;
    let new_description = req.description.as_deref();

    storage
        .update_persona(uid, new_name, new_avatar, new_config, new_description)
        .await?;

    tracing::info!(
        uid = %mask_id(uid),
        name_changed = req.name.is_some(),
        avatar_changed = req.avatar.is_some(),
        description_changed = req.description.is_some(),
        "人格信息已更新"
    );

    let updated = match storage.get_persona_by_uid(uid).await? {
        Some(persona) => persona,
        None => {
            // 更新成功后记录消失属异常状态，显式报错而非静默
            return Err(RamariaError::storage(format!(
                "更新后 persona 意外不存在: uid={uid}"
            )));
        }
    };
    Ok(full_view(updated))
}

/// 确保系统用户人格（user-0001）存在（幂等）。
///
/// 返回:
/// - `Ok(true)`: 本次创建了 user-0001；
/// - `Ok(false)`: 已存在，未做任何写入。
pub(crate) async fn ensure_user(engine: &Engine) -> RamariaResult<bool> {
    let storage = engine.storage_ref();
    if storage.get_persona_by_uid("user-0001").await?.is_some() {
        tracing::debug!("user-0001 已存在，跳过创建");
        return Ok(false);
    }

    let user = Persona::new(
        "user-0001".to_string(),
        "用户".to_string(),
        PersonaKind::User,
        1,
        "system".to_string(),
    );
    storage.create_persona(&user).await?;
    tracing::info!("已创建 persona: user-0001 (用户)");
    Ok(true)
}

/// 人格文件导入模式（记录已存在时的处置）。
///
/// 格式:
/// - 各变体只表达"记录已存在时"的处置；记录不存在时一律新建。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PersonaLoadMode {
    /// 仅创建缺失：已存在的记录跳过（不写库、不刷新 updated_at）。
    CreateMissing,
    /// 创建或更新：已存在的记录按文件同步名称与配置（其他字段保持）。
    CreateOrUpdate,
}

/// 从人格文件目录导入 `.toml` 文件（文件名不含扩展名 = persona uid）。
///
/// 行为:
/// - 记录不存在 → 新建（source=file，config=文件全文）；
///   记录已存在 → 按 `mode` 处置（创建或更新 / 仅创建缺失跳过）；
/// - 名称取文件中的 `assistant_name`，缺失时回退 uid；
/// - 单文件失败（读取 / 查询 / 写入）不中断其余文件，以 [`PersonaFileOutcome`]
///   如实回传动作与消息；
/// - 文件按路径排序处理，顺序确定；`uid_filter` 指定时只处理文件名匹配的文件。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `dir`: 人格文件目录（目录解析由调用方负责）。
/// - `uid_filter`: 只处理指定 uid（文件名 stem 精确匹配）；`None` 表示全部。
/// - `mode`: 记录已存在时的处置模式（见 [`PersonaLoadMode`]）。
///
/// 返回:
/// - 每个文件的处理结果；目录不可读时返回 `Io` 错误。
///
/// 说明:
/// - 目录无匹配文件不属于本用例错误（返回空结果），由调用方按各自现状
///   给出提示或回退（如旧单文件兼容路径）。
pub(crate) async fn load_from_dir(
    engine: &Engine,
    dir: &Path,
    uid_filter: Option<&str>,
    mode: PersonaLoadMode,
) -> RamariaResult<Vec<PersonaFileOutcome>> {
    let files = collect_persona_files(dir, uid_filter)?;
    let storage = engine.storage_ref();

    let mut outcomes = Vec::with_capacity(files.len());
    for path in files {
        outcomes.push(load_single_file(storage, &path, mode).await);
    }

    tracing::info!(
        files = outcomes.len(),
        created = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Created)
            .count(),
        updated = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Updated)
            .count(),
        skipped = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Skipped)
            .count(),
        failed = outcomes
            .iter()
            .filter(|outcome| outcome.action == PersonaFileAction::Failed)
            .count(),
        "人格文件导入完成"
    );

    Ok(outcomes)
}

/// 行 → 全字段视图（字段映射的唯一入口）。
fn full_view(persona: Persona) -> PersonaFullView {
    PersonaFullView {
        uid: persona.uid,
        name: persona.name,
        kind: persona.kind.as_str().to_string(),
        source: persona.source,
        ref_id: persona.ref_id,
        avatar: persona.avatar,
        config: persona.config,
        description: persona.description,
        is_active: persona.active,
        created_at: persona.created_at,
        updated_at: persona.updated_at,
    }
}

/// 收集目录下的 `.toml` 人格文件（可选按 uid 过滤；按路径排序，顺序确定）。
fn collect_persona_files(dir: &Path, uid_filter: Option<&str>) -> RamariaResult<Vec<PathBuf>> {
    let entries = std::fs::read_dir(dir).map_err(|e| {
        RamariaError::io(format!("读取人格文件目录失败: {}", dir.display()), Some(e))
    })?;

    let mut files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| {
            RamariaError::io(
                format!("读取人格文件目录条目失败: {}", dir.display()),
                Some(e),
            )
        })?;
        let path = entry.path();

        // 仅处理 .toml 文件（扩展名大小写不敏感）
        if !path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("toml"))
        {
            continue;
        }

        // 按 uid 过滤（文件名不含扩展名精确匹配）
        if let Some(uid) = uid_filter {
            let stem = path
                .file_stem()
                .map(|s| s.to_string_lossy())
                .unwrap_or_default();
            if stem.as_ref() != uid {
                continue;
            }
        }

        files.push(path);
    }

    files.sort();
    Ok(files)
}

/// 从单个 `.toml` 文件导入人格（旧单文件布局兼容路径）。
///
/// 行为:
/// - 记录不存在 → 新建（source=file，config=文件全文）；
///   记录已存在 → 按 `mode` 处置（与目录导入同一实现）；
/// - 名称取文件中的 `assistant_name`，缺失时使用 `fallback_name`；
/// - 读取 / 查询 / 写入失败转为 `Failed` 条目（不上抛），调用方按各自现状处理日志与提示。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `path`: 人格文件路径（旧布局的文件名不携带 uid，uid 由调用方显式给出）。
/// - `uid`: 目标人格 uid（旧单文件布局使用固定的 `rama-0001`）。
/// - `fallback_name`: `assistant_name` 缺失时的名称兜底。
/// - `mode`: 记录已存在时的处置模式（见 [`PersonaLoadMode`]）。
///
/// 返回:
/// - 本文件的处理结果（新建 / 更新 / 跳过 / 失败）。
pub(crate) async fn load_file(
    engine: &Engine,
    path: &Path,
    uid: &str,
    fallback_name: &str,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    import_single_file(engine.storage_ref(), path, uid, Some(fallback_name), mode).await
}

/// 处理单个 `.toml` 文件（文件名（不含扩展名）= persona uid；失败转为结果条目，不上抛）。
///
/// 说明:
/// - `mode` 仅作用于"记录已存在"分支：创建或更新（同步名称与配置）或
///   仅创建缺失（跳过，不写库、不刷新 updated_at）。
async fn load_single_file(
    storage: &Arc<dyn StorageBackend>,
    path: &Path,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    let file_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    // 文件名（不含扩展名）= persona uid
    let Some(uid) = path
        .file_stem()
        .and_then(|s| s.to_str())
        .map(str::to_string)
    else {
        return PersonaFileOutcome {
            uid: file_label.clone(),
            action: PersonaFileAction::Failed,
            message: format!(
                "无法从文件名提取 UID: {file_label}。文件名必须为 <uid>.toml 格式（如 rama-0001.toml）"
            ),
        };
    };

    import_single_file(storage, path, &uid, None, mode).await
}

/// 从单个文件导入人格（显式 uid；读取 / 名称提取 / 已存在处置的共用实现）。
///
/// 说明:
/// - `fallback_name`: 文件缺少 `assistant_name` 时的名称兜底；`None` 回退 uid；
/// - 单文件失败（读取 / 查询 / 写入）转为 `Failed` 条目，不上抛。
async fn import_single_file(
    storage: &Arc<dyn StorageBackend>,
    path: &Path,
    uid: &str,
    fallback_name: Option<&str>,
    mode: PersonaLoadMode,
) -> PersonaFileOutcome {
    let file_label = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());

    let content = match std::fs::read_to_string(path) {
        Ok(content) => content,
        Err(e) => {
            tracing::warn!(uid = %mask_id(uid), error = %e, "读取人格文件失败，跳过该文件");
            return PersonaFileOutcome {
                uid: uid.to_string(),
                action: PersonaFileAction::Failed,
                message: format!("读取文件失败: {file_label}: {e}"),
            };
        }
    };

    // 名称：assistant_name 缺失时按调用方口径回退（目录导入缺省回退 uid）
    let name = extract_assistant_name(&content)
        .unwrap_or_else(|| fallback_name.unwrap_or(uid).to_string());

    match storage.get_persona_by_uid(uid).await {
        Ok(Some(_)) => match mode {
            PersonaLoadMode::CreateMissing => {
                tracing::debug!(uid = %mask_id(uid), "人格已存在，按仅创建缺失模式跳过（未更新）");
                let message = format!("已跳过 persona: {uid} ({name})（已存在）");
                PersonaFileOutcome {
                    uid: uid.to_string(),
                    action: PersonaFileAction::Skipped,
                    message,
                }
            }
            PersonaLoadMode::CreateOrUpdate => match storage
                .update_persona(uid, &name, None, Some(&content), None)
                .await
            {
                Ok(()) => {
                    tracing::info!(uid = %mask_id(uid), "人格文件已同步（更新）");
                    let message = format!("已更新 persona: {uid} ({name})");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Updated,
                        message,
                    }
                }
                Err(e) => {
                    tracing::warn!(uid = %mask_id(uid), error = %e, "更新 persona 失败");
                    let message = format!("更新 persona 失败: {uid}: {e}");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Failed,
                        message,
                    }
                }
            },
        },
        Ok(None) => {
            let kind = PersonaKind::from_uid(uid);
            let mut persona =
                Persona::new(uid.to_string(), name.clone(), kind, 1, "file".to_string());
            persona.config = Some(content);
            match storage.create_persona(&persona).await {
                Ok(_) => {
                    tracing::info!(uid = %mask_id(uid), "人格文件已加载（新建）");
                    let message = format!("已创建 persona: {uid} ({name})");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Created,
                        message,
                    }
                }
                Err(e) => {
                    tracing::warn!(uid = %mask_id(uid), error = %e, "创建 persona 失败");
                    let message = format!("创建 persona 失败: {uid}: {e}");
                    PersonaFileOutcome {
                        uid: uid.to_string(),
                        action: PersonaFileAction::Failed,
                        message,
                    }
                }
            }
        }
        Err(e) => {
            tracing::warn!(uid = %mask_id(uid), error = %e, "查询已有 persona 失败");
            let message = format!("查询已有 persona 失败: {uid}: {e}");
            PersonaFileOutcome {
                uid: uid.to_string(),
                action: PersonaFileAction::Failed,
                message,
            }
        }
    }
}

/// 从人格文件文本中提取 `assistant_name` 字段值。
///
/// 说明:
/// - 行级轻量解析（不引入 TOML 解析器）：跳过空行 / 注释行 / 分组行；
/// - 支持引号（单 / 双）与裸值两种写法；键名后必须紧跟 `=`（避免
///   `assistant_name_extra` 之类的键被误匹配）；
/// - 提取失败返回 None，调用方回退使用 uid。
fn extract_assistant_name(content: &str) -> Option<String> {
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('[') {
            continue;
        }

        let Some(rest) = trimmed.strip_prefix("assistant_name") else {
            continue;
        };
        let Some(rest) = rest.trim_start().strip_prefix('=') else {
            continue;
        };
        let value = rest.trim();
        if value.is_empty() {
            continue;
        }

        // 引号包裹：去一层引号；裸值原样返回
        for quote in ['"', '\''] {
            if value.len() >= 2 && value.starts_with(quote) && value.ends_with(quote) {
                return Some(value[1..value.len() - 1].to_string());
            }
        }
        return Some(value.to_string());
    }
    None
}

// =========================================================
// regenerate_import_l1（人格 L1 重生成）
// =========================================================

/// 外层连续失败阈值：单 session 内部已有重试与退避，外层连续 3 次失败即判定 LLM 不可用。
const MAX_CONSECUTIVE_L1_FAILURES: u32 = 3;

/// 人格 L1 重生成结果（供宿主构造用户提示与统计展示）。
///
/// 字段约定:
/// - `l1_regenerated` / `l1_failed`: 生成成功 / 失败的会话数（单会话内部重试耗尽后才计入失败）。
/// - `total_sessions`: 参与重生成的会话总数（含跳过与未处理会话）。
/// - `early_terminated`: 是否因连续失败提前终止。
/// - `remaining_skipped`: 提前终止时未处理的会话数（未提前终止为 0）。
/// - `message`: 面向用户的提示文案。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PersonaRegenerateOutcome {
    pub l1_regenerated: usize,
    pub l1_failed: usize,
    pub total_sessions: usize,
    pub early_terminated: bool,
    pub remaining_skipped: usize,
    pub message: String,
}

/// 为某人格的导入会话重新生成 L1 摘要（导入失败后的离线重建路径）。
///
/// 流程:
/// 1. 校验 UID 非空并确认人格存在（否则返回业务校验错误）；
/// 2. 全量枚举该人格消息并推导会话列表（去重按消息枚举顺序保留首次出现，顺序确定）；
/// 3. 逐会话按单段口径重生成 L1，连续失败达 [`MAX_CONSECUTIVE_L1_FAILURES`] 次判定 LLM 不可用并提前终止；
/// 4. 按成功 / 部分失败 / 提前终止三个分支构造提示文案。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `persona_uid`: 目标人格 UID。
///
/// 返回:
/// - 计数与提示文案；空 UID 与人格不存在返回 `Validation` 错误。
///
/// 说明:
/// - L2/L3 级联不在本用例内触发：宿主拿到结果后自行触发 [`Engine::trigger_l2_check`]
///   （提示文案中的"L2/L3 正在后台处理中"即指该宿主行为，避免阻塞当前调用）。
/// - 幂等：会话已有目标人格的 L1 时按跳过处理（不计成功 / 失败，也不影响连续失败计数）。
pub(crate) async fn regenerate_import_l1(
    engine: &Engine,
    persona_uid: &str,
) -> RamariaResult<PersonaRegenerateOutcome> {
    if persona_uid.trim().is_empty() {
        return Err(RamariaError::validation("人格 UID 不能为空"));
    }

    let storage = engine.storage_ref();
    if storage.get_persona_by_uid(persona_uid).await?.is_none() {
        return Err(RamariaError::validation(format!(
            "人格不存在: uid={persona_uid}"
        )));
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        "重新生成导入会话的 L1 摘要"
    );

    // 离线重建路径：必须覆盖该人格的全部会话，故全量枚举其消息（不做截断）；
    // 若将来出现 persona 消息的浏览 / 展示需求，须另走分页查询。
    let messages = storage.list_messages_by_persona(persona_uid).await?;

    // 会话去重：按消息枚举顺序保留首次出现（确定性顺序便于复现），
    // 不使用 HashSet 的迭代顺序，避免会话处理顺序随哈希随机化漂移。
    let mut seen_sessions = HashSet::new();
    let mut session_ids: Vec<Uuid> = Vec::new();
    for message in &messages {
        if seen_sessions.insert(message.session_id) {
            session_ids.push(message.session_id);
        }
    }

    if session_ids.is_empty() {
        return Ok(PersonaRegenerateOutcome {
            l1_regenerated: 0,
            l1_failed: 0,
            total_sessions: 0,
            early_terminated: false,
            remaining_skipped: 0,
            message: "该人格没有关联的导入消息，无需处理。".to_string(),
        });
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        session_count = session_ids.len(),
        message_count = messages.len(),
        "找到关联的导入 session，开始重新生成 L1"
    );

    let total = session_ids.len();
    let mut l1_regenerated = 0usize;
    let mut l1_failed = 0usize;
    let mut consecutive_failures: u32 = 0;
    let mut early_terminated = false;
    let mut remaining_skipped = 0usize;

    for (idx, sid) in session_ids.iter().enumerate() {
        match engine
            .regenerate_l1_no_cascade(*sid, Some(persona_uid), None, None)
            .await
        {
            Ok(Some(_)) => {
                l1_regenerated += 1;
                consecutive_failures = 0;
                tracing::debug!(session_id = %sid, "L1 重新生成成功");
            }
            Ok(None) => {
                // 会话已有目标人格的 L1：幂等跳过，不计成功 / 失败，也不影响连续失败计数
                tracing::debug!(session_id = %sid, "L1 无需生成，跳过");
            }
            Err(e) => {
                l1_failed += 1;
                consecutive_failures += 1;
                tracing::warn!(
                    session_id = %sid,
                    error = %e,
                    consecutive_failures,
                    "L1 重新生成失败"
                );

                if consecutive_failures >= MAX_CONSECUTIVE_L1_FAILURES {
                    remaining_skipped = total.saturating_sub(idx + 1);
                    tracing::warn!(
                        persona_uid = %mask_id(persona_uid),
                        consecutive_failures,
                        l1_regenerated,
                        l1_failed,
                        remaining_skipped,
                        "L1 连续失败达到上限，判定 LLM 不可用，跳过剩余会话"
                    );
                    early_terminated = true;
                    break;
                }
            }
        }
    }

    tracing::info!(
        persona_uid = %mask_id(persona_uid),
        l1_regenerated,
        l1_failed,
        total,
        early_terminated,
        remaining_skipped,
        "L1 重新生成完成"
    );

    let message = if early_terminated {
        format!(
            "L1 连续失败 {MAX_CONSECUTIVE_L1_FAILURES} 次，已提前终止。成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接后重试。剩余 {remaining_skipped} 个 session 未处理。"
        )
    } else if l1_failed > 0 {
        format!(
            "L1 重新生成完成: 成功 {l1_regenerated}/{total}, 失败 {l1_failed}。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
        )
    } else {
        format!("L1 全部重新生成成功 ({l1_regenerated}/{total})。L2/L3 正在后台处理中...")
    };

    Ok(PersonaRegenerateOutcome {
        l1_regenerated,
        l1_failed,
        total_sessions: total,
        early_terminated,
        remaining_skipped,
        message,
    })
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::{
        L1_JSON_REPLY, ScriptedLlm, engine_with_db, engine_with_failing_llm, engine_with_l1_reply,
        engine_with_shared_scripted_llm, seed_persona, seed_session_with_messages,
    };
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::traits::StoreCrud;
    use ramaria_core::types::MemoryL1;
    use ramaria_storage::SqliteStorage;
    use std::sync::Arc;

    /// 造一个"已有目标人格 L1"的会话（带消息；供幂等跳过路径用例）。
    async fn seed_session_with_persona_l1(
        storage: &SqliteStorage,
        persona: &str,
        count: usize,
        base_ts: i64,
    ) -> Uuid {
        let session = seed_session_with_messages(storage, persona, count, base_ts).await;
        let mut l1 = MemoryL1::new(session, "既有摘要".to_string(), None);
        l1.persona_uid = Some(persona.to_string());
        storage
            .save_memory_l1(&l1)
            .await
            .expect("写入既有 L1 应成功");
        session
    }

    /// 空 UID（含纯空白）：返回业务校验错误。
    #[tokio::test]
    async fn regenerate_rejects_blank_uid() {
        let (engine, _storage, dir) = engine_with_db("persona-regen-blank-uid").await;

        let err = engine
            .regenerate_persona_l1("   ")
            .await
            .expect_err("空 UID 应返回错误");
        assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
        assert!(
            err.to_string().contains("人格 UID 不能为空"),
            "错误文案应提示 UID 为空: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 人格不存在：返回业务校验错误（文案含目标 UID）。
    #[tokio::test]
    async fn regenerate_rejects_missing_persona() {
        let (engine, storage, dir) = engine_with_db("persona-regen-missing").await;
        seed_persona(&storage, "char-0001").await;

        let err = engine
            .regenerate_persona_l1("char-missing")
            .await
            .expect_err("人格不存在应返回错误");
        assert_eq!(err.category(), "validation", "应为业务校验错误: {err}");
        assert!(
            err.to_string().contains("人格不存在: uid=char-missing"),
            "错误文案应含目标 UID: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 该人格无消息：返回零计数与"无需处理"提示，不触达 LLM。
    #[tokio::test]
    async fn regenerate_returns_noop_without_messages() {
        let (engine, storage, dir) =
            engine_with_l1_reply("persona-regen-noop", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("无消息应正常返回");
        assert_eq!(outcome.total_sessions, 0);
        assert_eq!(outcome.l1_regenerated, 0);
        assert_eq!(outcome.l1_failed, 0);
        assert!(!outcome.early_terminated);
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(outcome.message, "该人格没有关联的导入消息，无需处理。");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 多会话全部成功：逐会话产出绑定人格的 L1，计数与提示为全成功分支。
    #[tokio::test]
    async fn regenerate_all_sessions_succeed() {
        let (engine, storage, dir) =
            engine_with_l1_reply("persona-regen-success", L1_JSON_REPLY).await;
        seed_persona(&storage, "char-0001").await;
        let session_a = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let session_b = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("重生成应成功");
        assert_eq!(outcome.total_sessions, 2);
        assert_eq!(outcome.l1_regenerated, 2);
        assert_eq!(outcome.l1_failed, 0);
        assert!(!outcome.early_terminated);
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(
            outcome.message,
            "L1 全部重新生成成功 (2/2)。L2/L3 正在后台处理中..."
        );

        for session in [session_a, session_b] {
            let l1_list = storage
                .list_memory_l1(session)
                .await
                .expect("读取 L1 应成功");
            assert_eq!(l1_list.len(), 1, "每个会话应恰有一条 L1");
            assert_eq!(
                l1_list[0].persona_uid.as_deref(),
                Some("char-0001"),
                "L1 应绑定目标人格"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 部分失败未达早停阈值：成功 / 失败计数如实上报，提示为失败分支。
    #[tokio::test]
    async fn regenerate_reports_partial_failure_without_early_stop() {
        // 脚本队列仅一条有效回复：处理顺序在前的会话成功；其后的会话耗尽队列后
        // 拿到的空回复解析失败，内部重试均失败 → 计入失败，但未达连续失败阈值。
        let llm = Arc::new(ScriptedLlm::replies(&[L1_JSON_REPLY]));
        let (engine, storage, dir) = engine_with_shared_scripted_llm(
            "persona-regen-partial",
            llm,
            RamariaConfig::default(),
            None,
        )
        .await;
        seed_persona(&storage, "char-0001").await;
        let ok_session = seed_session_with_messages(&storage, "char-0001", 2, 2_000).await;
        let fail_session = seed_session_with_messages(&storage, "char-0001", 2, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("部分失败应正常返回");
        assert_eq!(outcome.total_sessions, 2);
        assert_eq!(outcome.l1_regenerated, 1);
        assert_eq!(outcome.l1_failed, 1);
        assert!(!outcome.early_terminated, "失败未达阈值不应提前终止");
        assert_eq!(outcome.remaining_skipped, 0);
        assert_eq!(
            outcome.message,
            "L1 重新生成完成: 成功 1/2, 失败 1。请确认 LLM 模型已连接。L2/L3 正在后台处理中..."
        );

        assert_eq!(
            storage
                .list_memory_l1(ok_session)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "成功会话应产出 L1"
        );
        assert!(
            storage
                .list_memory_l1(fail_session)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "失败会话不应残留 L1"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 连续失败达到阈值：提前终止并报告跳过数量，跳过的会话未被处理。
    #[tokio::test]
    async fn regenerate_stops_after_consecutive_failures() {
        let (engine, storage, dir) = engine_with_failing_llm("persona-regen-early-stop").await;
        seed_persona(&storage, "char-0001").await;
        let session_1 = seed_session_with_messages(&storage, "char-0001", 3, 4_000).await;
        let session_2 = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
        let session_3 = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let session_skipped = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("失败路径应返回结果而非错误");
        assert_eq!(outcome.total_sessions, 4);
        assert_eq!(outcome.l1_regenerated, 0);
        assert_eq!(outcome.l1_failed, 3, "连续 3 次失败后应停止");
        assert!(outcome.early_terminated, "应提前终止");
        assert_eq!(outcome.remaining_skipped, 1, "应跳过剩余 1 个会话");
        assert_eq!(
            outcome.message,
            "L1 连续失败 3 次，已提前终止。成功 0/4, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
        );

        for session in [session_1, session_2, session_3] {
            assert!(
                storage
                    .list_memory_l1(session)
                    .await
                    .expect("读取 L1 应成功")
                    .is_empty(),
                "失败的会话不应残留 L1"
            );
        }
        assert!(
            storage
                .list_memory_l1(session_skipped)
                .await
                .expect("读取 L1 应成功")
                .is_empty(),
            "提前终止后跳过的会话不应被处理"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 幂等跳过（已有目标人格 L1）夹在失败之间：不重置连续失败计数，也不计入成功。
    #[tokio::test]
    async fn regenerate_skip_does_not_reset_failure_streak() {
        let (engine, storage, dir) = engine_with_failing_llm("persona-regen-skip-streak").await;
        seed_persona(&storage, "char-0001").await;
        // 处理顺序按消息时间倒序：失败、跳过、失败、失败、未处理
        let fail_a = seed_session_with_messages(&storage, "char-0001", 3, 5_000).await;
        let skipped = seed_session_with_persona_l1(&storage, "char-0001", 2, 4_000).await;
        let fail_b = seed_session_with_messages(&storage, "char-0001", 3, 3_000).await;
        let fail_c = seed_session_with_messages(&storage, "char-0001", 3, 2_000).await;
        let fail_unprocessed = seed_session_with_messages(&storage, "char-0001", 3, 1_000).await;

        let outcome = engine
            .regenerate_persona_l1("char-0001")
            .await
            .expect("失败路径应返回结果而非错误");
        assert_eq!(outcome.total_sessions, 5);
        assert_eq!(outcome.l1_regenerated, 0, "跳过不计入成功");
        assert_eq!(
            outcome.l1_failed, 3,
            "跳过不重置连续失败计数：第 4 个失败不应发生"
        );
        assert!(outcome.early_terminated, "应在第 3 次连续失败时提前终止");
        assert_eq!(outcome.remaining_skipped, 1);
        assert_eq!(
            outcome.message,
            "L1 连续失败 3 次，已提前终止。成功 0/5, 失败 3。请确认 LLM 模型已连接后重试。剩余 1 个 session 未处理。"
        );

        assert_eq!(
            storage
                .list_memory_l1(skipped)
                .await
                .expect("读取 L1 应成功")
                .len(),
            1,
            "跳过会话应保留既有 L1"
        );
        for session in [fail_a, fail_b, fail_c, fail_unprocessed] {
            assert!(
                storage
                    .list_memory_l1(session)
                    .await
                    .expect("读取 L1 应成功")
                    .is_empty(),
                "失败 / 未处理会话不应有 L1"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 全字段列表：字段逐项映射（含 ref_id / avatar / config / description / 时间戳）。
    #[tokio::test]
    async fn list_full_maps_all_fields() {
        let (engine, storage, dir) = engine_with_db("persona-full").await;

        let mut persona = Persona::new(
            "char-0001".to_string(),
            "小林".to_string(),
            PersonaKind::Char,
            1,
            "qq".to_string(),
        );
        persona.ref_id = Some("qq-123456".to_string());
        persona.avatar = Some("avatar.png".to_string());
        persona.config = Some("assistant_name = \"小林\"".to_string());
        persona.description = Some("大学同学".to_string());
        storage
            .create_persona(&persona)
            .await
            .expect("插入 persona 应成功");

        let views = engine.persona_list_full().await.expect("列表应成功");
        assert_eq!(views.len(), 1);
        let view = &views[0];
        assert_eq!(view.uid, "char-0001");
        assert_eq!(view.name, "小林");
        assert_eq!(view.kind, "char");
        assert_eq!(view.source, "qq");
        assert_eq!(view.ref_id.as_deref(), Some("qq-123456"));
        assert_eq!(view.avatar.as_deref(), Some("avatar.png"));
        assert_eq!(view.config.as_deref(), Some("assistant_name = \"小林\""));
        assert_eq!(view.description.as_deref(), Some("大学同学"));
        assert!(view.is_active);
        assert!(view.created_at > 0);
        assert!(view.updated_at >= view.created_at);

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 信息更新：提供字段覆盖、未提供字段保持、空描述清空、空 uid / 不存在校验。
    #[tokio::test]
    async fn update_info_applies_partial_changes() {
        let (engine, storage, dir) = engine_with_db("persona-update").await;
        seed_persona(&storage, "char-0001").await;

        let updated = engine
            .persona_update_info(
                "char-0001",
                PersonaUpdateRequest {
                    name: Some("小林".to_string()),
                    avatar: Some("avatar.png".to_string()),
                    description: Some("大学同学".to_string()),
                },
            )
            .await
            .expect("更新应成功");
        assert_eq!(updated.name, "小林");
        assert_eq!(updated.avatar.as_deref(), Some("avatar.png"));
        assert_eq!(updated.description.as_deref(), Some("大学同学"));

        // 只改描述：名称 / 头像保持旧值
        let updated = engine
            .persona_update_info(
                "char-0001",
                PersonaUpdateRequest {
                    name: None,
                    avatar: None,
                    description: Some("旧同学".to_string()),
                },
            )
            .await
            .expect("更新应成功");
        assert_eq!(updated.name, "小林", "未提供的名称保持旧值");
        assert_eq!(updated.avatar.as_deref(), Some("avatar.png"));
        assert_eq!(updated.description.as_deref(), Some("旧同学"));

        // 空描述：清空（与 None 行为不同）
        let updated = engine
            .persona_update_info(
                "char-0001",
                PersonaUpdateRequest {
                    name: None,
                    avatar: None,
                    description: Some(String::new()),
                },
            )
            .await
            .expect("更新应成功");
        assert_eq!(updated.description.as_deref(), Some(""));

        // 空 uid / 不存在人格：校验错误
        let err = engine
            .persona_update_info("  ", PersonaUpdateRequest::default())
            .await
            .expect_err("空 uid 应报错");
        assert_eq!(err.category(), "validation");
        let err = engine
            .persona_update_info("char-missing", PersonaUpdateRequest::default())
            .await
            .expect_err("人格不存在应报错");
        assert_eq!(err.category(), "validation");
        assert!(
            err.to_string().contains("人格不存在: uid=char-missing"),
            "错误文案应含目标 uid: {err}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 文件导入：新建 / 已存在更新两条路径、缺失名称回退、uid 过滤、目录缺失报错。
    #[tokio::test]
    async fn load_from_dir_creates_and_updates() {
        let (engine, storage, dir) = engine_with_db("persona-load").await;
        let personas_dir = dir.join("personas");
        std::fs::create_dir_all(&personas_dir).expect("创建人格目录应成功");

        std::fs::write(
            personas_dir.join("char-0001.toml"),
            "assistant_name = \"小林\"\n[blocks]\nA_persona = \"\"\"\n正文\n\"\"\"\n",
        )
        .expect("写入人格文件应成功");
        // 缺失 assistant_name → 名称回退 uid
        std::fs::write(
            personas_dir.join("char-0002.toml"),
            "[blocks]\nA_persona = \"\"\"\n无名称\n\"\"\"\n",
        )
        .expect("写入人格文件应成功");
        // 非 toml 文件不参与导入
        std::fs::write(personas_dir.join("readme.txt"), "not toml").expect("写入说明文件应成功");

        let outcomes = engine
            .persona_load_from_dir(&personas_dir, None, PersonaLoadMode::CreateOrUpdate)
            .await
            .expect("导入应成功");
        assert_eq!(outcomes.len(), 2, "仅处理 .toml 文件");
        assert_eq!(outcomes[0].uid, "char-0001", "按路径排序处理");
        assert_eq!(outcomes[0].action, PersonaFileAction::Created);
        assert_eq!(outcomes[1].uid, "char-0002");
        assert_eq!(outcomes[1].action, PersonaFileAction::Created);

        let created = storage
            .get_persona_by_uid("char-0001")
            .await
            .expect("查询应成功")
            .expect("应已创建");
        assert_eq!(created.name, "小林");
        assert_eq!(created.kind, PersonaKind::Char);
        assert_eq!(created.source, "file");
        assert!(
            created.config.as_deref().unwrap_or("").contains("正文"),
            "config 应保存文件全文"
        );
        let fallback = storage
            .get_persona_by_uid("char-0002")
            .await
            .expect("查询应成功")
            .expect("应已创建");
        assert_eq!(fallback.name, "char-0002", "缺失 assistant_name 回退 uid");

        // 已存在 → 更新：名称与配置同步，其他字段保持
        storage
            .update_persona("char-0001", "小林", Some("avatar.png"), None, Some("描述"))
            .await
            .expect("预置字段应成功");
        std::fs::write(
            personas_dir.join("char-0001.toml"),
            "assistant_name = \"小林酱\"\n[blocks]\nA_persona = \"\"\"\n新正文\n\"\"\"\n",
        )
        .expect("写入人格文件应成功");

        let outcomes = engine
            .persona_load_from_dir(
                &personas_dir,
                Some("char-0001"),
                PersonaLoadMode::CreateOrUpdate,
            )
            .await
            .expect("导入应成功");
        assert_eq!(outcomes.len(), 1, "uid 过滤只处理目标文件");
        assert_eq!(outcomes[0].uid, "char-0001");
        assert_eq!(outcomes[0].action, PersonaFileAction::Updated);

        let updated = storage
            .get_persona_by_uid("char-0001")
            .await
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(updated.name, "小林酱");
        assert!(
            updated.config.as_deref().unwrap_or("").contains("新正文"),
            "配置应同步为文件内容"
        );
        assert_eq!(
            updated.avatar.as_deref(),
            Some("avatar.png"),
            "头像等字段保持"
        );
        assert_eq!(updated.description.as_deref(), Some("描述"), "描述保持");

        // 目录不存在：Io 错误（目录解析由调用方负责）
        let err = engine
            .persona_load_from_dir(&dir.join("missing"), None, PersonaLoadMode::CreateOrUpdate)
            .await
            .expect_err("目录不存在应报错");
        assert_eq!(err.category(), "io");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 仅创建缺失模式：已存在记录不被改写（名称 / 配置 / updated_at 均保持），
    /// 缺失记录照常创建。
    #[tokio::test]
    async fn load_from_dir_create_missing_skips_existing() {
        let (engine, storage, dir) = engine_with_db("persona-load-skip").await;
        let personas_dir = dir.join("personas");
        std::fs::create_dir_all(&personas_dir).expect("创建人格目录应成功");

        // 预置已存在记录（名称 / 配置与随后写入的文件内容不同）
        let mut persona = Persona::new(
            "char-0001".to_string(),
            "旧名称".to_string(),
            PersonaKind::Char,
            1,
            "file".to_string(),
        );
        persona.config = Some("assistant_name = \"旧名称\"\n".to_string());
        storage
            .create_persona(&persona)
            .await
            .expect("插入 persona 应成功");
        let before = storage
            .get_persona_by_uid("char-0001")
            .await
            .expect("查询应成功")
            .expect("应存在");

        std::fs::write(
            personas_dir.join("char-0001.toml"),
            "assistant_name = \"新名称\"\n[blocks]\nA_persona = \"\"\"\n新正文\n\"\"\"\n",
        )
        .expect("写入人格文件应成功");
        std::fs::write(
            personas_dir.join("char-0002.toml"),
            "assistant_name = \"小新\"\n",
        )
        .expect("写入人格文件应成功");

        let outcomes = engine
            .persona_load_from_dir(&personas_dir, None, PersonaLoadMode::CreateMissing)
            .await
            .expect("导入应成功");
        assert_eq!(outcomes.len(), 2);
        assert_eq!(outcomes[0].uid, "char-0001");
        assert_eq!(
            outcomes[0].action,
            PersonaFileAction::Skipped,
            "已存在记录应跳过"
        );
        assert_eq!(outcomes[1].action, PersonaFileAction::Created);

        // 已存在记录未被改写
        let after = storage
            .get_persona_by_uid("char-0001")
            .await
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(after.name, "旧名称", "名称不应被改写");
        assert_eq!(
            after.config.as_deref(),
            Some("assistant_name = \"旧名称\"\n"),
            "配置不应被改写"
        );
        assert_eq!(after.updated_at, before.updated_at, "updated_at 不应刷新");

        // 缺失记录正常创建
        let created = storage
            .get_persona_by_uid("char-0002")
            .await
            .expect("查询应成功")
            .expect("应已创建");
        assert_eq!(created.name, "小新");
        assert_eq!(created.source, "file");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 单文件导入（显式 uid）：创建 / 幂等跳过 / 名称兜底 / 读取失败条目。
    #[tokio::test]
    async fn load_file_uses_explicit_uid_and_name_fallback() {
        let (engine, storage, dir) = engine_with_db("persona-load-file").await;
        let legacy_path = dir.join("persona.toml");

        // 按显式 uid 创建（文件名不参与 uid 解析），名称取文件中的 assistant_name
        std::fs::write(&legacy_path, "assistant_name = \"Ramaria\"\n").expect("写入应成功");
        let outcome = engine
            .persona_load_file(
                &legacy_path,
                "rama-0001",
                "Ramaria",
                PersonaLoadMode::CreateMissing,
            )
            .await;
        assert_eq!(outcome.uid, "rama-0001");
        assert_eq!(outcome.action, PersonaFileAction::Created);
        let created = storage
            .get_persona_by_uid("rama-0001")
            .await
            .expect("查询应成功")
            .expect("应已创建");
        assert_eq!(created.name, "Ramaria");
        assert_eq!(created.kind, PersonaKind::Rama);
        assert_eq!(created.source, "file");

        // 已存在：仅创建缺失模式跳过（不写库）
        let outcome = engine
            .persona_load_file(
                &legacy_path,
                "rama-0001",
                "Ramaria",
                PersonaLoadMode::CreateMissing,
            )
            .await;
        assert_eq!(outcome.action, PersonaFileAction::Skipped);

        // 缺失 assistant_name：名称回退调用方兜底名（与目录导入的 uid 回退不同）
        let unnamed_path = dir.join("persona-unnamed.toml");
        std::fs::write(
            &unnamed_path,
            "[blocks]\nA_persona = \"\"\"\n无名称\n\"\"\"\n",
        )
        .expect("写入应成功");
        let outcome = engine
            .persona_load_file(
                &unnamed_path,
                "char-0009",
                "Ramaria",
                PersonaLoadMode::CreateMissing,
            )
            .await;
        assert_eq!(outcome.action, PersonaFileAction::Created);
        let unnamed = storage
            .get_persona_by_uid("char-0009")
            .await
            .expect("查询应成功")
            .expect("应已创建");
        assert_eq!(unnamed.name, "Ramaria", "缺失名称应回退调用方兜底名");

        // 文件不可读：失败条目（不上抛）
        let outcome = engine
            .persona_load_file(
                &dir.join("missing.toml"),
                "rama-0001",
                "Ramaria",
                PersonaLoadMode::CreateMissing,
            )
            .await;
        assert_eq!(outcome.action, PersonaFileAction::Failed);
        assert!(
            outcome.message.contains("读取文件失败"),
            "失败条目应含原因: {}",
            outcome.message
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 用户人格：首次创建、再次幂等（不重复写库）。
    #[tokio::test]
    async fn ensure_user_is_idempotent() {
        let (engine, storage, dir) = engine_with_db("persona-user").await;

        assert!(
            engine.persona_ensure_user().await.expect("创建应成功"),
            "首次应创建"
        );
        let user = storage
            .get_persona_by_uid("user-0001")
            .await
            .expect("查询应成功")
            .expect("应存在");
        assert_eq!(user.name, "用户");
        assert_eq!(user.kind, PersonaKind::User);
        assert_eq!(user.source, "system");

        assert!(
            !engine.persona_ensure_user().await.expect("重复应成功"),
            "已存在时不应重复创建"
        );
        assert_eq!(
            storage.list_personas().await.expect("列表应成功").len(),
            1,
            "重复调用不应新增记录"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
