//! crates/ramaria-memory/src/chat/material.rs - System Prompt 装配素材加载
//!
//! 设计特点:
//! - 装配素材加载：persona 结构化画像 / 事实 / 性格 / 示例 / 自动风格规则 / 知识层去重
//! - 普通装配路径：结构化素材走装配器，纯文本降级（无 persona / 冷启动）原样返回
//! - 冷启动兜底：persona 无结构化画像时经 persona.toml 组装基础 prompt（DB 配置优先，文件系统回退）
//! - 知识层去重：RAG 覆盖剔除 / 角色层重复剔除，层间仲裁（默认关闭）追加内容级去重
//! - 安全约束：不记录完整 prompt 或用户消息；日志只记数量与计数

use ramaria_core::config::{InjectionGate, LayerDedupConfig};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{PersonaExample, PersonaFact, ProfileField};

use crate::behavior::MergedDecision;
use crate::init::resolve_chat_style_rules;
use crate::prompt::builder::{
    ProactivePromptContext, PromptConfig, PromptContext, assemble_prompt,
};

use super::persona_fallback::load_persona_toml_prompt;
use super::time::now_timestamp_str;

// =========================================================
// 装配素材（共享加载，供普通/协调装配消费）
// =========================================================

/// 已加载的 System Prompt 装配素材。
///
/// 职责:
/// - 承载普通装配（`build_system_prompt`）与调用方协调装配共享的 persona 数据加载结果。
///
/// 状态:
/// - `Plain`: 纯文本 prompt（无 persona / persona.toml 冷启动兜底），无可协调注入块。
/// - `Structured`: 结构化装配上下文（装箱压缩枚举体积；可经 `render_prompt_parts`
///   拆分为固定骨架 + 注入块）。
pub enum LoadedPromptMaterial {
    /// 纯文本 prompt（无 persona / persona.toml 冷启动兜底），无可协调注入块。
    Plain(String),
    /// 结构化装配上下文（装箱压缩枚举体积；可经 `render_prompt_parts`
    /// 拆分为固定骨架 + 注入块）。
    Structured(Box<PromptContext>, PromptConfig),
}

/// System Prompt 装配素材加载的完整输入集合。
///
/// 职责:
/// - 把素材加载所需的人格数据、检索产物、注入闸门与去重配置收拢为单一入参，
///   供普通装配与调用方协调装配复用。
///
/// 字段约定:
/// - `persona_uid`: 人格标识（None 表示 rama 自身，回退 "rama-0001"）。
/// - `recent_summaries`: 近期 L1 摘要列表（预格式化文本，时间降序）。
/// - `last_active_at`: 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）。
/// - `utt_context`: utt 原文片段（已按预算裁剪渲染；None 表示不注入）。
/// - `bridge_context`: 桥接内容（上一会话尾部原文，已按预算截断；None 表示不注入）。
/// - `behavior_decision`: 行为层路由合并决策（None = 未命中/关闭，不注入行为块）。
/// - `examples`: 已选好的 Few-shot 示例（由 `load_examples_for_input` 评分轮换/兜底后传入）。
/// - `max_examples`: examples 注入上限（来自生效配置 `examples.max_examples`，
///   由调用方传入以支持配置覆盖的探针场景）。
/// - `knowledge_facts`: 知识层判定器命中的 active facts（装配前经 RAG 覆盖/角色层去重，
///   空集不产生段落）。
/// - `rag_covered_labels`: RAG 摘要实际注入的文档 label 集合（`L1:{uuid}`/`L2:{id}`）。
///   知识层去重消费：同一事实已由 RAG 摘要文本覆盖则不重复注入（兜底语义不失效）。
///   空集合 = RAG 未注入/闸门关闭 → 知识卡片不去重（回退既有兜底行为）。
/// - `knowledge_budget_chars`: 知识块渲染预算（对齐 core `[knowledge].injection_budget_chars`；
///   `None` 使用 prompt 层默认预算）。
/// - `injection`: 注入层运行时间门（逐层控制 prompt 注入，探针消融专用）。
/// - `style_enabled`: 表达层风格子系统总开关（`[style].enabled`，主配置而非闸门）。
///   关闭时自动风格规则不加载（风格子系统"不统计/不注入"口径），与既有装配行为一致。
/// - `rag_text`: RAG 摘要实际注入文本（`memory_context`；`None` = RAG 未注入）。
///   层间仲裁开启时作为"保留参照"做内容级判重（RAG 摘要为主）。
/// - `layer_dedup`: 层间证据去重与冲突仲裁配置（`[layer_dedup]`；默认关闭 =
///   回退既有引用级去重，prompt 输出与既有版本逐字段等价）。
pub struct PromptMaterialInputs<'a> {
    /// 人格标识（None 表示 rama 自身）。
    pub persona_uid: Option<&'a str>,
    /// 近期 L1 摘要列表（预格式化文本）。
    pub recent_summaries: &'a [String],
    /// 最后活跃时间字符串。
    pub last_active_at: Option<&'a str>,
    /// utt 原文片段（None 表示不注入）。
    pub utt_context: Option<&'a str>,
    /// 桥接内容（None 表示不注入）。
    pub bridge_context: Option<&'a str>,
    /// 行为层路由合并决策（None = 未命中/关闭）。
    pub behavior_decision: Option<MergedDecision>,
    /// 已选好的 Few-shot 示例。
    pub examples: Vec<PersonaExample>,
    /// examples 注入上限。
    pub max_examples: usize,
    /// 知识层判定器命中的 active facts。
    pub knowledge_facts: Vec<PersonaFact>,
    /// RAG 摘要实际注入的文档 label 集合。
    pub rag_covered_labels: &'a [String],
    /// 知识块渲染预算（None = prompt 层默认预算）。
    pub knowledge_budget_chars: Option<usize>,
    /// 注入层运行时间门。
    pub injection: &'a InjectionGate,
    /// 表达层风格子系统总开关（`[style].enabled`）。
    pub style_enabled: bool,
    /// RAG 摘要实际注入文本（None = RAG 未注入）。
    pub rag_text: Option<&'a str>,
    /// 层间证据去重与冲突仲裁配置。
    pub layer_dedup: &'a LayerDedupConfig,
    /// 主动生成场景上下文（None = 非主动生成，Prompt 不产生主动段）。
    pub proactive: Option<ProactivePromptContext>,
}

// =========================================================
// 装配素材加载
// =========================================================

/// 加载 System Prompt 装配素材（普通 / 协调装配共享）。
///
/// 流程:
/// 1. 从 storage 加载当前 persona 的数据（persona/facts/traits/examples）。
/// 2. 注入近期 L1 摘要（跨 session 上下文）和最后活跃时间。
/// 3. 返回结构化上下文（`Structured`）或纯文本降级（`Plain`）。
///    - 无 persona → `Plain`（默认 Ramaria prompt）。
///    - persona 存在但 facts/traits 均为空且 persona.toml 可用 → `Plain`（冷启动）。
///    - 其余 → `Structured(ctx, config)`（由调用方选择普通/协调装配）。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `inputs`: 装配素材输入集合（字段语义见 `PromptMaterialInputs`）。
///
/// 降级策略:
/// - storage 读取失败 → 记录 warn 日志，使用空数据继续。
/// - persona 不存在 → 使用默认 Ramaria 身份 prompt。
/// - facts/traits/examples 为空 → 对应 Block 自动省略（由 builder 处理）。
/// - recent_summaries 为空 → Block C1 显示"首次对话"提示。
/// - behavior_decision=None → 行为块不注入（静默降级）。
///
/// 安全约束:
/// - 不在此处写入 system prompt 到日志（完整 prompt 仅发送到 LLM）。
pub async fn load_prompt_material(
    storage: &dyn StorageBackend,
    inputs: &PromptMaterialInputs<'_>,
) -> LoadedPromptMaterial {
    let actual_uid = inputs.persona_uid.unwrap_or("rama-0001");

    // 尝试加载 persona 数据
    let persona = match storage.get_persona_by_uid(actual_uid).await {
        Ok(Some(p)) => Some(p),
        Ok(None) => {
            tracing::debug!(%actual_uid, "persona 不存在，使用默认 prompt");
            None
        }
        Err(e) => {
            tracing::warn!(%actual_uid, %e, "加载 persona 失败，使用默认 prompt");
            None
        }
    };

    // 有 persona 数据时使用 5-Block 装配器
    if let Some(ref p) = persona {
        // 加载关联数据（各独立调用，失败单独降级）
        let facts = storage
            .list_facts_by_persona(&p.uid, ProfileField::BasicInfo)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(persona_uid = %p.uid, %e, "加载 facts 失败，跳过");
                Vec::new()
            });

        let traits = storage
            .list_traits_by_persona(&p.uid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(persona_uid = %p.uid, %e, "加载 traits 失败，跳过");
                Vec::new()
            });

        // 自动风格规则（表达层）：仅 [style].enabled 且注入闸门开启时加载
        // （探针消融关闭表达层时跳过加载）；
        // 数据不足/无显著项 → None（不注入，prompt 不含自动风格规则）
        let style_rule_text = if inputs.style_enabled && inputs.injection.speaking_style {
            crate::style::orchestrate::load_style_rule(storage, &p.uid)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(persona_uid = %p.uid, %e, "加载自动风格规则失败，跳过");
                    None
                })
        } else {
            None
        };

        // examples 由调用方预选后传入：
        // 注入侧按话题/情绪/长度评分轮换，并在记忆检索未命中时作风格兜底。

        // 冷启动兜底：facts/traits 均为空时，尝试加载 persona.toml
        // 优先从 DB persona.config 读取，其次回退到文件系统
        if facts.is_empty() && traits.is_empty() {
            if let Some(prompt) = load_persona_toml_prompt(p.config.as_deref()) {
                tracing::info!("使用 persona.toml 加载的系统 prompt（无结构化画像）");
                return LoadedPromptMaterial::Plain(prompt);
            }
        }

        // 知识层注入去重：RAG 摘要为主召回路径、断言知识为兜底。
        // 装配前剔除两类重复——① 来源文档已进入 RAG 覆盖集合的事实（同一事实已由
        // 摘要文本提供）；② 与角色层已知事实区同 id 的记录（角色区已展示，取后者去重）。
        // 无来源引用（手工/冷启动等）或 RAG 未覆盖的事实保留，兜底注入不失效。
        //
        // `[layer_dedup]` 开启（默认关闭 = 回退既有引用级去重路径）时，在引用级之上
        // 追加内容级去重与冲突仲裁（layer_guard）：剔除与角色区/RAG 摘要文本/行为规则
        // 内容级重复的知识卡片，产出保留方引用（证据可追溯，日志不含原文）。
        let knowledge_facts = if inputs.knowledge_facts.is_empty() {
            inputs.knowledge_facts.clone()
        } else if inputs.layer_dedup.enabled {
            use crate::prompt::layer_guard::{
                LayerGuardInput, RetentionKind, RetentionReference, arbitrate_fact_layers,
            };
            let covered: std::collections::HashSet<String> =
                inputs.rag_covered_labels.iter().cloned().collect();
            // 保留参照：RAG 摘要文本（RAG 为主）+ 行为规则 reaction（行为层优先）
            let mut references: Vec<RetentionReference> = Vec::with_capacity(2);
            if let Some(rag) = inputs.rag_text.map(str::trim).filter(|s| !s.is_empty()) {
                references.push(RetentionReference {
                    kind: RetentionKind::RagSummary,
                    text: rag,
                });
            }
            if let Some(reaction) = inputs
                .behavior_decision
                .as_ref()
                .and_then(|d| d.primary_rule.reaction.as_deref())
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                references.push(RetentionReference {
                    kind: RetentionKind::BehaviorRule,
                    text: reaction,
                });
            }
            let outcome = arbitrate_fact_layers(
                &LayerGuardInput {
                    knowledge: &inputs.knowledge_facts,
                    role: &facts,
                    references: &references,
                    rag_covered_labels: &covered,
                },
                true,
            );
            if !outcome.traces.is_empty() {
                // 只记计数/原因类别/保留方引用，不记原文全文（隐私红线）
                tracing::debug!(
                    persona_uid = %p.uid,
                    before = inputs.knowledge_facts.len(),
                    after = outcome.knowledge_facts.len(),
                    reasons = ?outcome
                        .traces
                        .iter()
                        .map(|t| t.reason.as_str())
                        .collect::<Vec<_>>(),
                    kept_refs = ?outcome
                        .traces
                        .iter()
                        .map(|t| t.kept_ref.as_str())
                        .collect::<Vec<_>>(),
                    "层间证据去重与冲突仲裁已应用"
                );
            }
            outcome.knowledge_facts
        } else {
            let covered: std::collections::HashSet<String> =
                inputs.rag_covered_labels.iter().cloned().collect();
            let deduped = crate::fact::retriever::dedup_knowledge_facts(
                &inputs.knowledge_facts,
                &covered,
                &facts,
            );
            if deduped.len() != inputs.knowledge_facts.len() {
                tracing::debug!(
                    persona_uid = %p.uid,
                    before = inputs.knowledge_facts.len(),
                    after = deduped.len(),
                    "知识层注入去重（RAG 覆盖/角色层重复剔除）"
                );
            }
            deduped
        };

        let ctx = PromptContext {
            persona: Some(p.clone()),
            facts,
            traits,
            examples: inputs.examples.clone(),
            // memory_context 由调用方在 ChatRequest 中单独注入，不在此处拼入
            memory_context: None,
            // 跨 session 上下文: 近期 L1 摘要 + 最后活跃时间
            recent_session_summaries: inputs.recent_summaries.to_vec(),
            last_active_at: inputs.last_active_at.map(|s| s.to_string()),
            knowledge_boundary: None,
            current_time_str: Some(now_timestamp_str()),
            weather: None,
            // 回复规则：显式 E_rules 优先，缺省用共享规则（与 Stage 路径同一口径）；
            // 陈述档由 builder 门控回退中性默认。
            chat_style_rules: Some(resolve_chat_style_rules(p.config.as_deref())),
            // utt 原文片段（检索层已按白名单与预算过滤，None 表示不注入）
            utt_context: inputs.utt_context.map(|s| s.to_string()),
            // 桥接内容（桥接层已按白名单与预算过滤，None 表示未启用）
            bridge_context: inputs.bridge_context.map(|s| s.to_string()),
            // 行为层路由决策（None = 未命中/关闭）
            behavior_decision: inputs.behavior_decision.clone(),
            // 知识层 active 事实（判定器命中后由调用方检索传入，装配前已
            // 按 RAG 覆盖/角色层去重；空 = 关闭/未命中/全部去重 → prompt 不含知识块）
            knowledge_facts,
            // 自动风格规则（None = 风格关闭/数据不足 → prompt 不含自动风格规则）
            style_rule_text,
            // 主动生成场景（None = 非主动路径 → prompt 不含主动段）
            proactive_context: inputs.proactive.clone(),
        };

        // examples.max_examples 经配置传播，
        // 与 `load_examples_for_input` 的预选上限保持一致（双闸门）。
        // 注入闸门映射（探针消融）：把 InjectionGate 逐子段翻译为 PromptConfig
        // 渲染开关——行为/知识在数据层已置空（behavior_decision/knowledge_facts），
        // 此处只需表达层、记忆块与全局体裁（基调）的渲染开关。
        let config = PromptConfig {
            max_examples: inputs.max_examples,
            include_examples: inputs.injection.examples,
            include_speaking_style: inputs.injection.speaking_style,
            include_narrative: inputs.injection.narrative,
            include_memory_rag: inputs.injection.memory_rag,
            include_utt: inputs.injection.utt,
            include_bridge: inputs.injection.bridge,
            // 全局社交对话基调（体裁约束，非记忆层）：默认开，probe 的
            // statement 档经闸门关闭（陈述/知识表述语域对照）。
            include_social_tone: inputs.injection.social_tone,
            // 知识块渲染预算接线：core [knowledge].injection_budget_chars
            // 默认 800 → Some(800)，与 layers 默认预算一致（行为等价）；显式值生效。
            knowledge_block_max_chars: inputs.knowledge_budget_chars,
            ..Default::default()
        };
        tracing::debug!(
            persona_uid = %p.uid,
            facts = ctx.facts.len(),
            traits = ctx.traits.len(),
            examples = ctx.examples.len(),
            "四层 System Prompt 素材已加载"
        );
        return LoadedPromptMaterial::Structured(Box::new(ctx), config);
    }

    // 降级：默认 Ramaria 基础 prompt
    tracing::info!("使用默认 Ramaria System Prompt（无 persona 数据）");
    LoadedPromptMaterial::Plain(format!(
        "你是 Ramaria，一个具有记忆能力、善解人意的 AI 助手。\n\
         你可以记住与用户的对话历史，并在后续对话中引用这些记忆。\n\
         请用自然、友好的语气回复用户。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。\n\
         当前时间：{}",
        now_timestamp_str()
    ))
}

// =========================================================
// 普通装配路径
// =========================================================

/// 构建 System Prompt（普通装配路径）。
///
/// 说明:
/// - 消费 `load_prompt_material`：结构化素材走 `assemble_prompt`，
///   纯文本降级（无 persona / 冷启动）原样返回。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `inputs`: 装配素材输入集合（字段语义见 `PromptMaterialInputs`）。
///
/// 返回:
/// - 可直接用于 LLM 请求的 System Prompt 文本。
pub async fn build_system_prompt(
    storage: &dyn StorageBackend,
    inputs: &PromptMaterialInputs<'_>,
) -> String {
    match load_prompt_material(storage, inputs).await {
        LoadedPromptMaterial::Plain(prompt) => prompt,
        LoadedPromptMaterial::Structured(ctx, config) => assemble_prompt(&ctx, &config),
    }
}
