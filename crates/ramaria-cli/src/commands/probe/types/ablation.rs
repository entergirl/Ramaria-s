//! crates/ramaria-cli/src/commands/probe/types/ablation.rs - 探针消融档位定义
//!
//! 设计特点:
//! - 档位名称集合与 AblationProfile 的解析 / 描述 / 注入层闸门映射。
//! - 由 types 以 mod ablation 收纳并 pub use 对外（types::AblationProfile 路径不变）。

use super::{ProbeVariant, VariantOverrides};

// =========================================================
// 消融档位 Profile
// =========================================================

/// 消融档位 Profile 名称集合（数据集 `variants[].ablation` 可取值）。
///
/// 语义:
/// - `B0`: 基线 A——无记忆注入（纯角色 + 当前对话）。
/// - `B1`: 基线 B——压缩视图注入（仅摘要/转述 RAG，无原文无行为无知识）。
/// - `F0`: 完整体系（行为+知识+表达+脉络全开，等同 ablation=None）。
/// - `F1`~`F4`: 逐层关闭（−行为 / −知识 / −表达 / −脉络）。
/// - `S_behavior` / `S_knowledge` / `S_expression` / `S_narrative`:
///   替代对照——去掉 RAG 摘要基座，只单独注入该层（对照 B1 判定"单层能否替代 RAG"）。
/// - `I_behavior` / `I_knowledge` / `I_expression` / `I_narrative`:
///   净增量对照——保留 B1 压缩摘要基座 + 仅叠加一个专属层（对照 B1 判定"在该层上的净增量"）。
pub const ABLATION_PROFILE_NAMES: [&str; 15] = [
    "B0",
    "B1",
    "F0",
    "F1",
    "F2",
    "F3",
    "F4",
    "S_behavior",
    "S_knowledge",
    "S_expression",
    "S_narrative",
    "I_behavior",
    "I_knowledge",
    "I_expression",
    "I_narrative",
];

// =========================================================
// 消融对照语义
// =========================================================
// 消融档位回答三类不同的问题，必须区分口径:
//
// 1) 替代对照（`S_behavior` / `S_knowledge` / `S_expression` / `S_narrative`）:
//    - 定义: B1 基座中"去掉 RAG 压缩摘要"，只保留目标专属层单独注入。
//    - 闸门: memory_rag=false，仅目标专属层闸门为 true。
//    - 回答问题: "目标层能否独立替代 RAG 摘要基座"，测的是单层替代能力。
//    - 局限: 该口径测不出"在 RAG 之上叠加一层的净增量"。
//
// 2) 净增量对照（`I_behavior` / `I_knowledge` / `I_expression` / `I_narrative`）:
//    - 定义: B1 压缩摘要基座 + 仅叠加一个目标专属层，其余专属层全部关闭。
//    - 闸门: memory_rag=true（保留 B1 基座），仅目标专属层闸门为 true，
//      其余专属层（behavior/knowledge/speaking_style+examples+utt/narrative+bridge）为 false。
//    - 回答问题: "在 RAG 摘要基座之上加一层的净增量"，与 B1 配对做统计检验。
//
// 3) 移除对照（`F1`~`F4`）与基线:
//    - F1~F4: 全开（F0）基础上关闭对应专属层 → 回答"去掉某一层的边际损失"。
//    - B0/B1/F0 为两个基线与完整体系锚点；F0 与 ablation=None 完全一致。
//
// 专属层 → 闸门集合映射（行为/knowledge/speaking_style/examples/utt/narrative/bridge）:
// - 行为层: behavior
// - 知识层: knowledge
// - 表达层: speaking_style + examples + utt
// - 脉络层: narrative + bridge
// - RAG 摘要基座: memory_rag
//
/// 消融档位 Profile。
///
/// 职责:
/// - 把消融档位映射为
///   `RamariaConfig.injection`（注入层闸门）覆盖集，使 B0/B1/F0/F1~F4/S_*/I_*
///   在单次 `send_message` 内真实关闭/保留对应注入层。
/// - `F0` 覆盖集为空（全开），与 `ablation=None` 行为完全一致（向后兼容）。
/// - S 组为替代对照（去 RAG 摘要），I 组为净增量对照（保留 B1 RAG 基座）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AblationProfile {
    /// 基线 A：无记忆注入（纯角色 + 当前对话）。
    B0,
    /// 基线 B：压缩视图注入（仅 RAG 摘要/转述，无原文无行为无知识）。
    B1,
    /// 完整体系（行为+知识+表达+脉络全开）。
    F0,
    /// −行为层（关闭行为规则注入）。
    F1,
    /// −知识层（关闭事实卡片注入）。
    F2,
    /// −表达层（关闭原文样例与风格规则）。
    F3,
    /// −脉络层（关闭近期脉络与桥接）。
    F4,
    /// 替代对照：去 RAG 摘要、仅注入行为层（对照 B1 测"行为层能否替代 RAG"）。
    SBehavior,
    /// 替代对照：去 RAG 摘要、仅注入知识层。
    SKnowledge,
    /// 替代对照：去 RAG 摘要、仅注入表达层。
    SExpression,
    /// 替代对照：去 RAG 摘要、仅注入脉络层。
    SNarrative,
    /// 净增量对照：B1 基座 + 仅叠加行为层（对照 B1 测行为层净增量）。
    IBehavior,
    /// 净增量对照：B1 基座 + 仅叠加知识层。
    IKnowledge,
    /// 净增量对照：B1 基座 + 仅叠加表达层。
    IExpression,
    /// 净增量对照：B1 基座 + 仅叠加脉络层。
    INarrative,
}

impl AblationProfile {
    /// 解析档位名称（数据集 `variants[].ablation` 取值）。
    pub fn parse_name(name: &str) -> Option<Self> {
        match name {
            "B0" => Some(Self::B0),
            "B1" => Some(Self::B1),
            "F0" => Some(Self::F0),
            "F1" => Some(Self::F1),
            "F2" => Some(Self::F2),
            "F3" => Some(Self::F3),
            "F4" => Some(Self::F4),
            "S_behavior" => Some(Self::SBehavior),
            "S_knowledge" => Some(Self::SKnowledge),
            "S_expression" => Some(Self::SExpression),
            "S_narrative" => Some(Self::SNarrative),
            "I_behavior" => Some(Self::IBehavior),
            "I_knowledge" => Some(Self::IKnowledge),
            "I_expression" => Some(Self::IExpression),
            "I_narrative" => Some(Self::INarrative),
            _ => None,
        }
    }

    /// 返回档位名称（与数据集字段一致）。
    pub fn name(self) -> &'static str {
        match self {
            Self::B0 => "B0",
            Self::B1 => "B1",
            Self::F0 => "F0",
            Self::F1 => "F1",
            Self::F2 => "F2",
            Self::F3 => "F3",
            Self::F4 => "F4",
            Self::SBehavior => "S_behavior",
            Self::SKnowledge => "S_knowledge",
            Self::SExpression => "S_expression",
            Self::SNarrative => "S_narrative",
            Self::IBehavior => "I_behavior",
            Self::IKnowledge => "I_knowledge",
            Self::IExpression => "I_expression",
            Self::INarrative => "I_narrative",
        }
    }

    /// 返回人类可读描述（用于档位 description / 报告标注）。
    pub fn description(self) -> &'static str {
        match self {
            Self::B0 => "基线 A：无记忆注入（纯角色 + 当前对话）",
            Self::B1 => "基线 B：压缩视图注入（仅 RAG 摘要/转述，无原文无行为无知识）",
            Self::F0 => "完整体系（行为+知识+表达+脉络全开）",
            Self::F1 => "−行为层（关闭行为规则注入）",
            Self::F2 => "−知识层（关闭事实卡片注入）",
            Self::F3 => "−表达层（关闭原文样例与风格规则）",
            Self::F4 => "−脉络层（关闭近期脉络与桥接）",
            Self::SBehavior => "替代对照：去 RAG 摘要、仅注入行为层",
            Self::SKnowledge => "替代对照：去 RAG 摘要、仅注入知识层",
            Self::SExpression => "替代对照：去 RAG 摘要、仅注入表达层",
            Self::SNarrative => "替代对照：去 RAG 摘要、仅注入脉络层",
            Self::IBehavior => "净增量对照：B1 基座 + 行为层",
            Self::IKnowledge => "净增量对照：B1 基座 + 知识层",
            Self::IExpression => "净增量对照：B1 基座 + 表达层",
            Self::INarrative => "净增量对照：B1 基座 + 脉络层",
        }
    }

    /// 把本档位映射为 `RamariaConfig.injection`（注入层闸门）覆盖集。
    ///
    /// 说明:
    /// - `F0`/`ablation=None` → 全开（行为与未启用消融完全一致，兼容性要求）。
    /// - `F1`~`F4` → 在全开基础上关闭对应层。
    /// - `B0`/`B1`/`S_*`/`I_*` → 显式设置各闸门：
    ///   - `B1` = 仅 RAG 摘要基座（memory_rag）；
    ///   - `S_*`（替代对照）= 去 RAG 摘要、仅目标专属层（memory_rag=false）；
    ///   - `I_*`（净增量对照）= B1 基座 + 仅目标专属层（memory_rag=true）。
    pub fn apply_to(self, config: &mut ramaria_core::config::RamariaConfig) {
        use ramaria_core::config::InjectionGate;
        let g = &mut config.injection;
        match self {
            Self::B0 => {
                // 无记忆注入：除 persona 角色与当前对话外全部关闭。
                *g = InjectionGate::all_off();
            }
            Self::B1 => {
                // 压缩摘要基座：仅保留 RAG 相关记忆（经 ChatRequest 注入），
                // 关闭行为/知识/表达（风格+示例+原文）/脉络（近期脉络+桥接）。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
            }
            Self::F0 => {
                // 完整体系 = 全开（与 None 等同）。
                *g = InjectionGate::all_on();
            }
            Self::F1 => {
                g.behavior = false;
            }
            Self::F2 => {
                g.knowledge = false;
            }
            Self::F3 => {
                // −表达层：说话风格/风格规则 + 对话示例 + utt 原文样例。
                g.speaking_style = false;
                g.examples = false;
                g.utt = false;
            }
            Self::F4 => {
                // −脉络层：近期对话脉络 + 桥接。
                g.narrative = false;
                g.bridge = false;
            }
            Self::SBehavior => {
                // 替代对照：去掉 RAG 摘要基座，仅注入行为层。
                *g = InjectionGate::all_off();
                g.behavior = true;
            }
            Self::SKnowledge => {
                // 替代对照：去掉 RAG 摘要基座，仅注入知识层。
                *g = InjectionGate::all_off();
                g.knowledge = true;
            }
            Self::SExpression => {
                // 替代对照：去掉 RAG 摘要基座，仅注入表达层（风格+示例+原文样例）。
                *g = InjectionGate::all_off();
                g.speaking_style = true;
                g.examples = true;
                g.utt = true;
            }
            Self::SNarrative => {
                // 替代对照：去掉 RAG 摘要基座，仅注入脉络层（近期脉络+桥接）。
                *g = InjectionGate::all_off();
                g.narrative = true;
                g.bridge = true;
            }
            Self::IBehavior => {
                // 净增量对照：B1 RAG 基座 + 仅叠加行为层。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.behavior = true;
            }
            Self::IKnowledge => {
                // 净增量对照：B1 RAG 基座 + 仅叠加知识层。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.knowledge = true;
            }
            Self::IExpression => {
                // 净增量对照：B1 RAG 基座 + 仅叠加表达层。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.speaking_style = true;
                g.examples = true;
                g.utt = true;
            }
            Self::INarrative => {
                // 净增量对照：B1 RAG 基座 + 仅叠加脉络层。
                *g = InjectionGate::all_off();
                g.memory_rag = true;
                g.narrative = true;
                g.bridge = true;
            }
        }
    }
}

/// 构建消融档位 Profile 变体集合（B0/B1/F0、F1~F4、S_* 替代对照、I_* 净增量对照）。
///
/// 说明:
/// - utt 三参数取定稿基准（θ_gap=10 / 条数=80 / top_k=3，与 `baseline` 档位一致），
///   消融只改变记忆注入层（`ablation` 字段），不改变 utt 切分。
/// - 供评估数据集构建方把消融档位并入数据集 variants；默认构建的数据集不包含。
pub fn ablation_variants() -> Vec<ProbeVariant> {
    ABLATION_PROFILE_NAMES
        .iter()
        .map(|name| {
            let profile = AblationProfile::parse_name(name)
                .expect("ABLATION_PROFILE_NAMES 必须与 parse_name 一一对应");
            ProbeVariant {
                id: profile.name().to_string(),
                description: format!("[消融] {}", profile.description()),
                theta_gap_minutes: 10,
                max_msgs_per_block: 80,
                retrieve_top_k: 3,
                ablation: Some(profile.name().to_string()),
                overrides: VariantOverrides::default(),
            }
        })
        .collect()
}
