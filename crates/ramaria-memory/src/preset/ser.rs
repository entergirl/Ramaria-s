//! crates/ramaria-memory/src/preset/ser.rs - Ramaria 预设序列化模块
//!
//! 设计特点:
//! - 统一中间表示到 persona.toml 超集文本的序列化（注册段 / 对话身份 / 自由文本块 / L3 结构化段 / 提示词补丁）
//! - ST 卡 personality 降级语义：原文写入 [blocks].A_persona（blocks 已有同名键时不覆盖）
//! - 空段整体省略（无条目不输出对应节）；行为规则参数总是写全四键
//! - 序列化结果可被解析入口再次消费（序列化 → 解析语义等价）

use ramaria_core::ProfileField;
use ramaria_core::behavior::BehaviorParams;
use ramaria_core::{RamariaError, RamariaResult};
use toml::{Table, Value};

use super::model::{ExampleEntry, FactEntry, ParsedPreset, RuleEntry, TraitEntry, TraitsSection};

/// 将解析产物序列化为超集 TOML 文本（供 personas.config 保存与导出）。
///
/// 说明:
/// - 输出为可再次解析的合法超集 TOML：序列化 → 解析（round-trip）得到语义等价的
///   解析产物。ST 来源的 `personality` 原文在序列化时转化为 `[blocks].A_persona`，
///   再次解析后 `personality_raw` 为 None、原文保留于 blocks。
/// - `[blocks]` 已含 `A_persona` 键时以 blocks 为准，不追加注入。
/// - 空段（无条目）不输出对应节；`[[behavior_rules]].params` 总是写全四个参数键。
///
/// 参数:
/// - `parsed`: 解析产物（中间表示）。
///
/// 返回:
/// - 成功时返回超集 TOML 文本；序列化失败返回 `RamariaError::config`。
pub fn to_superset_toml(parsed: &ParsedPreset) -> RamariaResult<String> {
    let mut root = Table::new();
    root.insert("persona".to_string(), Value::Table(persona_table(parsed)));
    root.insert("identity".to_string(), Value::Table(identity_table(parsed)));

    let blocks = blocks_table(parsed);
    if !blocks.is_empty() {
        root.insert("blocks".to_string(), Value::Table(blocks));
    }
    let traits = traits_table(&parsed.traits);
    if !traits.is_empty() {
        root.insert("traits".to_string(), Value::Table(traits));
    }
    if !parsed.facts.is_empty() {
        let items = parsed.facts.iter().map(fact_value).collect();
        root.insert("facts".to_string(), Value::Array(items));
    }
    if !parsed.examples.is_empty() {
        let items = parsed.examples.iter().map(example_value).collect();
        root.insert("examples".to_string(), Value::Array(items));
    }
    if !parsed.rules.is_empty() {
        let items = parsed.rules.iter().map(rule_value).collect();
        root.insert("behavior_rules".to_string(), Value::Array(items));
    }
    let prompt = prompt_table(parsed);
    if !prompt.is_empty() {
        root.insert("prompt".to_string(), Value::Table(prompt));
    }

    toml::to_string_pretty(&root)
        .map_err(|e| RamariaError::config(format!("序列化预设为超集 TOML 失败: {e}")))
}

// =========================================================
// 各节构建
// =========================================================

/// 构建 [persona] 注册段：uid / kind / avatar / description 为有值才写，name / source 总写。
fn persona_table(parsed: &ParsedPreset) -> Table {
    let mut table = Table::new();
    insert_opt_str(&mut table, "uid", parsed.persona.uid.as_deref());
    if let Some(kind) = parsed.persona.kind {
        table.insert("kind".to_string(), Value::String(kind.as_str().to_string()));
    }
    table.insert(
        "name".to_string(),
        Value::String(parsed.persona.name.clone()),
    );
    insert_opt_str(&mut table, "avatar", parsed.persona.avatar.as_deref());
    insert_opt_str(
        &mut table,
        "description",
        parsed.persona.description.as_deref(),
    );
    table.insert(
        "source".to_string(),
        Value::String(parsed.persona.source.clone()),
    );
    table
}

/// 构建 [identity] 对话身份段：assistant_name 总写，其余有值才写。
fn identity_table(parsed: &ParsedPreset) -> Table {
    let mut table = Table::new();
    table.insert(
        "assistant_name".to_string(),
        Value::String(parsed.identity.assistant_name.clone()),
    );
    insert_opt_str(
        &mut table,
        "user_name",
        parsed.identity.user_name.as_deref(),
    );
    insert_opt_str(&mut table, "work", parsed.identity.work.as_deref());
    insert_opt_str(&mut table, "era", parsed.identity.era.as_deref());
    table
}

/// 构建 [blocks] 自由文本块：按中间表示顺序写入；personality 原文降级为 A_persona 追加。
fn blocks_table(parsed: &ParsedPreset) -> Table {
    let mut table = Table::new();
    for (key, value) in &parsed.blocks {
        table.insert(key.clone(), Value::String(value.clone()));
    }
    let has_persona_block = parsed.blocks.iter().any(|(key, _)| key == "A_persona");
    if !has_persona_block {
        if let Some(raw) = parsed
            .personality_raw
            .as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
        {
            table.insert("A_persona".to_string(), Value::String(raw.to_string()));
        }
    }
    table
}

/// 构建 [traits] 三层：空层不写、三层全空返回空表（由调用方省略该节）。
fn traits_table(traits: &TraitsSection) -> Table {
    let mut table = Table::new();
    for (key, items) in [
        ("base", &traits.base),
        ("primary", &traits.primary),
        ("accent", &traits.accent),
    ] {
        if items.is_empty() {
            continue;
        }
        let array = items.iter().map(trait_value).collect();
        table.insert(key.to_string(), Value::Array(array));
    }
    table
}

/// 单条 trait：label / meaning 总写，其余可选键有值才写，motives 非空才写。
fn trait_value(entry: &TraitEntry) -> Value {
    let mut table = Table::new();
    table.insert("label".to_string(), Value::String(entry.label.clone()));
    table.insert("meaning".to_string(), Value::String(entry.meaning.clone()));
    insert_opt_str(&mut table, "not_meaning", entry.not_meaning.as_deref());
    insert_opt_str(&mut table, "trigger", entry.trigger.as_deref());
    insert_opt_str(&mut table, "suppress", entry.suppress.as_deref());
    insert_opt_str(&mut table, "related", entry.related.as_deref());
    if !entry.motives.is_empty() {
        let array = entry
            .motives
            .iter()
            .map(|m| Value::String(m.clone()))
            .collect();
        table.insert("motives".to_string(), Value::Array(array));
    }
    Value::Table(table)
}

/// 单条 fact：field 写规范值形式，content 原文。
fn fact_value(entry: &FactEntry) -> Value {
    let mut table = Table::new();
    table.insert(
        "field".to_string(),
        Value::String(profile_field_label(entry.field)),
    );
    table.insert("content".to_string(), Value::String(entry.content.clone()));
    Value::Table(table)
}

/// 单条 example：user / assistant 均总写。
fn example_value(entry: &ExampleEntry) -> Value {
    let mut table = Table::new();
    table.insert("user".to_string(), Value::String(entry.user.clone()));
    table.insert(
        "assistant".to_string(),
        Value::String(entry.assistant.clone()),
    );
    Value::Table(table)
}

/// 单条 behavior_rule：keywords 总写、reaction 有值才写、avoid 非空才写、params 总写全四键。
fn rule_value(entry: &RuleEntry) -> Value {
    let mut table = Table::new();
    let keywords = entry
        .keywords
        .iter()
        .map(|k| Value::String(k.clone()))
        .collect();
    table.insert("keywords".to_string(), Value::Array(keywords));
    insert_opt_str(&mut table, "reaction", entry.reaction.as_deref());
    if !entry.avoid.is_empty() {
        let avoid = entry
            .avoid
            .iter()
            .map(|a| Value::String(a.clone()))
            .collect();
        table.insert("avoid".to_string(), Value::Array(avoid));
    }
    table.insert("params".to_string(), params_value(&entry.params));
    Value::Table(table)
}

/// 行为参数：四个参数键总是全部写出（值取中间表示已合并结果）。
fn params_value(params: &BehaviorParams) -> Value {
    let mut table = Table::new();
    table.insert(
        "emotion_intensity".to_string(),
        Value::Float(params.emotional_intensity),
    );
    table.insert(
        "proactiveness".to_string(),
        Value::Float(params.proactiveness),
    );
    table.insert(
        "detail_level".to_string(),
        Value::Float(params.detail_level),
    );
    table.insert("formality".to_string(), Value::Float(params.formality));
    Value::Table(table)
}

/// 构建 [prompt] 补丁：两个后缀均有值才写对应键；全空返回空表。
fn prompt_table(parsed: &ParsedPreset) -> Table {
    let mut table = Table::new();
    insert_opt_str(
        &mut table,
        "role_suffix",
        parsed.prompt.role_suffix.as_deref(),
    );
    insert_opt_str(
        &mut table,
        "style_suffix",
        parsed.prompt.style_suffix.as_deref(),
    );
    table
}

// =========================================================
// 取值辅助
// =========================================================

/// 可选字符串有值才写。
fn insert_opt_str(table: &mut Table, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        table.insert(key.to_string(), Value::String(value.to_string()));
    }
}

/// ProfileField 规范值形式（PascalCase，对齐预设格式规范取值表）。
fn profile_field_label(field: ProfileField) -> String {
    match field {
        ProfileField::BasicInfo => "BasicInfo".to_string(),
        ProfileField::PersonalStatus => "PersonalStatus".to_string(),
        ProfileField::Interests => "Interests".to_string(),
        ProfileField::Social => "Social".to_string(),
        ProfileField::History => "History".to_string(),
        ProfileField::RecentContext => "RecentContext".to_string(),
        ProfileField::SpeakingStyle => "SpeakingStyle".to_string(),
        // 非穷尽枚举的通配分支：未知变体回退 Debug 名（与 PascalCase 口径一致）
        _ => format!("{field:?}"),
    }
}
