//! crates/ramaria-core/src/config/layers.rs - Ramaria 注入与分层配置模块
//!
//! 设计特点:
//! - 定义注入门控、L1 摘要与渐进式摘要配置
//! - 定义注入槽位、注入预算与层间去重配置
//! - 各配置组提供稳定默认值
//! - 支持 serde，供配置文件与 DB settings 共享
//! - 只描述数据，不访问外部资源

use serde::{Deserialize, Serialize};

// =========================================================
// 注入层运行时间门（探针消融专用，仅内存）
// =========================================================

/// 记忆注入层运行时间门——逐层控制对话 prompt 的记忆注入。
///
/// 职责:
/// - 承载消融评估所需的每层 on/off 开关：行为 / 知识 / 表达（说话风格+示例）/
///   utt 原文 / 脉络（近期对话脉络+桥接）/ RAG 相关记忆。
/// - 全部默认开启：不修改本结构时，对话管线行为与既有版本完全一致。
///
/// 使用约定:
/// - 本结构是"运行时内存开关"，不在 config.toml / DB settings 中持久化；
///   探针等评估场景在克隆出的配置上修改后传入 `send_message_with_config`。
/// - 字段与 prompt 段落一一对应（详见各字段注释），关闭后该段落不注入；
///   对应"层"的定义与技术报告 §16.3 消融口径一致：
///   - 行为层（`behavior`）: 情境-反应规则块。
///   - 知识层（`knowledge`）: 事实卡片（动态检索的知识块）。
///   - 表达层（`speaking_style` + `examples` + `utt`）: 说话风格 / 风格规则 /
///     对话示例 / 原文样例（原文片段）。
///   - 脉络层（`narrative` + `bridge`）: 近期对话脉络 / 桥接（上一会话尾部）。
///   - RAG 相关记忆（`memory_rag`）: ChatRequest.memory_context（L1/L2/L3 摘要检索）。
///   - 社交对话基调（`social_tone`）: 全局体裁约束，非记忆层；不参与记忆层消融，
///     全开/全关档位均保持注入（详见字段注释）。
#[derive(Debug, Clone)]
pub struct InjectionGate {
    /// 行为规则注入（`## 行为规则`）。
    pub behavior: bool,
    /// 知识层事实卡片注入（`# 知识（知识层，按需）`）。
    pub knowledge: bool,
    /// 说话风格注入（`## 说话风格` / `## 自动风格规则`，表达层子段）。
    pub speaking_style: bool,
    /// 对话示例（Few-shot `## 对话示例`，表达层子段）。
    pub examples: bool,
    /// utt 原文片段注入（`## 原文片段`，表达层"原文样例"）。
    pub utt: bool,
    /// 近期对话脉络注入（`## 近期对话脉络`，脉络层）。
    pub narrative: bool,
    /// 桥接注入（`## 桥接（上一会话尾部）`，脉络层）。
    pub bridge: bool,
    /// RAG 相关历史记忆注入（`ChatRequest.memory_context`，摘要/转述通道）。
    pub memory_rag: bool,
    /// 是否注入全局社交对话基调块（`### 社交对话基调`）。
    ///
    /// 基调是面向全部 persona 的体裁约束、非记忆层闸门：默认开启，
    /// 关闭后 prompt 仅保留 persona 风格规则（供对照/语域切换）；
    /// probe 的 statement 档按题关闭。
    pub social_tone: bool,
}

impl InjectionGate {
    /// 全部开启（默认状态；ablation=None 时行为与既有版本一致）。
    pub fn all_on() -> Self {
        Self {
            behavior: true,
            knowledge: true,
            speaking_style: true,
            examples: true,
            utt: true,
            narrative: true,
            bridge: true,
            memory_rag: true,
            // 基调是全局体裁约束：全开档位与默认路径保持一致注入。
            social_tone: true,
        }
    }

    /// 全部关闭（B0 无记忆注入：仅保留 persona 角色与当前对话）。
    pub fn all_off() -> Self {
        Self {
            behavior: false,
            knowledge: false,
            speaking_style: false,
            examples: false,
            utt: false,
            narrative: false,
            bridge: false,
            memory_rag: false,
            // 基调是全局体裁约束、非记忆层闸门：不随 B0/B1/S_*/I_* 关闭，
            // 保持这些档位既有消融语义与历史结果可比性。
            social_tone: true,
        }
    }
}

impl Default for InjectionGate {
    /// 默认全部开启（无覆盖时与既有版本行为一致）。
    fn default() -> Self {
        Self::all_on()
    }
}

// =========================================================
// L1 渐进式摘要配置（B3）
// =========================================================

/// L1 摘要相关配置（`[l1]`）。
///
/// 职责:
/// - 承载渐进式摘要（B3）触发参数：长会话按 `tail_msg_count` 切段、全段生成 L1
///   （尾段覆盖最新对话）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1Config {
    /// 渐进式摘要配置（`[l1.progressive]`）
    #[serde(default)]
    pub progressive: L1ProgressiveConfig,
}

impl Default for L1Config {
    /// 创建默认 L1 配置。
    ///
    /// 返回:
    /// - 渐进式摘要默认开启（关闭时回退 v1.6 整会话/按 utt 切分行为）。
    fn default() -> Self {
        Self {
            progressive: L1ProgressiveConfig::default(),
        }
    }
}

/// 渐进式摘要（B3）触发参数。
///
/// 职责:
/// - 长会话（消息数 > `msg_threshold` 或跨度 > `span_hours`）在封存时按段生成 L1，
///   每段独立成 L1（absorbed=0 入候选池），最后一段覆盖最新对话（尾部）。
/// - 短会话未达触发条件时回退 v1.6 行为（整会话摘要，不额外切段）。
///
/// 设计依据:
/// - 决策 D-V17-005：消息数>100 或跨度>24h（可配置）；段 L1 实时入缓冲；
///   L2 提取仍封存触发；按 `tail_msg_count` 切段、全段生成（尾段覆盖最新）。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct L1ProgressiveConfig {
    /// 渐进式摘要总开关（默认 true——关闭时回退 v1.6 行为）。
    #[serde(default = "default_progressive_enabled")]
    pub enabled: bool,
    /// 消息数触发阈值（默认 100 条）：会话消息数超过此值触发分段。
    #[serde(default = "default_progressive_msg_threshold")]
    pub msg_threshold: u32,
    /// 时间跨度触发阈值（默认 24 小时）：首末消息跨度超过此值触发分段。
    #[serde(default = "default_progressive_span_hours")]
    pub span_hours: u32,
    /// 单段最大消息条数（默认 60）：分段时每段不超过此值，尾段覆盖最新消息。
    #[serde(default = "default_progressive_tail_msg_count")]
    pub tail_msg_count: u32,
}

/// serde 默认值：渐进式摘要默认开启（关闭时回退 v1.6 行为）。
fn default_progressive_enabled() -> bool {
    true
}

/// serde 默认值：消息数触发阈值 100 条。
fn default_progressive_msg_threshold() -> u32 {
    100
}

/// serde 默认值：时间跨度触发阈值 24 小时。
fn default_progressive_span_hours() -> u32 {
    24
}

/// serde 默认值：单段最大消息条数 60。
fn default_progressive_tail_msg_count() -> u32 {
    60
}

impl Default for L1ProgressiveConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            msg_threshold: 100,
            span_hours: 24,
            tail_msg_count: 60,
        }
    }
}

// =========================================================
// 注入协调预算（RAG 基座与四层注入的协调分配）
// =========================================================

/// 参与注入协调预算的通道。
///
/// 职责:
/// - 标识一次记忆注入的组成通道：RAG 摘要（`memory_context` 独立 XML 通道）
///   与 system prompt 内的四层注入块（行为/知识/表达/脉络）。
/// - 供 `[injection_budget].order` 以强类型数组声明"超预算时的保留优先级"
///   （数值小的排在前面、越优先保留）。
///
/// 字段约定:
/// - `Rag`: RAG 基座（`ChatRequest.memory_context`，L1/L2/L3 摘要检索转述）。
/// - `Behavior`: 行为层（情境-反应规则块）。
/// - `Knowledge`: 知识层（事实卡片）。
/// - `Style`: 表达层（说话风格 / 自动风格规则 / 对话示例）。
/// - `Memory`: 脉络层（近期对话脉络 / 原文片段 / 桥接）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum InjectionSlot {
    /// RAG 基座摘要通道
    Rag,
    /// 行为层
    Behavior,
    /// 知识层
    Knowledge,
    /// 表达层
    Style,
    /// 脉络层
    Memory,
}

impl InjectionSlot {
    /// 返回通道的小写字符串标识（配置书写与日志用）。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rag => "rag",
            Self::Behavior => "behavior",
            Self::Knowledge => "knowledge",
            Self::Style => "style",
            Self::Memory => "memory",
        }
    }
}

impl std::fmt::Display for InjectionSlot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// 注入协调预算配置（`[injection_budget]`）。
///
/// 职责:
/// - 为"RAG 基座 + 四层注入"建立统一协调池：`max_injection_tokens` 限定
///   RAG 摘要与四层注入块合计的 token 上限（固定骨架不计入池），超限时按
///   `order` 从低优先通道开始整块丢弃，高优先内容完整保留。
/// - 阶段一默认关闭（`enabled=false`）：不启用时走既有各层独立预算 +
///   `apply_token_budget` 整条截断路径，行为与既有版本逐字段等价。
/// - 通道内渲染预算（行为/知识/脉络的字符预算）不受本池替代，本池是
///   通道之上的"第二道总闸"。
///
/// 安全约束:
/// - 本组只承载数量级预算与顺序，不含任何原文/隐私内容。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct InjectionBudgetConfig {
    /// 协调预算总开关（默认 false = 机制关闭，回退既有路径）。
    pub enabled: bool,
    /// RAG 摘要 + 四层注入合计 token 上限（默认 1000，机制起点非定稿值）。
    ///
    /// 说明:
    /// - 固定骨架（能力边界/角色层/当前时间）不在池内，始终完整保留。
    /// - 超限时按 `order` 从低优先通道开始整块丢弃；最高优先通道单块仍超池
    ///   时在句子边界截断兜底，保证"总注入 ≤ 本值"恒成立。
    pub max_injection_tokens: usize,
    /// RAG 摘要独立 token 上限（默认 0）。
    ///
    /// `0` = 不设独立上限（仅受 `max_injection_tokens` 总池约束）；
    /// `> 0` 时先按本值在句子边界截断 RAG 摘要，再参与总池协调。
    pub max_rag_tokens: usize,
    /// 超预算时的保留优先级（高优先在前，低优先先被整块丢弃）。
    ///
    /// 字段约定:
    /// - 默认 `[rag, behavior, knowledge, style, memory]`：RAG 基座（事实召回
    ///   主力）最先保留，随后按 行为 > 知识 > 表达 > 脉络 的层优先级。
    /// - 未列出的通道视为比所有列出通道更低优先（最先被丢弃）；重复项取首个
    ///   位置；空数组 = 不区分优先级、按装配顺序从尾部丢弃。
    pub order: Vec<InjectionSlot>,
}

impl Default for InjectionBudgetConfig {
    /// 创建默认注入协调预算配置。
    ///
    /// 返回:
    /// - 机制默认关闭（不改变既有行为）；预算上限为机制起点值（阶段一不定稿，
    ///   参数定稿在阶段二/M8 数据 Gate 之后）。
    /// - 默认保留顺序：RAG 基座 > 行为 > 知识 > 表达 > 脉络。
    fn default() -> Self {
        Self {
            enabled: false,
            max_injection_tokens: 1000,
            max_rag_tokens: 0,
            order: vec![
                InjectionSlot::Rag,
                InjectionSlot::Behavior,
                InjectionSlot::Knowledge,
                InjectionSlot::Style,
                InjectionSlot::Memory,
            ],
        }
    }
}

// =========================================================
// 层间证据去重与冲突仲裁配置
// =========================================================

/// 层间证据去重与冲突仲裁配置（`[layer_dedup]`）。
///
/// 职责:
/// - 承载注入装配前"同一事实跨层只注入一次、冲突按来源优先级保留"的独立开关。
/// - 仅服务 prompt 渲染前的注入仲裁（`ramaria-memory::prompt::layer_guard`）；
///   写库侧的事实版本链仲裁（`fact/arbitration.rs`）不读本配置。
///
/// 开关约定:
/// - `enabled=false`（默认）→ 对话管线沿用既有知识层引用级去重（RAG 覆盖集合 +
///   角色层同 id 剔除），prompt 输出与既有版本逐字段等价（回归红线）。
/// - `enabled=true` → 装配前执行跨层内容级去重与冲突仲裁（默认关闭 = 回退 v1.7）。
///
/// 安全约束:
/// - 本组只承载开关，不含原文/隐私内容。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LayerDedupConfig {
    /// 层间证据去重与冲突仲裁总开关（默认 false = 机制关闭，回退既有路径）。
    pub enabled: bool,
}

impl Default for LayerDedupConfig {
    /// 创建默认层间去重配置（默认关闭）。
    fn default() -> Self {
        Self { enabled: false }
    }
}
