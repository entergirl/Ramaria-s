//! crates/ramaria-memory/src/chat.rs - 在线对话装配编排模块（记忆层）
//!
//! 设计特点:
//! - 装配素材加载：persona 结构化画像 / 事实 / 性格 / 示例 / 自动风格规则 / 知识层去重
//! - 普通装配路径：结构化素材走 5-Block 装配器，纯文本降级（无 persona / 冷启动）原样返回
//! - 冷启动兜底：persona 无结构化画像时经 persona.toml 组装基础 prompt（DB 配置优先，文件系统回退）
//! - 示例预选：评分轮换 + 记忆未命中兜底；关闭时回退静态 selected 注入
//! - 与传输无关：storage / 配置经参数注入，供 app 与 service / MCP 入口同源复用
//! - 安全约束：不记录完整 prompt 或用户消息；日志只记数量与计数

use ramaria_core::config::{
    DecayConfig as CoreDecayConfig, ExamplesConfig, InjectionGate, LayerDedupConfig,
    RetrievalConfig,
};
use ramaria_core::traits::StorageBackend;
use ramaria_core::types::{MemoryL1, PersonaExample, PersonaFact, ProfileField};

use crate::behavior::MergedDecision;
use crate::bm25::DocId;
use crate::decay::DecayConfig;
use crate::init::{parse_persona_toml, resolve_chat_style_rules};
use crate::prompt::builder::{PromptConfig, PromptContext, assemble_prompt};
use crate::recall::RetrieverSource;
use crate::retriever::SearchResult;

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

// =========================================================
// 示例预选（few-shot 注入素材）
// =========================================================

/// 预选 Few-shot 示例。
///
/// 选择策略:
/// - `examples.enabled=false` → 回退：静态 `selected=1` 查询（`list_selected_examples`）。
/// - `examples.enabled=true`：
///   - 记忆检索命中（`memory_hit=true`）→ 不注入（避免与记忆内容重复）；
///   - 记忆未命中 → 从候选池按话题/情绪/长度评分轮换选择，风格兜底。
///
/// 降级:
/// - 候选池为空 / 存储失败 → 空列表（不注入）。
/// - 评分选择不满足最低条数 → 空列表（example_selector 语义，不强制凑数）。
///
/// 安全约束:
/// - 日志只记录数量，不记录示例内容。
///
/// 参数:
/// - `storage`: 存储后端。
/// - `examples_cfg`: 示例配置（`[examples]`）。
/// - `persona_uid`: 人格 UID（None 表示 rama 自身，回退 "rama-0001"）。
/// - `user_input`: 用户当前输入（话题匹配关键词来源）。
/// - `memory_hit`: 记忆检索是否命中（RAG 上下文非空）。
///
/// 返回:
/// - 注入用示例列表（最多 `[examples].max_examples` 条）。
pub async fn load_examples_for_input(
    storage: &dyn StorageBackend,
    examples_cfg: &ExamplesConfig,
    persona_uid: Option<&str>,
    user_input: &str,
    memory_hit: bool,
) -> Vec<PersonaExample> {
    use crate::prompt::example_selector::{ExampleSelector, ExampleSelectorConfig};

    let uid = persona_uid.unwrap_or("rama-0001");

    // 关闭评分轮换时的兼容路径：静态 selected 注入（无条件）
    if !examples_cfg.enabled {
        return storage
            .list_selected_examples(uid)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(persona_uid = %uid, %e, "加载 selected examples 失败，跳过");
                Vec::new()
            });
    }

    // 评分轮换路径：记忆命中不重复注入（兜底语义）
    if memory_hit {
        tracing::debug!(persona_uid = %uid, "记忆检索命中，跳过 examples 兜底注入");
        return Vec::new();
    }

    // 记忆未命中 → 候选池评分轮换（风格兜底）
    let candidates = storage.list_all_examples(uid).await.unwrap_or_else(|e| {
        tracing::warn!(persona_uid = %uid, %e, "加载 examples 候选池失败，跳过");
        Vec::new()
    });
    if candidates.is_empty() {
        tracing::debug!(persona_uid = %uid, "examples 候选池为空，跳过注入");
        return Vec::new();
    }

    let keywords = crate::prompt::example_selector::extract_keywords(user_input);
    let keyword_refs: Vec<&str> = keywords.iter().map(|s| s.as_str()).collect();
    let selector_config = ExampleSelectorConfig {
        max_examples: examples_cfg.max_examples as usize,
        ..ExampleSelectorConfig::default()
    };

    let selected = ExampleSelector::select(&candidates, &keyword_refs, 0.0, &selector_config);

    tracing::debug!(
        persona_uid = %uid,
        candidates = candidates.len(),
        selected = selected.len(),
        "examples 评分轮换完成（记忆未命中兜底注入）"
    );
    selected
}

// =========================================================
// 脉络素材（跨会话上下文）
// =========================================================

/// 脉络素材（进入 prompt 近期对话脉络块的摘要行 + 最后活跃时间）。
///
/// 职责:
/// - 承载跨 session 上下文注入所需的近期 L1 摘要文本行与最后活跃时间，
///   供 System Prompt 装配（近期对话脉络块）消费。
///
/// 字段约定:
/// - `recent_summaries`: 近期 L1 摘要列表（预格式化文本行，按加载顺序排列）。
/// - `last_active_at`: 最后活跃时间字符串（`YYYY-MM-DD HH:MM`；无摘要时为 None）。
#[derive(Debug, Clone, Default)]
pub struct NarrativeMaterial {
    /// 近期 L1 摘要列表（预格式化文本行）。
    pub recent_summaries: Vec<String>,
    /// 最后活跃时间字符串（YYYY-MM-DD HH:MM 格式）。
    pub last_active_at: Option<String>,
}

/// 加载脉络素材（在线管线脉络段的单份实现，桌面与服务入口同源复用）。
///
/// 流程:
/// - 闸门关闭（`narrative_enabled=false`）→ 直接返回空素材（不查询 retriever/storage）。
/// - `narrative_weighted=true`（加权路径）：以 `query` 为话题依据经
///   [`RetrieverSource::search_narrative`] 加权排序；检索无结果时回退最近 N 条。
/// - `narrative_weighted=false`（无条件路径）：直接取最近 N 条
///   （`list_recent_l1_by_persona`）。
///
/// 参数:
/// - `storage`: 存储后端（无条件取最近 N 条路径）。
/// - `retriever`: 检索器只读视图（加权路径；未加载按空结果降级）。
/// - `retrieval`: `[retrieval]` 配置（`narrative_weighted` / `narrative_top_k`）。
/// - `decay`: `[decay]` 配置（加权路径的时间衰减）。
/// - `narrative_enabled`: 脉络注入闸门（`[injection].narrative`；false 时直接返回空素材）。
/// - `persona_uid`: 目标人格。
/// - `query`: 当前用户输入（加权路径的相关性输入）。
///
/// 返回:
/// - 脉络素材；任何降级路径都返回空字段而非错误（调用方按空值处理）。
///
/// 降级策略:
/// - 存储读取失败 → warn 日志 + 空摘要列表（不阻塞对话）。
/// - 检索无结果（无 L1 / 索引未加载）→ 回退最近 N 条；仍为空则返回空素材。
///
/// 安全约束:
/// - 不记录完整摘要到日志；日志只记计数与是否有最后活跃时间。
pub async fn load_narrative_material<R: RetrieverSource + ?Sized>(
    storage: &dyn StorageBackend,
    retriever: &R,
    retrieval: &RetrievalConfig,
    decay: &CoreDecayConfig,
    narrative_enabled: bool,
    persona_uid: &str,
    query: &str,
) -> NarrativeMaterial {
    // 脉络注入条数下限：至少 1 条（0 会导致检索与回退路径均为空素材）
    let narrative_top_k = retrieval.narrative_top_k.max(1);
    let recent_l1 = if !narrative_enabled {
        tracing::debug!(
            persona_uid = persona_uid,
            "脉络注入闸门关闭（探针消融），跳过近期 L1 摘要加载"
        );
        Vec::new()
    } else if retrieval.narrative_weighted {
        let now = ramaria_core::types::now_ms();
        let decay_config = DecayConfig::from_core(decay, "l1");
        let narrative_results = retriever.search_narrative(
            query,
            persona_uid,
            narrative_top_k as usize,
            now,
            &decay_config,
        );
        if !narrative_results.is_empty() {
            // 加权命中 → 转回 MemoryL1（脉络行格式与"最近 N 条"路径一致，
            // 缺 time_period/atmosphere 时显示纯摘要——加权优先保证话题相关性，展示次要）
            narrative_results
                .iter()
                .filter_map(search_result_to_memory_l1)
                .collect::<Vec<MemoryL1>>()
        } else {
            // 检索无结果（无 L1 或 query 无相关性命中）→ 回退最近 N 条
            storage
                .list_recent_l1_by_persona(persona_uid, narrative_top_k)
                .await
                .unwrap_or_else(|e| {
                    tracing::warn!(
                        persona_uid = persona_uid,
                        error = %e,
                        "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                    );
                    Vec::new()
                })
        }
    } else {
        storage
            .list_recent_l1_by_persona(persona_uid, narrative_top_k)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    persona_uid = persona_uid,
                    error = %e,
                    "加载近期 L1 摘要失败，跨 session 上下文降级为空"
                );
                Vec::new()
            })
    };

    // 格式化近期摘要为可读文本行
    let recent_summaries: Vec<String> = recent_l1.iter().map(format_l1_as_context_line).collect();

    // 从最近一条 L1 的创建时间提取最后活跃时间
    let last_active_at: Option<String> = recent_l1.first().map(|l1| {
        let secs = l1.created_at / 1000;
        match chrono::DateTime::from_timestamp(secs, 0) {
            Some(dt) => dt.format("%Y-%m-%d %H:%M").to_string(),
            None => String::new(),
        }
    });

    tracing::debug!(
        persona_uid = persona_uid,
        l1_count = recent_l1.len(),
        has_last_active = last_active_at.is_some(),
        "近期 L1 摘要已加载"
    );

    NarrativeMaterial {
        recent_summaries,
        last_active_at,
    }
}

/// 将脉络加权检索的 `SearchResult` 转换为 `MemoryL1`（供脉络行格式化）。
///
/// 说明:
/// - 仅接受 L1 层结果（`DocId::L1`）；其他层（L2/图谱）不是脉络注入目标。
/// - `time_period` / `atmosphere` 在 `SearchResult` 中不承载，置 None——
///   脉络行退化为纯摘要格式（加权路径优先保证话题相关性，展示次要）。
/// - `session_id` 置 nil：脉络行只用于上下文文本展示，不参与会话归属。
fn search_result_to_memory_l1(sr: &SearchResult) -> Option<MemoryL1> {
    let id = match &sr.doc_id {
        DocId::L1(id) => *id,
        _ => return None,
    };
    Some(MemoryL1 {
        id,
        session_id: uuid::Uuid::nil(),
        summary: sr.doc_summary.clone(),
        keywords: None,
        time_period: None,
        atmosphere: None,
        valence: 0.0,
        salience: 0.5,
        absorbed: false,
        created_at: sr.created_at,
        last_accessed_at: sr.last_accessed_at,
        persona_uid: sr.persona_uid.clone(),
        context_json: None,
        situation_strength: None,
        evidence_notes: None,
        continuation: None,
    })
}

/// 把一条 L1 摘要格式化为 prompt 脉络行（含时间/氛围标注与 120 字符截断）。
///
/// 格式:
/// - 含时间段与氛围: "上午 — 讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 仅时间段: "上午 — 讨论了Python异步编程的线程安全问题。"
/// - 仅氛围: "讨论了Python异步编程的线程安全问题。氛围融洽。"
/// - 均缺失: 纯摘要文本。
///
/// 截断规则:
/// - 单条摘要最多 120 字符，超出加省略号。
///
/// 安全约束:
/// - 仅返回展示文本，不写日志。
pub fn format_l1_as_context_line(l1: &MemoryL1) -> String {
    let time_label = l1.time_period.as_deref().unwrap_or("");
    let atmosphere = l1.atmosphere.as_deref().unwrap_or("");

    let base = if !time_label.is_empty() && !atmosphere.is_empty() {
        format!("{time_label} — {}。氛围{atmosphere}。", l1.summary)
    } else if !time_label.is_empty() {
        format!("{time_label} — {}", l1.summary)
    } else if !atmosphere.is_empty() {
        format!("{}。氛围{atmosphere}。", l1.summary)
    } else {
        l1.summary.clone()
    };

    // 截断到 120 字符（统一字符边界工具，预算内含省略号）
    ramaria_core::text::truncate_chars(&base, 120)
}

// =========================================================
// persona.toml 冷启动兜底
// =========================================================

/// 尝试加载 persona.toml 并构建有温度的基础 system prompt。
///
/// 数据来源优先级:
/// 1. `db_config`: 从 DB persona.config 中读取的 TOML 内容（setup 时写入）
/// 2. 文件系统回退: `../config/personas/rama-0001.toml`，其次旧路径
///    `../config/persona.toml`（未迁移的旧安装兼容回退）
///
/// 参数:
/// - `db_config`: DB persona.config 内容（None 时直接走文件系统回退）。
///
/// 返回:
/// - `Some(prompt)`: 由 `A_persona` + `E_rules`（显式优先，缺省共享规则）组装的基础 prompt。
/// - `None`: 解析失败 / 文件缺失 —— 由上层降级到默认 Ramaria prompt。
pub fn load_persona_toml_prompt(db_config: Option<&str>) -> Option<String> {
    let content = if let Some(cfg) = db_config {
        // 优先使用 DB 中的 persona.toml 内容
        if cfg.contains("[identity]") || cfg.contains("[blocks]") {
            tracing::debug!("从 DB persona.config 加载 persona.toml");
            cfg.to_string()
        } else {
            // config 字段是其他 JSON 格式，回退到文件系统
            read_persona_toml_from_fs()?
        }
    } else {
        read_persona_toml_from_fs()?
    };

    let parsed = match parse_persona_toml(&content) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(%e, "persona.toml 解析失败");
            return None;
        }
    };

    let persona_block = parsed
        .blocks
        .iter()
        .find(|(k, _)| k == "A_persona")
        .map(|(_, v)| v.as_str())
        .unwrap_or("");

    // 回复规则：显式 E_rules 优先，缺省回退共享规则（与生产装配路径同一口径）
    let rules_block = resolve_chat_style_rules(Some(content.as_str()));

    let name = &parsed.assistant_name;
    let time_str = now_timestamp_str();

    Some(format!(
        "你的名字是{name}。\n\n{persona_block}\n\n回复规则:\n{rules_block}\n\n\
         当前时间：{time_str}\n\n\
         你可以记住与用户的对话历史。如果用户提到之前聊过的内容，\
         请结合记忆上下文给出更有针对性的回复。"
    ))
}

/// 文件系统回退: 优先尝试新路径 `../config/personas/rama-0001.toml`，其次旧路径 `../config/persona.toml`。
///
/// 说明:
/// - 新路径为目录扫描模式，每文件 = 一个 persona。
/// - 旧路径保留作为兼容回退，供未迁移的旧安装使用。
/// - 相对路径以进程工作目录为基准（与调用方 crate 位置无关）。
fn read_persona_toml_from_fs() -> Option<String> {
    // 优先尝试新路径
    let new_path = "../config/personas/rama-0001.toml";
    if let Ok(c) = std::fs::read_to_string(new_path) {
        tracing::debug!(%new_path, "从文件系统加载 persona.toml (新路径)");
        return Some(c);
    }

    // 回退到旧路径
    let old_path = "../config/persona.toml";
    match std::fs::read_to_string(old_path) {
        Ok(c) => {
            tracing::debug!(%old_path, "从文件系统加载 persona.toml (旧路径兼容)");
            Some(c)
        }
        Err(e) => {
            tracing::debug!(%old_path, %e, "persona.toml 文件系统回退失败");
            None
        }
    }
}

// =========================================================
// 共享时间格式化
// =========================================================

/// 返回当前时间的 `YYYY-MM-DD HH:MM` 字符串（本地时区）。
///
/// 用途: 消息时间戳、System Prompt 当前时间等共享格式化。
pub fn now_timestamp_str() -> String {
    chrono::Local::now().format("%Y-%m-%d %H:%M").to_string()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, RwLock};

    use crate::retriever::{L1DocView, Retriever};
    use ramaria_core::config::RamariaConfig;
    use ramaria_core::types::{Persona, PersonaKind};
    use uuid::Uuid;

    /// 用真实 SQLite（临时文件库）构造存储后端（脉络素材只需 L1 读写路径）。
    async fn test_storage(tag: &str) -> Arc<dyn StorageBackend> {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("系统时间应可读")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("ramaria-chat-{tag}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("临时目录创建应成功");
        let pool = ramaria_storage::database::init_pool(Some(dir.join("assistant.db")))
            .await
            .expect("测试库初始化应成功");
        Arc::new(ramaria_storage::SqliteStorage::new(pool))
    }

    /// 创建指定 uid 的 persona（满足 `memory_l1.persona_uid` 外键）。
    async fn ensure_persona(storage: &dyn StorageBackend, persona_uid: &str) {
        let persona = Persona::new(
            persona_uid.to_string(),
            "测试人格".to_string(),
            PersonaKind::User,
            1,
            "local".to_string(),
        );
        storage
            .create_persona(&persona)
            .await
            .expect("persona 创建应成功");
    }

    /// 写入一条指定 persona 的 L1（先建 persona 与 session 满足外键；created_at 为当前时间）。
    async fn save_l1(storage: &dyn StorageBackend, persona_uid: &str, summary: &str) {
        ensure_persona(storage, persona_uid).await;
        let session = storage
            .create_session(None)
            .await
            .expect("session 创建应成功");
        let mut l1 = MemoryL1::new(session.id, summary.to_string(), Some("下午".to_string()));
        l1.persona_uid = Some(persona_uid.to_string());
        storage.save_memory_l1(&l1).await.expect("L1 写入应成功");
    }

    // =========================================================
    // 脉络素材加载
    // =========================================================

    /// 闸门关闭（`injection.narrative=false`）→ 跳过 L1 加载，返回空素材。
    #[tokio::test]
    async fn gate_off_returns_empty_material() {
        let storage = test_storage("gate").await;
        // 预置 L1：若闸门不生效将被加载
        save_l1(storage.as_ref(), "rama-0001", "不应出现的摘要").await;

        let cfg = RamariaConfig::default();
        let retriever = RwLock::new(Retriever::new());
        let material = load_narrative_material(
            storage.as_ref(),
            &retriever,
            &cfg.retrieval,
            &cfg.decay,
            false,
            "rama-0001",
            "你好",
        )
        .await;

        assert!(
            material.recent_summaries.is_empty(),
            "闸门关闭应跳过 L1 摘要加载"
        );
        assert!(material.last_active_at.is_none(), "last_active_at 应为空");
    }

    /// 非加权路径：无条件取最近 N 条，最后活跃时间按 UTC `%Y-%m-%d %H:%M` 口径。
    #[tokio::test]
    async fn unweighted_path_reads_recent_l1_and_last_active() {
        let storage = test_storage("recent").await;
        ensure_persona(storage.as_ref(), "rama-0001").await;
        let session = storage
            .create_session(None)
            .await
            .expect("session 创建应成功");
        // created_at 固定：2023-11-14 22:13:20 UTC
        let mut l1 = MemoryL1::new(
            session.id,
            "最近的一次对话摘要".to_string(),
            Some("下午".to_string()),
        );
        l1.atmosphere = Some("轻松".to_string());
        l1.persona_uid = Some("rama-0001".to_string());
        l1.created_at = 1_700_000_000_000;
        storage.save_memory_l1(&l1).await.expect("L1 写入应成功");

        let mut cfg = RamariaConfig::default();
        cfg.retrieval.narrative_weighted = false; // 回退"无条件取最近 N 条"
        let retriever = RwLock::new(Retriever::new());
        let material = load_narrative_material(
            storage.as_ref(),
            &retriever,
            &cfg.retrieval,
            &cfg.decay,
            true,
            "rama-0001",
            "完全不相关的话题",
        )
        .await;

        assert_eq!(material.recent_summaries.len(), 1);
        assert!(material.recent_summaries[0].contains("最近的一次对话摘要"));
        assert!(material.recent_summaries[0].contains("下午"));
        assert!(material.recent_summaries[0].contains("轻松"));
        assert_eq!(
            material.last_active_at.as_deref(),
            Some("2023-11-14 22:13"),
            "最后活跃时间应按 UTC %Y-%m-%d %H:%M 格式化"
        );
    }

    /// 加权路径（默认）：以当前消息为话题依据，话题相关的 L1 优先注入。
    #[tokio::test]
    async fn weighted_path_ranks_topic_relevant_first() {
        let storage = test_storage("weighted").await;
        let cfg = RamariaConfig::default(); // narrative_weighted = true
        let now = ramaria_core::types::now_ms();

        let mut retriever = Retriever::new();
        retriever.index_l1(&L1DocView {
            id: Uuid::new_v4(),
            summary: "用户讨论了Rust异步编程".to_string(),
            keywords: Some("Rust,编程".to_string()),
            persona_uid: Some("rama-0001".to_string()),
            created_at: now - 3 * 86_400_000,
            salience: 0.5,
            last_accessed_at: None,
        });
        retriever.index_l1(&L1DocView {
            id: Uuid::new_v4(),
            summary: "用户和朋友去吃了火锅".to_string(),
            keywords: Some("社交,火锅".to_string()),
            persona_uid: Some("rama-0001".to_string()),
            created_at: now - 86_400_000,
            salience: 0.5,
            last_accessed_at: None,
        });
        let retriever = RwLock::new(retriever);

        let material = load_narrative_material(
            storage.as_ref(),
            &retriever,
            &cfg.retrieval,
            &cfg.decay,
            true,
            "rama-0001",
            "Rust 编程",
        )
        .await;

        assert!(
            !material.recent_summaries.is_empty(),
            "加权注入应有脉络结果"
        );
        assert!(
            material.recent_summaries[0].contains("Rust"),
            "话题相关的 L1 应优先注入，got: {:?}",
            material.recent_summaries[0]
        );
    }

    /// 加权检索无结果（空索引）→ 回退最近 N 条（不丢脉络）。
    #[tokio::test]
    async fn weighted_no_hit_falls_back_to_recent_l1() {
        let storage = test_storage("weighted-fallback").await;
        save_l1(storage.as_ref(), "rama-0001", "回退取到的最近摘要").await;

        let cfg = RamariaConfig::default(); // narrative_weighted = true
        let retriever = RwLock::new(Retriever::new()); // 空索引 → 检索无结果
        let material = load_narrative_material(
            storage.as_ref(),
            &retriever,
            &cfg.retrieval,
            &cfg.decay,
            true,
            "rama-0001",
            "任意输入",
        )
        .await;

        assert_eq!(material.recent_summaries.len(), 1);
        assert!(material.recent_summaries[0].contains("回退取到的最近摘要"));
    }

    /// 服务层懒加载槽（`RwLock<Option<Retriever>>`）未加载 → 空检索 → 回退最近 N 条。
    #[tokio::test]
    async fn unloaded_slot_falls_back_to_recent_l1() {
        let storage = test_storage("slot").await;
        save_l1(storage.as_ref(), "rama-0001", "懒加载槽未加载时回退摘要").await;

        let cfg = RamariaConfig::default();
        let slot: RwLock<Option<Retriever>> = RwLock::new(None);
        let material = load_narrative_material(
            storage.as_ref(),
            &slot,
            &cfg.retrieval,
            &cfg.decay,
            true,
            "rama-0001",
            "任意输入",
        )
        .await;

        assert_eq!(material.recent_summaries.len(), 1);
        assert!(material.recent_summaries[0].contains("懒加载槽未加载时回退摘要"));
    }

    // =========================================================
    // 脉络行格式化与结果转换
    // =========================================================

    #[test]
    fn format_line_with_time_and_atmosphere() {
        let mut l1 = MemoryL1::new(
            Uuid::new_v4(),
            "讨论了编程".to_string(),
            Some("下午".to_string()),
        );
        l1.atmosphere = Some("轻松".to_string());
        assert_eq!(
            format_l1_as_context_line(&l1),
            "下午 — 讨论了编程。氛围轻松。"
        );
    }

    #[test]
    fn format_line_with_time_only() {
        let l1 = MemoryL1::new(
            Uuid::new_v4(),
            "讨论了编程".to_string(),
            Some("上午".to_string()),
        );
        assert_eq!(format_l1_as_context_line(&l1), "上午 — 讨论了编程");
    }

    #[test]
    fn format_line_with_atmosphere_only() {
        let mut l1 = MemoryL1::new(Uuid::new_v4(), "讨论了编程".to_string(), None);
        l1.atmosphere = Some("融洽".to_string());
        assert_eq!(format_l1_as_context_line(&l1), "讨论了编程。氛围融洽。");
    }

    #[test]
    fn format_line_without_annotations() {
        let l1 = MemoryL1::new(Uuid::new_v4(), "讨论了编程".to_string(), None);
        assert_eq!(format_l1_as_context_line(&l1), "讨论了编程");
    }

    /// 单条摘要截断到 120 字符（预算内含省略号）。
    #[test]
    fn format_line_truncates_to_120_chars() {
        let long_summary = "这是一段非常长的摘要".repeat(20);
        let l1 = MemoryL1::new(Uuid::new_v4(), long_summary, None);
        let formatted = format_l1_as_context_line(&l1);
        assert_eq!(
            formatted.chars().count(),
            120,
            "截断结果应为预算内 120 字符"
        );
        assert!(formatted.ends_with('…'));
    }

    /// 加权结果转换：仅 L1 层可转（L2/图谱不是脉络注入目标），标注字段置空。
    #[test]
    fn search_result_conversion_rules() {
        let l1_id = Uuid::new_v4();
        let l1_sr = SearchResult {
            doc_id: DocId::L1(l1_id),
            layer: "l1".to_string(),
            rrf_score: 0.8,
            bm25_score: Some(0.8),
            vector_score: None,
            graph_score: None,
            persona_uid: Some("rama-0001".to_string()),
            share: None,
            created_at: 1_700_000_000_000,
            last_accessed_at: None,
            doc_summary: "用户讨论了Rust".to_string(),
        };
        let l1 = search_result_to_memory_l1(&l1_sr).expect("L1 结果应转换成功");
        assert_eq!(l1.id, l1_id);
        assert_eq!(l1.summary, "用户讨论了Rust");
        assert_eq!(l1.created_at, 1_700_000_000_000);
        assert_eq!(l1.session_id, Uuid::nil(), "脉络行不参与会话归属");
        assert!(l1.time_period.is_none());
        assert!(l1.atmosphere.is_none());

        let l2_sr = SearchResult {
            doc_id: DocId::L2(42),
            layer: "l2".to_string(),
            rrf_score: 1.0,
            bm25_score: None,
            vector_score: None,
            graph_score: None,
            persona_uid: Some("rama-0001".to_string()),
            share: None,
            created_at: 1_000,
            last_accessed_at: None,
            doc_summary: "事件摘要".to_string(),
        };
        assert!(
            search_result_to_memory_l1(&l2_sr).is_none(),
            "非 L1 层不应进入脉络注入"
        );
    }
}
