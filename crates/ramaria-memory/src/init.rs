//! rust/crates/ramaria-memory/src/init.rs - Ramaria 助手冷启动模块
//!
//! 设计特点:
//! - 首次启动检测: 查询 personas 表无 kind='rama' 记录时触发
//! - persona.toml 解析: 手动解析 [identity] 和 [blocks] 节（零外部依赖）
//! - LLM 结构化拆解: 将 persona 配置拆解为 PersonaFact + PersonalityTrait 写入存储
//! - 已有画像时跳过初始化，返回已有 persona 信息
//! - 错误处理: LLM 可恢复错误不阻断启动（降级为仅创建基础 persona）
//! - 所有 LLM 依赖通过 LlmProvider trait 注入，支持 mock 测试
//!
//! 冷启动流程:
//! 1. 查询 personas 表，检查是否已存在 rama persona
//! 2. 不存在 → 创建 rama-0001 → 读取 persona.toml → LLM 拆解 → 写入 facts + traits
//! 3. 已存在 → 加载已有画像 → persona.toml 作为参考而非事实源

use ramaria_core::traits::ChatRequest;
use ramaria_core::types::{FactSource, PersonaKind};
use ramaria_core::{
    LlmProviderTrait, Persona, PersonaFact, PersonalityTrait, ProfileField, RamariaError,
    RamariaResult, StorageBackend, TraitLayer, TraitSource, TraitStatus,
};
use serde::Deserialize;
use tracing::{debug, info, warn};
use uuid::Uuid;

// =========================================================
// persona.toml 解析结构
// =========================================================

/// 解析后的 persona.toml 内容。
///
/// 字段:
/// - `assistant_name`: [identity] 节 assistant_name
/// - `user_name`: [identity] 节 user_name
/// - `blocks`: [blocks] 节所有键值对
#[derive(Debug, Clone)]
pub struct PersonaToml {
    pub assistant_name: String,
    pub user_name: String,
    pub blocks: Vec<(String, String)>,
}

/// 手动解析 persona.toml。
///
/// 支持的格式（简化版 TOML 解析器）:
/// - `[identity]` 节: key = "value"
/// - `[blocks]` 节: key = """...""" 或 key = "..."
/// - 忽略空行和注释行（以 # 开头）
///
/// 参数:
/// - `content`: persona.toml 文件原始内容。
///
/// 返回:
/// - 成功时返回 PersonaToml，解析失败返回错误。
pub fn parse_persona_toml(content: &str) -> RamariaResult<PersonaToml> {
    let mut assistant_name = String::new();
    let mut user_name = String::new();
    let mut blocks: Vec<(String, String)> = Vec::new();

    let mut current_section: Option<String> = None;
    let mut in_multiline = false;
    let mut multiline_key = String::new();
    let mut multiline_value = String::new();

    for line in content.lines() {
        let trimmed = line.trim();

        // 跳过空行和注释
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }

        // 正在收集多行字符串
        if in_multiline {
            if trimmed == r#"""""# {
                // 多行字符串结束
                blocks.push((
                    multiline_key.clone(),
                    multiline_value.trim_end().to_string(),
                ));
                in_multiline = false;
                multiline_key.clear();
                multiline_value.clear();
            } else {
                if !multiline_value.is_empty() {
                    multiline_value.push('\n');
                }
                multiline_value.push_str(line);
            }
            continue;
        }

        // 节标题 [section]
        if trimmed.starts_with('[') && trimmed.ends_with(']') {
            current_section = Some(trimmed[1..trimmed.len() - 1].to_string());
            continue;
        }

        // 键值对 key = value
        if let Some(eq_pos) = trimmed.find('=') {
            let key = trimmed[..eq_pos].trim().to_string();
            let value_raw = trimmed[eq_pos + 1..].trim();

            // 检测多行字符串开始 """
            if value_raw == r#"""""# {
                in_multiline = true;
                multiline_key = key;
                multiline_value = String::new();
                continue;
            }

            // 普通字符串值（去掉首尾引号）
            let value = if value_raw.starts_with('"') && value_raw.ends_with('"') {
                value_raw[1..value_raw.len() - 1].to_string()
            } else {
                value_raw.to_string()
            };

            match current_section.as_deref() {
                Some("identity") => match key.as_str() {
                    "assistant_name" => assistant_name = value,
                    "user_name" => user_name = value,
                    _ => debug!(key = %key, "忽略 [identity] 中未知字段"),
                },
                Some("blocks") => {
                    blocks.push((key, value));
                }
                _ => debug!(section = ?current_section, "忽略未知节"),
            }
        }
    }

    // 检查是否有未闭合的多行字符串
    if in_multiline {
        return Err(RamariaError::config(
            "persona.toml 中存在未闭合的多行字符串 (\"\"\")",
        ));
    }

    if assistant_name.is_empty() {
        return Err(RamariaError::config(
            "persona.toml 缺少 [identity] assistant_name",
        ));
    }

    if blocks.is_empty() {
        warn!("persona.toml 的 [blocks] 节为空，将创建无结构化画像的 rama persona");
    }

    Ok(PersonaToml {
        assistant_name,
        user_name,
        blocks,
    })
}

// =========================================================
// 冷启动配置
// =========================================================

/// 冷启动配置。
#[derive(Debug, Clone)]
pub struct ColdStartConfig {
    /// LLM 生成温度
    pub temperature: f64,
    /// LLM 最大输出 tokens
    pub max_tokens: u32,
}

impl Default for ColdStartConfig {
    fn default() -> Self {
        Self {
            temperature: 0.3,
            max_tokens: 4096,
        }
    }
}

// =========================================================
// 冷启动结果
// =========================================================

/// 冷启动完成后的结果。
#[derive(Debug, Clone)]
pub struct ColdStartResult {
    /// 创建的（或已有）的 rama persona uid
    pub persona_uid: String,
    /// 是否为新创建（false 表示已有画像，跳过初始化）
    pub is_new: bool,
    /// 提取的 fact 数量
    pub fact_count: usize,
    /// 提取的 trait 数量
    pub trait_count: usize,
}

// =========================================================
// LLM Prompt 模板
// =========================================================

/// 冷启动 Prompt：将 persona.toml 配置拆解为结构化画像。
///
/// 要求 LLM 输出严格 JSON，包含:
/// - `facts`: 人物事实数组（field + content + source）
/// - `traits`: 性格标签数组（layer + trait_label + meaning + not_meaning + trigger + suppress + related）
const COLD_START_PROMPT: &str = r#"你是一个角色画像结构化分析助手。请分析下面给出的角色设定配置，提取结构化的人物事实和性格标签。

【角色配置】
{persona_config}

【任务要求】
1. 提取 10-20 条结构化人物事实（facts），每条包含：
   - field: 必须从以下选项中选一个 —— "BasicInfo"（基础信息：姓名/年龄/性别/生日/身份）、"PersonalStatus"（近期状态/性格描述）、"Interests"（兴趣爱好/偏好/习惯）、"Social"（社交关系/与他人的关系）、"History"（背景/历史事件/经历）、"RecentContext"（近期背景/当前处境）、"SpeakingStyle"（说话风格/表达习惯/回复规则）
   - content: 用第三人称陈述句描述该事实，如"她的名字是黎杋枫"、"她性格知性稳重"
   - source: 始终填 "Manual"

2. 提取 5-15 条性格标签（traits），每条包含：
   - layer: 从以下选项中选一个 —— "Base"（底色，跨场景一致的基础性格，如"知性稳重"）、"Primary"（主色调，经常表现但非绝对的次要特质，如"冷幽默"）、"Accent"（点缀，特定场景下出现的条件性特质，如"引导探索不直接给答案"）
   - trait_label: 简短中文性格标签，如"知性"、"幽默感"、"回避型回应"
   - meaning: 这个标签的具体含义，1-2句话，第三人称
   - not_meaning: 不是什么意思（澄清边界），1-2句话，可选填null
   - trigger: 触发此特质的典型场景，可选填null
   - suppress: 抑制此特质的典型场景，可选填null
   - related: 与此特质相关的其他标签名（逗号分隔），可选填null

【输出格式要求】
严格按照以下 JSON 格式输出，不要输出任何其他内容（不要加 markdown 代码块，不要加说明文字）：

{
  "facts": [
    {
      "field": "BasicInfo",
      "content": "她的名字是黎杋枫",
      "source": "Manual"
    }
  ],
  "traits": [
    {
      "layer": "Base",
      "trait_label": "知性稳重",
      "meaning": "她以理性、克制的态度与人交流，不情绪化，保持一致的成熟气质",
      "not_meaning": "不是冷漠或疏离，她在稳重中仍有关心",
      "trigger": "日常对话、知识讲解、情绪安抚",
      "suppress": "极端紧急情况可能需要更直接表达",
      "related": "理性,情绪稳定"
    }
  ]
}

【注意事项】
- 从角色配置中提取所有明确陈述和隐含信息
- 如果配置中有具体的说话风格要求（如用||断句、不反问等），归入 SpeakingStyle
- 性格标签避免过度抽象（如"有思想"），应具体可操作（如"善于比喻讲解"）
- 输出必须为合法 JSON，字符串中使用双引号，不能使用单引号
- content 和 meaning 中避免直接复制配置原文，要用自己的话重新组织"#;

// =========================================================
// LLM 响应 JSON 结构
// =========================================================

/// LLM 返回的单条 fact。
#[derive(Debug, Deserialize)]
struct ColdStartFactJson {
    field: String,
    content: String,
    #[serde(default)]
    source: Option<String>,
}

/// LLM 返回的单条 trait。
#[derive(Debug, Deserialize)]
struct ColdStartTraitJson {
    layer: String,
    trait_label: String,
    meaning: String,
    not_meaning: Option<String>,
    trigger: Option<String>,
    suppress: Option<String>,
    related: Option<String>,
}

/// LLM 返回的顶层结构。
#[derive(Debug, Deserialize)]
struct ColdStartResponse {
    facts: Vec<ColdStartFactJson>,
    traits: Vec<ColdStartTraitJson>,
}

// =========================================================
// 冷启动常量
// =========================================================

/// rama persona 的显示顺序（排在用户之后）。
const RAMA_DISPLAY_ORDER: i64 = 1;

/// rama persona 的数据来源（手动配置）。
const RAMA_SOURCE: &str = "manual";

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
    let prompt = COLD_START_PROMPT.replace("{persona_config}", &blocks_text);

    let request = ChatRequest {
        system_prompt: "你是一个精确的角色画像分析助手。请严格按照要求的 JSON 格式输出。"
            .to_string(),
        memory_context: None,
        history: vec![],
        user_message: prompt,
        temperature: config.temperature,
        max_tokens: config.max_tokens,
        request_id: Uuid::new_v4(),
    };

    let raw_response = match llm.chat(&request).await {
        Ok(text) => text,
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

    // ---- 步骤 5: 解析 LLM 响应 ----
    let json_text = crate::utils::strip_thinking(&raw_response);
    let json_text = crate::utils::extract_first_json_object(&json_text);

    let response: ColdStartResponse = match json_text {
        Some(j) => match serde_json::from_str::<ColdStartResponse>(&j) {
            Ok(r) => r,
            Err(e) => {
                warn!(error = %e, json = %j, "LLM 返回的 JSON 解析失败，创建基础 persona");
                return Ok(ColdStartResult {
                    persona_uid: "rama-0001".to_string(),
                    is_new: true,
                    fact_count: 0,
                    trait_count: 0,
                });
            }
        },
        None => {
            warn!("LLM 返回内容中未找到 JSON 对象，创建基础 persona");
            return Ok(ColdStartResult {
                persona_uid: "rama-0001".to_string(),
                is_new: true,
                fact_count: 0,
                trait_count: 0,
            });
        }
    };

    // ---- 步骤 6: 写入 facts ----
    let mut fact_count = 0usize;
    for fact_json in &response.facts {
        let field = parse_profile_field(&fact_json.field);
        let source = match fact_json.source.as_deref() {
            Some("L1") => FactSource::L1,
            Some("Event") => FactSource::Event,
            Some("Manual") => FactSource::Manual,
            _ => FactSource::Manual,
        };

        let fact = PersonaFact::new(
            "rama-0001".to_string(),
            field,
            fact_json.content.clone(),
            source,
        );

        match storage.save_fact(&fact).await {
            Ok(_) => {
                fact_count += 1;
                debug!(field = %fact_json.field, "已写入 fact");
            }
            Err(e) => {
                warn!(field = %fact_json.field, error = %e, "写入 fact 失败，跳过");
            }
        }
    }

    // ---- 步骤 7: 写入 traits ----
    let mut trait_count = 0usize;
    for (idx, trait_json) in response.traits.iter().enumerate() {
        let layer = parse_trait_layer(&trait_json.layer);

        let personality_trait = PersonalityTrait::new(
            "rama-0001".to_string(),
            layer,
            trait_json.trait_label.clone(),
            trait_json.meaning.clone(),
            TraitSource::Manual,
            (idx + 1) as i32,
        );

        // 注意：PersonalityTrait::new 不接受 optional 字段，需要手动设置
        let mut personality_trait = personality_trait;
        personality_trait.not_meaning = trait_json.not_meaning.clone();
        personality_trait.trigger = trait_json.trigger.clone();
        personality_trait.suppress = trait_json.suppress.clone();
        personality_trait.related = trait_json.related.clone();
        personality_trait.status = TraitStatus::Active;

        match storage.save_trait(&personality_trait).await {
            Ok(_) => {
                trait_count += 1;
                debug!(
                    label = %trait_json.trait_label,
                    layer = %trait_json.layer,
                    "已写入 trait"
                );
            }
            Err(e) => {
                warn!(
                    label = %trait_json.trait_label,
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

// =========================================================
// 辅助函数
// =========================================================

/// 将 blocks 列表格式化为 LLM prompt 中的人类可读文本。
fn format_blocks_for_prompt(blocks: &[(String, String)]) -> String {
    let mut out = String::new();
    for (key, value) in blocks {
        out.push_str(&format!("## {}\n\n{}\n\n", key, value));
    }
    out
}

/// 解析 ProfileField 字符串。
///
/// 支持中英文变体，容错处理。映射到 ramaria-core 的实际枚举变体。
fn parse_profile_field(field: &str) -> ProfileField {
    match field {
        "BasicInfo" | "身份信息" | "身份" | "Identity" => ProfileField::BasicInfo,
        "PersonalStatus" | "性格描述" | "性格" | "Personality" | "近期状态" => {
            ProfileField::PersonalStatus
        }
        "Interests" | "兴趣爱好" | "偏好" | "习惯" | "Preference" | "偏好/习惯" => {
            ProfileField::Interests
        }
        "Social" | "社交关系" | "关系" | "Relationship" | "社交情况" | "与他人的关系" => {
            ProfileField::Social
        }
        "History" | "历史事件" | "背景" | "经历" | "Background" | "背景/经历" => {
            ProfileField::History
        }
        "RecentContext" | "近期背景" | "当前处境" => ProfileField::RecentContext,
        "SpeakingStyle" | "说话风格" | "表达习惯" | "风格" | "说话风格/表达习惯" => {
            ProfileField::SpeakingStyle
        }
        other => {
            warn!(field = %other, "未知 ProfileField，回退为 BasicInfo");
            ProfileField::BasicInfo
        }
    }
}

/// 解析 TraitLayer 字符串。
///
/// 支持中英文变体，容错处理。
fn parse_trait_layer(layer: &str) -> TraitLayer {
    match layer {
        "Base" | "底色" | "基础" => TraitLayer::Base,
        "Primary" | "主色调" | "主要" => TraitLayer::Primary,
        "Accent" | "点缀" | "条件" => TraitLayer::Accent,
        other => {
            warn!(layer = %other, "未知 TraitLayer，回退为 Accent");
            TraitLayer::Accent
        }
    }
}

/// 统计某个 persona 的 facts 数量。
///
/// TODO(P3): 当前对 7 个 ProfileField 各发一次 DB 查询（N+1），
/// 应在 StorageBackend 中添加 `count_all_facts_for_persona` 方法，
/// 改为一条 `GROUP BY field` SQL 完成统计。
async fn count_facts_for_persona(
    storage: &dyn StorageBackend,
    persona_uid: &str,
) -> RamariaResult<usize> {
    // 遍历所有 ProfileField 变体，累加 count
    let fields = [
        ProfileField::BasicInfo,
        ProfileField::PersonalStatus,
        ProfileField::Interests,
        ProfileField::Social,
        ProfileField::History,
        ProfileField::RecentContext,
        ProfileField::SpeakingStyle,
    ];

    let mut total = 0usize;
    for field in &fields {
        let facts = storage.list_facts_by_persona(persona_uid, *field).await?;
        total += facts.len();
    }
    Ok(total)
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // =========================================================
    // persona.toml 解析测试
    // =========================================================

    #[test]
    fn test_parse_basic_persona_toml() {
        let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = "烧酒"

[blocks]
A_persona = "你是黎杋枫。性格知性稳重。"
E_rules = "用||分隔回复。"
"#;

        let parsed = parse_persona_toml(content).expect("解析失败");
        assert_eq!(parsed.assistant_name, "黎杋枫");
        assert_eq!(parsed.user_name, "烧酒");
        assert_eq!(parsed.blocks.len(), 2);
        assert_eq!(parsed.blocks[0].0, "A_persona");
        assert_eq!(parsed.blocks[0].1, "你是黎杋枫。性格知性稳重。");
        assert_eq!(parsed.blocks[1].0, "E_rules");
        assert_eq!(parsed.blocks[1].1, "用||分隔回复。");
    }

    #[test]
    fn test_parse_multiline_toml() {
        let content = r#"[identity]
assistant_name = "测试助手"
user_name = "用户"

[blocks]
A_persona = """
第一行内容
第二行内容
第三行内容
"""
"#;

        let parsed = parse_persona_toml(content).expect("解析失败");
        assert_eq!(parsed.assistant_name, "测试助手");
        assert_eq!(parsed.blocks.len(), 1);
        assert!(parsed.blocks[0].1.contains("第一行内容"));
        assert!(parsed.blocks[0].1.contains("第二行内容"));
        assert!(parsed.blocks[0].1.contains("第三行内容"));
    }

    #[test]
    fn test_parse_empty_blocks() {
        let content = r#"[identity]
assistant_name = "test"
user_name = "user"

[blocks]
"#;

        let parsed = parse_persona_toml(content).expect("解析失败");
        assert_eq!(parsed.assistant_name, "test");
        assert_eq!(parsed.blocks.len(), 0); // 空 blocks 节
    }

    #[test]
    fn test_parse_missing_assistant_name() {
        let content = r#"[identity]
user_name = "user"
"#;

        let err = parse_persona_toml(content).unwrap_err();
        let err_msg = format!("{}", err);
        assert!(err_msg.contains("assistant_name"));
    }

    #[test]
    fn test_parse_unclosed_multiline() {
        let content = r#"[identity]
assistant_name = "test"

[blocks]
A_persona = """
未闭合的多行字符串
"#;

        let err = parse_persona_toml(content).unwrap_err();
        let err_msg = format!("{}", err);
        assert!(err_msg.contains("未闭合"));
    }

    #[test]
    fn test_parse_with_comments() {
        let content = r#"# 这是注释
[identity]
assistant_name = "name"
# 这也是注释
user_name = "user"
"#;

        let parsed = parse_persona_toml(content).expect("解析失败");
        assert_eq!(parsed.assistant_name, "name");
        assert_eq!(parsed.user_name, "user");
    }

    #[test]
    fn test_parse_with_escaped_quotes() {
        let content = r#"[identity]
assistant_name = "name"
user_name = "user"

[blocks]
A_persona = '你是黎杋枫。被问及"是否是AI"时温柔回避。'
"#;

        // 注意：这里值使用了单引号括起来的普通字符串，
        // 解析器将其当作不带引号的原始值
        let parsed = parse_persona_toml(content).expect("解析失败");
        assert_eq!(parsed.assistant_name, "name");
        assert_eq!(parsed.blocks.len(), 1);
        // 单引号的值会被当作字面量（包括引号本身）
        let block_value = &parsed.blocks[0].1;
        assert!(block_value.contains("黎杋枫"));
    }

    // =========================================================
    // 辅助函数测试
    // =========================================================

    #[test]
    fn test_parse_profile_field_english() {
        assert_eq!(parse_profile_field("BasicInfo"), ProfileField::BasicInfo);
        assert_eq!(
            parse_profile_field("PersonalStatus"),
            ProfileField::PersonalStatus
        );
        assert_eq!(parse_profile_field("Interests"), ProfileField::Interests);
        assert_eq!(parse_profile_field("Social"), ProfileField::Social);
        assert_eq!(parse_profile_field("History"), ProfileField::History);
        assert_eq!(
            parse_profile_field("RecentContext"),
            ProfileField::RecentContext
        );
        assert_eq!(
            parse_profile_field("SpeakingStyle"),
            ProfileField::SpeakingStyle
        );
    }

    #[test]
    fn test_parse_profile_field_chinese() {
        assert_eq!(parse_profile_field("身份信息"), ProfileField::BasicInfo);
        assert_eq!(
            parse_profile_field("性格描述"),
            ProfileField::PersonalStatus
        );
        assert_eq!(parse_profile_field("兴趣爱好"), ProfileField::Interests);
        assert_eq!(parse_profile_field("社交关系"), ProfileField::Social);
        assert_eq!(parse_profile_field("背景"), ProfileField::History);
        assert_eq!(parse_profile_field("近期背景"), ProfileField::RecentContext);
        assert_eq!(parse_profile_field("说话风格"), ProfileField::SpeakingStyle);
    }

    #[test]
    fn test_parse_profile_field_unknown() {
        // 未知字段回退为 BasicInfo
        assert_eq!(parse_profile_field("UnknownField"), ProfileField::BasicInfo);
    }

    #[test]
    fn test_parse_trait_layer_english() {
        assert_eq!(parse_trait_layer("Base"), TraitLayer::Base);
        assert_eq!(parse_trait_layer("Primary"), TraitLayer::Primary);
        assert_eq!(parse_trait_layer("Accent"), TraitLayer::Accent);
    }

    #[test]
    fn test_parse_trait_layer_chinese() {
        assert_eq!(parse_trait_layer("底色"), TraitLayer::Base);
        assert_eq!(parse_trait_layer("主色调"), TraitLayer::Primary);
        assert_eq!(parse_trait_layer("点缀"), TraitLayer::Accent);
    }

    #[test]
    fn test_parse_trait_layer_unknown() {
        // 未知回退为 Accent
        assert_eq!(parse_trait_layer("Unknown"), TraitLayer::Accent);
    }

    #[test]
    fn test_format_blocks_for_prompt() {
        let blocks = vec![
            ("A_persona".to_string(), "你是助手".to_string()),
            ("E_rules".to_string(), "用||分隔".to_string()),
        ];
        let formatted = format_blocks_for_prompt(&blocks);
        assert!(formatted.contains("## A_persona"));
        assert!(formatted.contains("你是助手"));
        assert!(formatted.contains("## E_rules"));
        assert!(formatted.contains("用||分隔"));
    }

    // =========================================================
    // ColdStartConfig 测试
    // =========================================================

    #[test]
    fn test_cold_start_config_default() {
        let cfg = ColdStartConfig::default();
        assert_eq!(cfg.temperature, 0.3);
        assert_eq!(cfg.max_tokens, 4096);
    }

    // =========================================================
    // ColdStartResponse JSON 反序列化测试
    // =========================================================

    #[test]
    fn test_deserialize_full_response() {
        let json = r#"{
            "facts": [
                {"field": "BasicInfo", "content": "她的名字是黎杋枫", "source": "Manual"},
                {"field": "PersonalStatus", "content": "她性格知性稳重"}
            ],
            "traits": [
                {
                    "layer": "Base",
                    "trait_label": "知性稳重",
                    "meaning": "以理性克制的态度交流",
                    "not_meaning": null,
                    "trigger": "日常对话",
                    "suppress": null,
                    "related": "理性,情绪稳定"
                }
            ]
        }"#;

        let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(resp.facts.len(), 2);
        assert_eq!(resp.facts[0].field, "BasicInfo");
        assert_eq!(resp.facts[0].content, "她的名字是黎杋枫");
        assert_eq!(resp.traits.len(), 1);
        assert_eq!(resp.traits[0].layer, "Base");
        assert_eq!(resp.traits[0].trait_label, "知性稳重");
    }

    #[test]
    fn test_deserialize_minimal_response() {
        let json = r#"{"facts": [], "traits": []}"#;
        let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
        assert!(resp.facts.is_empty());
        assert!(resp.traits.is_empty());
    }

    #[test]
    fn test_deserialize_missing_source() {
        // source 字段可选，缺失时应为 None
        let json = r#"{
            "facts": [{"field": "BasicInfo", "content": "测试"}],
            "traits": []
        }"#;
        let resp: ColdStartResponse = serde_json::from_str(json).expect("反序列化失败");
        assert_eq!(resp.facts[0].source, None);
    }

    // =========================================================
    // ColdStartResult 测试
    // =========================================================

    #[test]
    fn test_cold_start_result_new() {
        let result = ColdStartResult {
            persona_uid: "rama-0001".to_string(),
            is_new: true,
            fact_count: 15,
            trait_count: 8,
        };
        assert_eq!(result.persona_uid, "rama-0001");
        assert!(result.is_new);
        assert_eq!(result.fact_count, 15);
        assert_eq!(result.trait_count, 8);
    }

    #[test]
    fn test_cold_start_result_existing() {
        let result = ColdStartResult {
            persona_uid: "rama-0001".to_string(),
            is_new: false,
            fact_count: 20,
            trait_count: 10,
        };
        assert!(!result.is_new);
    }

    // =========================================================
    // 集成测试：完整的 persona.toml 解析 + Prompt 构建
    // =========================================================

    #[test]
    fn test_real_persona_toml_parse_and_prompt() {
        // 使用真实的 config/persona.toml 简化版
        let content = r#"[identity]
assistant_name = "黎杋枫"
user_name = "烧酒"

[blocks]

A_persona = """
你是黎杋枫。女，生日3月21日。
你是烧酒的学习伙伴和生活挚友。
性格：知性稳重，情绪稳定，偶有冷幽默。
"""

E_rules = """
用||分隔成多条短句发送。
不反问。不重复对方的词开头。
"""
"#;

        let parsed = parse_persona_toml(content).expect("真实配置解析失败");
        assert_eq!(parsed.assistant_name, "黎杋枫");
        assert_eq!(parsed.user_name, "烧酒");
        assert!(parsed.blocks.len() >= 2);

        let prompt = COLD_START_PROMPT.replace(
            "{persona_config}",
            &format_blocks_for_prompt(&parsed.blocks),
        );
        assert!(prompt.contains("黎杋枫"));
        assert!(prompt.contains("知性稳重"));
        assert!(prompt.contains("用||分隔"));
        assert!(!prompt.contains("{persona_config}")); // 占位符已被替换
    }
}
