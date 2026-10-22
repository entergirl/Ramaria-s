//! crates/ramaria-memory/src/init/persona.rs - 助手冷启动与画像落库
//!
//! 设计特点:
//! - 已有活跃 rama persona 时跳过创建，只统计画像规模
//! - 不存在时创建 rama-0001，LLM 拆解失败降级为仅基础 persona
//! - facts / traits 逐条写入，单条失败记 warn 不阻断

use super::*;

// =========================================================
// 冷启动核心逻辑
// =========================================================

/// 执行助手冷启动（或加载已有画像）。
///
/// 流程:
/// 1. 查询 personas 表是否存在 kind='rama' 的活跃记录
/// 2. 不存在 → 创建 rama-0001 → 调用 LLM 拆解 persona.toml → 写入 facts + traits
/// 3. 已存在 → 返回已有 persona 信息
///
/// 参数:
/// - `storage`: 存储后端（用于查询/写入 personas/facts/traits）。
/// - `llm`: LLM provider（用于结构化拆解）。
/// - `persona_toml_content`: persona.toml 文件原始内容。
/// - `config`: 冷启动配置。
///
/// 返回:
/// - `ColdStartResult`: 冷启动结果，包含 persona_uid、是否新建、facts 和 traits 数量。
///
/// 冷启动入口（README 核心特性）；v1.6 确认接线路径
pub async fn initialize_rama_persona(
    storage: &dyn StorageBackend,
    llm: &dyn LlmProviderTrait,
    persona_toml_content: &str,
    config: &ColdStartConfig,
) -> RamariaResult<ColdStartResult> {
    info!("开始 Ramaria 助手冷启动检查");

    // ---- 步骤 1: 查询是否存在 rama persona ----
    let existing_personas = storage.list_personas().await?;
    let existing_rama = existing_personas
        .iter()
        .find(|p| p.kind == PersonaKind::Rama && p.active);

    if let Some(persona) = existing_rama {
        info!(
            persona_uid = %persona.uid,
            "已存在活跃的 rama persona，跳过冷启动"
        );

        // 统计已有 facts 和 traits 数量
        let fact_count = count_facts_for_persona(storage, &persona.uid).await?;
        let trait_count = storage.list_traits_by_persona(&persona.uid).await?.len();

        return Ok(ColdStartResult {
            persona_uid: persona.uid.clone(),
            is_new: false,
            fact_count,
            trait_count,
        });
    }

    // ---- 步骤 2: 解析 persona.toml ----
    let parsed = parse_persona_toml(persona_toml_content)?;
    info!(
        assistant_name = %parsed.assistant_name,
        user_name = %parsed.user_name,
        block_count = parsed.blocks.len(),
        "persona.toml 解析成功"
    );

    // ---- 步骤 3: 创建 rama-0001 ----
    let rama_persona = Persona::new(
        "rama-0001".to_string(),
        parsed.assistant_name.clone(),
        PersonaKind::Rama,
        RAMA_DISPLAY_ORDER,
        RAMA_SOURCE.to_string(),
    );
    let persona_id = storage.create_persona(&rama_persona).await?;
    info!(
        persona_id = persona_id,
        persona_uid = "rama-0001",
        "已创建 rama persona"
    );

    // ---- 步骤 4: 调用 LLM 拆解画像 ----
    let blocks_text = format_blocks_for_prompt(&parsed.blocks);
    let decomposed = match decompose_profile_text(llm, &blocks_text, config).await {
        Ok(decomposed) => decomposed,
        Err(e) => {
            warn!(error = %e, "LLM 画像拆解失败，创建基础 persona（无结构化画像）");
            return Ok(ColdStartResult {
                persona_uid: "rama-0001".to_string(),
                is_new: true,
                fact_count: 0,
                trait_count: 0,
            });
        }
    };

    // ---- 步骤 5: 写入 facts ----
    let mut fact_count = 0usize;
    for fact_entry in &decomposed.facts {
        let fact = PersonaFact::new(
            "rama-0001".to_string(),
            fact_entry.field,
            fact_entry.content.clone(),
            FactSource::Manual,
        );

        match storage.save_fact(&fact).await {
            Ok(_) => {
                fact_count += 1;
                debug!(field = %fact_entry.field, "已写入 fact");
            }
            Err(e) => {
                warn!(field = %fact_entry.field, error = %e, "写入 fact 失败，跳过");
            }
        }
    }

    // ---- 步骤 6: 写入 traits ----
    let mut trait_count = 0usize;
    for (idx, trait_entry) in decomposed.traits.iter().enumerate() {
        // 注意：PersonalityTrait::new 不接受 optional 字段，需要手动设置
        let mut personality_trait = PersonalityTrait::new(
            "rama-0001".to_string(),
            trait_entry.layer,
            trait_entry.label.clone(),
            trait_entry.meaning.clone(),
            TraitSource::Manual,
            (idx + 1) as i32,
        );
        personality_trait.not_meaning = trait_entry.not_meaning.clone();
        personality_trait.trigger = trait_entry.trigger.clone();
        personality_trait.suppress = trait_entry.suppress.clone();
        personality_trait.related = trait_entry.related.clone();
        personality_trait.status = TraitStatus::Active;

        match storage.save_trait(&personality_trait).await {
            Ok(_) => {
                trait_count += 1;
                debug!(
                    label = %trait_entry.label,
                    layer = %trait_entry.layer,
                    "已写入 trait"
                );
            }
            Err(e) => {
                warn!(
                    label = %trait_entry.label,
                    error = %e,
                    "写入 trait 失败，跳过"
                );
            }
        }
    }

    info!(
        persona_uid = "rama-0001",
        fact_count = fact_count,
        trait_count = trait_count,
        "Ramaria 助手冷启动完成"
    );

    Ok(ColdStartResult {
        persona_uid: "rama-0001".to_string(),
        is_new: true,
        fact_count,
        trait_count,
    })
}
