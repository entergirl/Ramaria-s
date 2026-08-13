//! crates/ramaria-cli/src/commands/probe.rs - 探针 CLI 命令（probe build / probe run）
//!
//! 设计特点:
//! - `probe build`：从导入数据自动构建测试集（问题 × 参数档位组合，seed 固定可复跑），
//!   输出结构化 JSON 数据集；`dataset` 保留为 alias（D-V15-007 动词化决策）。
//! - `probe run`：按档位批量跑对话管线，结构化输出（档位 → 输出 → 指标），
//!   供 v1.6 T2 自动评分（evaluate/report）与 v1.7 T3 正式评估复用同一工具链。
//! - 探针规模：2 维（语气模仿 tone / 事实记忆 fact）× 每维 10 题（D-V14-008），
//!   正式评估可通过 `--questions-per-dim` 扩大（v1.7 ≥ 30 题）。
//! - 档位为「代表配对」（D-V14-008）：baseline + 每次只动一个参数，
//!   θ_gap / 条数上限 / top_k 各 2 档，便于归因单参数影响。
//! - 静默降级：无真实数据 / 数据源查询失败 / 文件解析失败 → 内置测试夹具数据兜底
//!   （不向用户抛错，记 warn 后继续，M2 验收要求）。
//! - 确定性抽样：内置 xorshift64* 伪随机（无外部依赖），seed 相同则测试集完全一致。
//! - 隐私红线：数据集仅含问题与参考文本（评估所需），不包含完整对话原文；
//!   日志不记录完整问题与回复（用 id / 长度代替）。

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Instant;

use anyhow::Context;
use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::error::RamariaError;
use ramaria_core::types::{MessageRole, PersonaKind, now_ms};
use ramaria_memory::utt::builder::UttBuilder;

// =========================================================
// 常量与档位定义
// =========================================================

/// 数据集 schema 版本（结构变更时递增，run 侧校验兼容性）。
const DATASET_SCHEMA_VERSION: u32 = 1;

/// 默认探针 seed（固定值，保证 `probe build` 默认输出可复跑）。
pub const DEFAULT_SEED: u64 = 2026_0810;

/// 默认每维题数（D-V14-008：2 维 × 10 题预跑）。
pub const DEFAULT_QUESTIONS_PER_DIM: usize = 10;

/// 默认目标 persona（无白名单 persona 时的兜底；fixture 数据即以此 persona 编写）。
const DEFAULT_PERSONA: &str = "char-0001";

// =========================================================
// 公共数据类型（数据集 / 结果，均需可序列化供 --json 输出与落盘）
// =========================================================

/// 探针测试集（`probe build` 的产物，`probe run` 的输入）。
///
/// 格式:
/// - `items`: 测试问题列表（tone / fact 两维）。
/// - `variants`: 参数档位组合（代表配对），run 时逐档位实验。
/// - `seed` / `questions_per_dimension`: 复跑参数（同 seed 同输出）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeDataset {
    pub schema_version: u32,
    pub seed: u64,
    pub persona_uid: String,
    pub dimensions: Vec<String>,
    pub questions_per_dimension: usize,
    /// 主数据来源: "db"（至少部分来自数据库）/ "file" / "fixture"（全部兜底）
    pub source: String,
    /// 生成时间（ISO-8601 UTC）
    pub generated_at: String,
    pub variants: Vec<ProbeVariant>,
    pub items: Vec<DatasetItem>,
}

/// 单条测试问题。
///
/// 字段约定:
/// - `id`: 维度内序号（`tone-0001` / `fact-0001`），稳定标识供结果关联。
/// - `question`: 发给对话管线的问题文本。
/// - `reference`: 参考回答（tone 为 persona 原回复、fact 为事件摘要），
///   仅作人工/自动评分参考，不注入对话管线。
/// - `source`: "db"（来自真实导入数据）或 "fixture"（内置夹具补齐）。
/// - `source_ref`: 溯源标识（session 或事件），便于回查原始数据，不记录原文。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DatasetItem {
    pub id: String,
    pub dimension: String,
    pub question: String,
    pub reference: Option<String>,
    pub source: String,
    pub source_ref: Option<String>,
}

/// 参数档位（代表配对：baseline 为 v3.1 初值，其余每次只动一个参数）。
///
/// 字段与 `[utt]` 配置组一一对应（theta_gap_minutes / max_msgs_per_block / retrieve_top_k）。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ProbeVariant {
    pub id: String,
    pub description: String,
    pub theta_gap_minutes: u32,
    pub max_msgs_per_block: u32,
    pub retrieve_top_k: u32,
}

/// 探针实验结果（`probe run` 的输出）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeExperiment {
    pub dataset_file: String,
    pub dataset_seed: u64,
    pub persona_uid: String,
    pub rebuild_utt: bool,
    pub variants: Vec<ProbeVariantResult>,
    pub generated_at: String,
}

/// 单档位实验结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeVariantResult {
    pub variant_id: String,
    pub description: String,
    pub params: VariantParams,
    pub runs: Vec<ProbeRunItem>,
    /// 该档位失败的题数（单题失败不中断批量，逐题记录原因）
    pub failed_count: usize,
}

/// 档位参数（结果中的可读形态，与 ProbeVariant 字段一致）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct VariantParams {
    pub theta_gap_minutes: u32,
    pub max_msgs_per_block: u32,
    pub retrieve_top_k: u32,
}

/// 单题运行结果。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeRunItem {
    pub item_id: String,
    pub dimension: String,
    pub question: String,
    pub reply: String,
    pub metrics: ProbeMetrics,
    /// 失败原因（成功为 None）
    pub error: Option<String>,
}

/// 单题可测指标（探针阶段：长度与耗时；语义质量由 v1.6 T2 自动评分）。
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProbeMetrics {
    /// 回复字符数
    pub reply_chars: usize,
    /// LLM 调用耗时（毫秒）
    pub elapsed_ms: u128,
}

/// probe 子命令（由 main.rs 解析 clap 参数后构造）。
#[derive(Debug, Clone)]
pub enum ProbeCmd {
    /// 构建测试集（`dataset` 保留为 alias）
    Build {
        /// 目标 persona_uid（None = 自动选白名单内第一个角色类 persona）
        persona: Option<String>,
        /// 每维题数（默认 10）
        questions_per_dim: usize,
        /// 抽样 seed（默认固定，可复跑）
        seed: u64,
        /// 显式数据源文件（可选；不指定则从数据库构建）
        source: Option<PathBuf>,
        /// 数据集输出文件（`-` = stdout；不指定时仅 --json 输出完整数据集）
        output: Option<String>,
        json: bool,
    },
    /// 执行档位实验
    Run {
        /// 数据集文件（probe build 产物）
        dataset: PathBuf,
        /// 只跑指定档位（逗号分隔 id，默认全部）
        variants: Option<String>,
        /// 每档位最多跑题数（默认全部）
        limit: Option<usize>,
        /// 是否按档位参数重建 utt 块（默认 true；θ_gap/条数档位必须重建才生效）
        rebuild_utt: bool,
        /// 结果输出文件（`-` = stdout 输出原始结果 JSON）
        output: Option<String>,
        json: bool,
    },
}

/// probe 命令入口（probe 不需要交互确认，yes 参数供线上 provider 隐私确认透传）。
pub async fn run(app: &Arc<ramaria_app::App>, cmd: ProbeCmd, yes: bool) -> anyhow::Result<()> {
    match cmd {
        ProbeCmd::Build {
            persona,
            questions_per_dim,
            seed,
            source,
            output,
            json,
        } => run_build(app, persona, questions_per_dim, seed, source, output, json).await,
        ProbeCmd::Run {
            dataset,
            variants,
            limit,
            rebuild_utt,
            output,
            json,
        } => {
            run_experiment(
                app,
                dataset,
                variants,
                limit,
                rebuild_utt,
                output,
                json,
                yes,
            )
            .await
        }
    }
}

// =========================================================
// 默认档位（代表配对，D-V14-008：各参数 2 档）
// =========================================================

/// 默认档位组合。
///
/// 设计:
/// - baseline 即 v3.1 初值（θ_gap=30 / 条数=40 / top_k=3），作为对照基准。
/// - 其余档位每次只动一个参数，便于归因单参数对输出质量的影响。
/// - 档位参数与 `[utt]` 配置组字段一一对应，直接覆盖 `UttConfig` 生效。
fn default_variants() -> Vec<ProbeVariant> {
    vec![
        ProbeVariant {
            id: "baseline".to_string(),
            description: "v3.1 初值（对照基准）".to_string(),
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
        },
        ProbeVariant {
            id: "theta_gap_60".to_string(),
            description: "θ_gap 上调至 60 分钟（相邻消息间隔更宽松，块更长）".to_string(),
            theta_gap_minutes: 60,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
        },
        ProbeVariant {
            id: "max_msgs_80".to_string(),
            description: "条数上限上调至 80（块可容纳更多消息）".to_string(),
            theta_gap_minutes: 30,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
        },
        ProbeVariant {
            id: "top_k_1".to_string(),
            description: "top_k 下调至 1（更保守的原文注入）".to_string(),
            theta_gap_minutes: 30,
            max_msgs_per_block: 40,
            retrieve_top_k: 1,
        },
    ]
}

// =========================================================
// probe build：构建测试集
// =========================================================

/// 构建测试集（供命令与脚本复用；含 fixture 兜底降级）。
///
/// 数据来源优先级:
/// 1. `--source <file>`: 显式指定的数据源文件（JSON，含 messages/events）。
/// 2. 数据库: 从导入数据构建（tone 用 persona 发言配对、fact 用 L2 事件）。
/// 3. 内置夹具: 上述路径无真实数据或构建失败时兜底（静默降级 + warn）。
///
/// 参数:
/// - `source`: 显式数据源文件（None = 从数据库构建）。
///
/// 返回:
/// - 恒成功：文件/数据库路径失败时自动降级为内置夹具（静默降级 + warn）。
pub async fn build_dataset(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<&Path>,
) -> ProbeDataset {
    let qpd = questions_per_dim.max(1);
    let target = resolve_target_persona(app, persona.as_deref()).await;

    // 按数据来源构建：文件 > 数据库 > fixture 兜底
    match source {
        Some(path) => match build_from_file(path, &target, qpd, seed).await {
            Ok(ds) => ds,
            Err(e) => {
                // 文件读取/解析失败 → 夹具兜底（静默降级，记 warn）
                tracing::warn!(
                    path = %path.display(),
                    %e,
                    "probe build 数据文件处理失败，降级为内置夹具数据"
                );
                build_from_fixture(&target, qpd, seed)
            }
        },
        None => match build_from_db(app, &target, qpd, seed).await {
            Ok(ds) => ds,
            Err(e) => {
                tracing::warn!(%e, "probe build 数据库构建失败，降级为内置夹具数据");
                build_from_fixture(&target, qpd, seed)
            }
        },
    }
}

/// 执行 `probe build`（构建 + 输出）。
async fn run_build(
    app: &Arc<ramaria_app::App>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<PathBuf>,
    output: Option<String>,
    json: bool,
) -> anyhow::Result<()> {
    let dataset = build_dataset(app, persona, questions_per_dim, seed, source.as_deref()).await;

    // 输出：--output 写数据集文件；--json 输出信封；文本模式打印摘要
    if let Some(out) = output.as_deref() {
        write_dataset_file(out, &dataset)?;
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": dataset.persona_uid,
                "source": dataset.source,
                "items": dataset.items.len(),
                "variants": dataset.variants.len(),
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "测试集已写入 {}（{} 题，{} 档位，source={}）",
            out,
            dataset.items.len(),
            dataset.variants.len(),
            dataset.source
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&dataset);
    }

    print_dataset_summary(&dataset);
    Ok(())
}

/// 从数据库构建测试集（tone 维度配对 persona 发言、fact 维度使用 L2 事件）。
async fn build_from_db(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let tone_pairs = collect_tone_pairs(app, persona_uid).await;
    let fact_items = collect_fact_events(app, persona_uid).await;

    tracing::info!(
        %persona_uid,
        tone_candidates = tone_pairs.len(),
        fact_candidates = fact_items.len(),
        "probe build 从数据库收集候选"
    );

    // 确定性抽样 + 夹具补齐（每维恒有 qpd 题，档位实验规模稳定）
    let fixture_tone = fixture_tone_pairs();
    let fixture_fact = fixture_fact_events();

    let (tone_items, tone_real) = sample_with_fallback(&tone_pairs, &fixture_tone, qpd, seed);
    let (fact_cands, fact_real) = sample_with_fallback(&fact_items, &fixture_fact, qpd, seed);

    let mut items = Vec::with_capacity(qpd * 2);
    for (idx, (question, reference)) in tone_items.into_iter().enumerate() {
        let is_real = idx < tone_real;
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: None,
        });
    }
    for (idx, (question, reference, event_title)) in fact_cands.into_iter().enumerate() {
        let is_real = idx < fact_real;
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: Some(event_title),
        });
    }

    let any_real = tone_real > 0 || fact_real > 0;
    let source = if any_real { "db" } else { "fixture" };

    if !any_real {
        tracing::warn!(%persona_uid, "probe build 无真实数据，测试集全部使用内置夹具");
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec!["tone".to_string(), "fact".to_string()],
        questions_per_dimension: qpd,
        source: source.to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 从显式数据源文件构建测试集。
///
/// 输入文件格式（JSON）:
/// ```json
/// {
///   "persona_uid": "char-0001",
///   "messages": [{"question": "...", "reply": "...", "source_ref": "..."}],
///   "events":   [{"title": "...", "summary": "..."}]
/// }
/// ```
async fn build_from_file(
    path: &Path,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("读取数据源文件失败: {}", path.display()))?;
    let raw: ProbeSourceFile = serde_json::from_str(&text)
        .with_context(|| format!("解析数据源文件失败: {}", path.display()))?;

    let persona = raw.persona_uid.unwrap_or_else(|| persona_uid.to_string());

    let tone_pairs: Vec<(String, String, Option<String>)> = raw
        .messages
        .iter()
        .filter(|m| !m.question.trim().is_empty())
        .map(|m| (m.question.clone(), m.reply.clone(), m.source_ref.clone()))
        .collect();
    let fact_cands: Vec<(String, String, String)> = raw
        .events
        .iter()
        .filter(|e| !e.title.trim().is_empty())
        .map(|e| {
            (
                format!("还记得「{}」这件事吗？", e.title),
                e.summary.clone(),
                e.title.clone(),
            )
        })
        .collect();

    let (tone_items, tone_real) = sample_with_fallback(
        &tone_pairs,
        &fixture_tone_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );
    let (fact_cands, fact_real) =
        sample_with_fallback(&fact_cands, &fixture_fact_events(), qpd, seed);

    let mut items = Vec::with_capacity(qpd * 2);
    for (idx, (question, reference, src_ref)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if idx < tone_real { "file" } else { "fixture" }.to_string(),
            source_ref: src_ref,
        });
    }
    for (idx, (question, reference, title)) in fact_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: if idx < fact_real { "file" } else { "fixture" }.to_string(),
            source_ref: Some(title),
        });
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona,
        dimensions: vec!["tone".to_string(), "fact".to_string()],
        questions_per_dimension: qpd,
        source: "file".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 全部使用内置夹具构建测试集（兜底路径）。
fn build_from_fixture(persona_uid: &str, qpd: usize, seed: u64) -> ProbeDataset {
    let (tone_items, _) = sample_with_fallback(&[], &fixture_tone_pairs(), qpd, seed);
    let (fact_cands, _) = sample_with_fallback(&[], &fixture_fact_events(), qpd, seed);

    let mut items = Vec::with_capacity(qpd * 2);
    for (idx, (question, reference)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: None,
        });
    }
    for (idx, (question, reference, title)) in fact_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("fact-{:04}", idx + 1),
            dimension: "fact".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: Some(title),
        });
    }

    ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec!["tone".to_string(), "fact".to_string()],
        questions_per_dimension: qpd,
        source: "fixture".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    }
}

/// 解析目标 persona：
/// 1. 显式指定 → 用之；
/// 2. 未指定 → 数据库第一个白名单内角色类 persona（char/anim/oc/hist）；
/// 3. 无匹配 → 默认 char-0001（夹具数据以此编写）。
async fn resolve_target_persona(app: &Arc<ramaria_app::App>, explicit: Option<&str>) -> String {
    if let Some(uid) = explicit {
        return uid.to_string();
    }
    match app.storage().list_personas().await {
        Ok(personas) => {
            let whitelisted = [
                PersonaKind::Char,
                PersonaKind::Anim,
                PersonaKind::Oc,
                PersonaKind::Hist,
            ];
            for p in &personas {
                if whitelisted.contains(&p.kind) {
                    tracing::info!(persona_uid = %p.uid, "probe build 自动选择白名单 persona");
                    return p.uid.clone();
                }
            }
        }
        Err(e) => {
            tracing::warn!(%e, "读取 persona 列表失败，使用默认 persona");
        }
    }
    tracing::info!(
        persona_uid = DEFAULT_PERSONA,
        "probe build 使用默认 persona"
    );
    DEFAULT_PERSONA.to_string()
}

/// 收集语气模仿维度候选：persona 发言与其同会话前一条 user 消息配对。
///
/// 返回 `(question, reference)` 列表（question = 用户消息，reference = persona 原回复）。
/// 查询失败按会话跳过（记 warn），不中断整体构建。
async fn collect_tone_pairs(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String)> {
    let sessions = match app.storage().list_sessions().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%e, "probe build 读取会话列表失败，语气模仿维度无候选");
            return Vec::new();
        }
    };

    let mut pairs = Vec::new();
    for session in &sessions {
        let messages = match app.storage().list_messages(session.id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(session_id = %session.id, %e, "probe build 读取会话消息失败，跳过该会话");
                continue;
            }
        };
        let mut last_user: Option<String> = None;
        for m in &messages {
            match m.role {
                MessageRole::User => {
                    last_user = Some(m.content.clone());
                }
                _ => {
                    // 目标 persona 的发言：与其前一条 user 消息配对
                    if m.persona_uid.as_deref() == Some(persona_uid)
                        && let Some(q) = last_user.take()
                    {
                        pairs.push((q, m.content.clone()));
                    }
                }
            }
        }
    }
    pairs
}

/// 收集事实记忆维度候选：L2 事件 → 模板化问题。
///
/// 返回 `(question, reference, title)`（reference = 事件摘要，title 用于溯源）。
/// 查询失败记 warn 后返回空（由上层夹具兜底）。
async fn collect_fact_events(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String, String)> {
    let events = match app
        .storage()
        .list_events_by_persona(persona_uid, 0, 10_000)
        .await
    {
        Ok(e) => e,
        Err(e) => {
            tracing::warn!(%persona_uid, %e, "probe build 读取事件失败，事实记忆维度无候选");
            return Vec::new();
        }
    };
    events
        .into_iter()
        .filter(|e| !e.title.trim().is_empty())
        .map(|e| {
            (
                format!("还记得「{}」这件事吗？", e.title),
                e.summary.clone(),
                e.title.clone(),
            )
        })
        .collect()
}

/// 确定性抽样 + 夹具补齐：从真实候选池抽 `count` 条，不足部分用夹具补满。
///
/// 返回 `(抽取结果, 真实条数, 夹具补齐条数)`。
/// 真实候选为空时直接取夹具前 `count` 条（保证确定性）。
fn sample_with_fallback<T: Clone>(
    candidates: &[T],
    fixture: &[T],
    count: usize,
    seed: u64,
) -> (Vec<T>, usize) {
    if candidates.is_empty() {
        let taken = fixture.iter().take(count).cloned().collect::<Vec<_>>();
        return (taken, 0);
    }
    let mut rng = DeterministicRng::new(seed);
    let mut pool = candidates.to_vec();
    rng.shuffle(&mut pool);
    let mut out: Vec<T> = pool.into_iter().take(count).collect();
    let real_n = out.len();
    for item in fixture {
        if out.len() >= count {
            break;
        }
        out.push(item.clone());
    }
    (out, real_n)
}

// =========================================================
// probe run：档位批量实验
// =========================================================

/// 执行 `probe run`。
///
/// 流程:
/// 1. 读取数据集（probe build 产物），校验 schema 版本与维度。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 4. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
// 参数为命令入口的完整输入集合（含输出模式与隐私透传），合并会降低可读性；
// 与 app_chat.rs 的 `build_system_prompt_with_context` 采用同一 allow 约定。
#[allow(clippy::too_many_arguments)]
async fn run_experiment(
    app: &Arc<ramaria_app::App>,
    dataset_path: PathBuf,
    variants_filter: Option<String>,
    limit: Option<usize>,
    rebuild_utt: bool,
    output: Option<String>,
    json: bool,
    yes: bool,
) -> anyhow::Result<()> {
    // Step 1: 读取并校验数据集（文件缺失/解析失败 → 业务校验失败，exit code 4）
    let text = std::fs::read_to_string(&dataset_path).with_context(|| {
        format!(
            "读取数据集失败: {}（请先运行 `ramaria probe build` 生成）",
            dataset_path.display()
        )
    })?;
    let dataset: ProbeDataset = serde_json::from_str(&text)
        .map_err(|e| RamariaError::validation(format!("数据集解析失败: {e}")))?;
    if dataset.items.is_empty() {
        return Err(anyhow::anyhow!(RamariaError::validation(
            "数据集不含任何测试问题"
        )));
    }

    // Step 2-5: 构建档位实验结果（隐私确认/配置基准/逐档位批量；单题失败不中断）
    let experiment = build_experiment(
        app,
        &dataset,
        &dataset_path,
        variants_filter.as_deref(),
        limit,
        rebuild_utt,
        yes,
    )
    .await?;

    let run_count: usize = experiment.variants.iter().map(|v| v.runs.len()).sum();
    tracing::info!(run_count, "probe run 完成");

    // Step 6: 输出
    if let Some(out) = output.as_deref() {
        write_experiment_file(out, &experiment)?;
        if json {
            let data = serde_json::json!({
                "file": out,
                "persona_uid": experiment.persona_uid,
                "variants": experiment.variants.len(),
                "runs": run_count,
            });
            return crate::json::emit_ok(&data);
        }
        crate::ui::success(&format!(
            "实验结果已写入 {}（{} 档位，{} 次运行）",
            out,
            experiment.variants.len(),
            run_count
        ));
        return Ok(());
    }

    if json {
        return crate::json::emit_ok(&experiment);
    }

    print_experiment_summary(&experiment);
    Ok(())
}

/// 构建档位实验结果（供命令输出与 v1.6 T2 自动评分复用）。
///
/// 流程:
/// 1. 校验数据集（schema 版本不匹配记 warn 继续）。
/// 2. 加载生效配置（config.toml + DB 双写合并）作为档位基准。
/// 3. 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）。
/// 4. 逐档位：覆盖 utt 三参数 →（可选）按档位参数重建 utt 块 → 逐题跑对话管线。
/// 5. 单题/单档位失败均不中断其余（记 warn + 记录失败原因）。
pub async fn build_experiment(
    app: &Arc<ramaria_app::App>,
    dataset: &ProbeDataset,
    dataset_path: &Path,
    variants_filter: Option<&str>,
    limit: Option<usize>,
    rebuild_utt: bool,
    yes: bool,
) -> anyhow::Result<ProbeExperiment> {
    if dataset.schema_version != DATASET_SCHEMA_VERSION {
        tracing::warn!(
            schema = dataset.schema_version,
            expected = DATASET_SCHEMA_VERSION,
            "数据集 schema 版本不匹配，继续尝试执行"
        );
    }

    // Step 2: 加载生效配置作为档位基准（失败降级为 App 默认配置）
    let base_config = load_effective_config(app).await;

    // Step 3: 隐私确认（线上 provider 需确认；本地 LM Studio 直接通过）
    crate::privacy::ensure_privacy(app, yes).await?;

    // Step 4: 过滤档位（--variants；无效 id 记 warn 跳过）
    let variants = filter_variants(&dataset.variants, variants_filter);

    tracing::info!(
        dataset = %dataset_path.display(),
        persona_uid = %dataset.persona_uid,
        items = dataset.items.len(),
        variants = variants.len(),
        rebuild_utt,
        "probe run 开始"
    );

    // Step 5: 逐档位实验
    let mut results = Vec::with_capacity(variants.len());
    for variant in &variants {
        // 覆盖 utt 三参数（档位基准 + 单参数变化）
        let mut variant_config = base_config.clone();
        variant_config.utt.theta_gap_minutes = variant.theta_gap_minutes;
        variant_config.utt.max_msgs_per_block = variant.max_msgs_per_block;
        variant_config.utt.retrieve_top_k = variant.retrieve_top_k;

        tracing::info!(
            variant_id = %variant.id,
            theta_gap_minutes = variant.theta_gap_minutes,
            max_msgs_per_block = variant.max_msgs_per_block,
            retrieve_top_k = variant.retrieve_top_k,
            "probe run 档位开始"
        );

        // 按档位参数重建 utt 块（θ_gap/条数档位必须重建才生效；失败记 warn 继续）
        if rebuild_utt && let Err(e) = rebuild_utt_for_config(app, &variant_config).await {
            tracing::warn!(
                variant_id = %variant.id,
                %e,
                "档位 utt 块重建失败，本次档位可能未按目标参数生效"
            );
        }

        let mut runs = Vec::new();
        let mut failed = 0usize;
        let max_runs = limit
            .unwrap_or(dataset.items.len())
            .min(dataset.items.len());
        for item in dataset.items.iter().take(max_runs) {
            let result =
                run_single_question(app, &variant_config, &dataset.persona_uid, item).await;
            if result.error.is_some() {
                failed += 1;
                tracing::warn!(
                    variant_id = %variant.id,
                    item_id = %result.item_id,
                    error = result.error.as_deref().unwrap_or(""),
                    "probe run 单题失败（不中断批量）"
                );
            }
            runs.push(result);
        }

        tracing::info!(
            variant_id = %variant.id,
            runs = runs.len(),
            failed,
            "probe run 档位完成"
        );

        results.push(ProbeVariantResult {
            variant_id: variant.id.clone(),
            description: variant.description.clone(),
            params: VariantParams {
                theta_gap_minutes: variant.theta_gap_minutes,
                max_msgs_per_block: variant.max_msgs_per_block,
                retrieve_top_k: variant.retrieve_top_k,
            },
            runs,
            failed_count: failed,
        });
    }

    Ok(ProbeExperiment {
        dataset_file: dataset_path.display().to_string(),
        dataset_seed: dataset.seed,
        persona_uid: dataset.persona_uid.clone(),
        rebuild_utt,
        variants: results,
        generated_at: now_iso8601(),
    })
}

/// 加载生效配置（config.toml + DB 双写合并，与 `blocks rebuild` 一致）。
/// 加载失败记 warn 并降级为 App 默认配置（不阻塞探针）。
async fn load_effective_config(app: &Arc<ramaria_app::App>) -> RamariaConfig {
    let config_path = PathBuf::from(&app.config().paths.config_dir).join("config.toml");
    let sync = ramaria_app::ConfigSyncService::new(app.storage().clone(), config_path);
    match sync.load_config_only().await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(%e, "读取生效配置失败，档位基准使用 App 默认配置");
            app.config().clone()
        }
    }
}

/// 按档位参数重建 utt 块（与 `blocks rebuild --force` 语义一致）。
///
/// 说明:
/// - 清空全部 utt 块后按目标参数全量重切（增量语义不会按新参数重切旧块）。
/// - embedding 不可用时块照常入库（仅无向量，检索退化为关键词通道）。
/// - 失败返回 Err，由调用方记 warn 后继续（档位实验不中断）。
async fn rebuild_utt_for_config(
    app: &Arc<ramaria_app::App>,
    config: &RamariaConfig,
) -> anyhow::Result<()> {
    let sessions = app.storage().list_sessions().await?;
    for session in &sessions {
        app.storage()
            .delete_utt_blocks_by_session(session.id)
            .await?;
    }
    let builder = UttBuilder::from_config(&config.utt);
    let embedding = app.embedding_provider();
    let embedder: Option<&dyn ramaria_core::EmbeddingProvider> =
        embedding.as_ref().map(|arc| arc.as_ref());
    builder
        .rebuild_all(app.storage().as_ref(), embedder)
        .await?;
    app.rebuild_retriever().await?;
    Ok(())
}

/// 跑单题对话并收集输出与指标。
///
/// 降级策略:
/// - `send_message` 本身失败（状态/隐私/存储）→ 记录 error，指标置零。
/// - 流内 Error 事件 → 记录 error，reply 保留已收到的部分。
async fn run_single_question(
    app: &Arc<ramaria_app::App>,
    config: &RamariaConfig,
    persona_uid: &str,
    item: &DatasetItem,
) -> ProbeRunItem {
    let start = Instant::now();
    let mut reply = String::new();
    let mut total_chars = 0usize;
    let mut error: Option<String> = None;

    let stream = match app
        .send_message_with_config(&item.question, Some(persona_uid), None, config)
        .await
    {
        Ok(s) => s,
        Err(e) => {
            return ProbeRunItem {
                item_id: item.id.clone(),
                dimension: item.dimension.clone(),
                question: item.question.clone(),
                reply: String::new(),
                metrics: ProbeMetrics {
                    reply_chars: 0,
                    elapsed_ms: start.elapsed().as_millis(),
                },
                error: Some(e.to_string()),
            };
        }
    };

    let mut stream = stream;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                ramaria_app::stream_event::StreamEvent::Delta { content, .. } => {
                    reply.push_str(&content);
                }
                ramaria_app::stream_event::StreamEvent::Done {
                    total_chars: tc, ..
                } => {
                    total_chars = tc;
                }
                ramaria_app::stream_event::StreamEvent::Error { error: e, .. }
                    if error.is_none() =>
                {
                    error = Some(e);
                }
                _ => {
                    // StreamEvent 为 #[non_exhaustive]，忽略未知事件类型
                }
            },
            Err(e) => {
                if error.is_none() {
                    error = Some(e.to_string());
                }
            }
        }
    }

    // 隐私红线：日志不记录完整问题与回复，仅记长度
    tracing::debug!(
        item_id = %item.id,
        reply_chars = reply.chars().count(),
        total_chars,
        has_error = error.is_some(),
        "probe run 单题完成"
    );

    let reply_chars = reply.chars().count();
    let reply = if total_chars > 0 {
        reply.chars().take(total_chars).collect()
    } else {
        reply
    };

    ProbeRunItem {
        item_id: item.id.clone(),
        dimension: item.dimension.clone(),
        question: item.question.clone(),
        reply,
        metrics: ProbeMetrics {
            reply_chars,
            elapsed_ms: start.elapsed().as_millis(),
        },
        error,
    }
}

/// 按 --variants 过滤档位（逗号分隔；无效 id 记 warn 跳过）。
fn filter_variants(variants: &[ProbeVariant], filter: Option<&str>) -> Vec<ProbeVariant> {
    let Some(filter) = filter else {
        return variants.to_vec();
    };
    let ids: Vec<&str> = filter
        .split(',')
        .map(|s| s.trim())
        .filter(|s| !s.is_empty())
        .collect();
    let mut out = Vec::new();
    for id in ids {
        match variants.iter().find(|v| v.id == id) {
            Some(v) => out.push(v.clone()),
            None => {
                tracing::warn!(variant_id = id, "probe run 忽略未知档位 id");
            }
        }
    }
    if out.is_empty() {
        tracing::warn!("probe run 档位过滤结果为空，回退为全部档位");
        variants.to_vec()
    } else {
        out
    }
}

// =========================================================
// 输出辅助
// =========================================================

/// 写数据集到文件（`-` 表示 stdout，输出原始数据集 JSON）。
fn write_dataset_file(out: &str, dataset: &ProbeDataset) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(dataset).context("数据集序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入数据集失败: {out}"))?;
    }
    Ok(())
}

/// 写实验结果到文件（`-` 表示 stdout，输出原始结果 JSON）。
fn write_experiment_file(out: &str, experiment: &ProbeExperiment) -> anyhow::Result<()> {
    let json = serde_json::to_string_pretty(experiment).context("实验结果序列化失败")?;
    if out == "-" {
        println!("{json}");
    } else {
        std::fs::write(out, format!("{json}\n"))
            .with_context(|| format!("写入实验结果失败: {out}"))?;
    }
    Ok(())
}

/// 文本模式打印数据集摘要（stdout 仅输出数据，提示走 stderr）。
fn print_dataset_summary(dataset: &ProbeDataset) {
    let tone = dataset
        .items
        .iter()
        .filter(|i| i.dimension == "tone")
        .count();
    let fact = dataset
        .items
        .iter()
        .filter(|i| i.dimension == "fact")
        .count();
    let real = dataset
        .items
        .iter()
        .filter(|i| i.source != "fixture")
        .count();
    println!(
        "probe 测试集: persona={} | 维度=tone({})/fact({}) | 档位={} | 真实数据 {} 题 / 夹具 {} 题 | source={}",
        dataset.persona_uid,
        tone,
        fact,
        dataset.variants.len(),
        real,
        dataset.items.len() - real,
        dataset.source
    );
    println!("seed={}（相同 seed 可复跑相同测试集）", dataset.seed);
    for v in &dataset.variants {
        println!(
            "  档位 {:<14} θ_gap={:<3} 条数={:<3} top_k={}  — {}",
            v.id, v.theta_gap_minutes, v.max_msgs_per_block, v.retrieve_top_k, v.description
        );
    }
    crate::ui::info("运行 `ramaria probe run --dataset <文件>` 执行档位实验");
}

/// 文本模式打印实验结果摘要。
fn print_experiment_summary(experiment: &ProbeExperiment) {
    println!(
        "probe 实验结果: persona={} | {} 档位 | 数据集 seed={}",
        experiment.persona_uid,
        experiment.variants.len(),
        experiment.dataset_seed
    );
    for v in &experiment.variants {
        let success = v.runs.len() - v.failed_count;
        let avg_ms: u128 = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.elapsed_ms).sum::<u128>() / v.runs.len() as u128
        };
        let avg_chars: usize = if v.runs.is_empty() {
            0
        } else {
            v.runs.iter().map(|r| r.metrics.reply_chars).sum::<usize>() / v.runs.len()
        };
        println!(
            "  档位 {:<14} 成功={}/{:<3} 平均回复 {} 字符 / {} ms  — {}",
            v.variant_id,
            success,
            v.runs.len(),
            avg_chars,
            avg_ms,
            v.description
        );
    }
    crate::ui::info("完整结果含每题 reply/metrics，可用 --json 或 --output 获取");
}

/// 当前时间（ISO-8601 UTC，M1 约定）。
fn now_iso8601() -> String {
    crate::util::format_timestamp_iso(now_ms())
        .unwrap_or_else(|| "1970-01-01T00:00:00Z".to_string())
}

// =========================================================
// 确定性伪随机（xorshift64*，无外部依赖，seed 固定可复跑）
// =========================================================

/// 确定性伪随机数生成器。
///
/// 职责:
/// - 为测试集抽样提供可复现的随机序列（同 seed → 同测试集）。
/// - 使用 xorshift64*（无外部 crate 依赖，实现短、可单测）。
struct DeterministicRng {
    state: u64,
}

impl DeterministicRng {
    /// 创建生成器（seed=0 时使用固定非零种子，避免全零状态退化）。
    fn new(seed: u64) -> Self {
        Self {
            state: if seed == 0 {
                0x9E37_79B9_7F4A_7C15
            } else {
                seed
            },
        }
    }

    /// 下一个 u64 随机数（xorshift64*）。
    fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// 返回 `[0, bound)` 内的随机数。
    fn next_usize(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// Fisher-Yates 洗牌（同 seed 同顺序，保证抽样可复跑）。
    fn shuffle<T>(&mut self, items: &mut [T]) {
        for i in (1..items.len()).rev() {
            let j = self.next_usize(i + 1);
            items.swap(i, j);
        }
    }
}

// =========================================================
// 内置测试夹具数据（构建失败时的兜底，D-V14-008 探针规模）
// =========================================================

/// 数据源文件输入格式（probe build --source）。
#[derive(Debug, serde::Deserialize)]
struct ProbeSourceFile {
    persona_uid: Option<String>,
    #[serde(default)]
    messages: Vec<SourceMessage>,
    #[serde(default)]
    events: Vec<SourceEvent>,
}

#[derive(Debug, serde::Deserialize)]
struct SourceMessage {
    question: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    source_ref: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SourceEvent {
    title: String,
    #[serde(default)]
    summary: String,
}

/// 内置语气模仿夹具（(用户问题, persona 原回复) 配对）。
///
/// 内容说明: 示例角色 persona（char-0001）的典型回应风格——
/// 工作吐槽安抚、生活建议、情绪陪伴，覆盖 v1.5「行为驱动」的目标情境。
fn fixture_tone_pairs() -> Vec<(String, String)> {
    vec![
        (
            "今天上班被领导批评了，说我方案写得不行".to_string(),
            "别太往心里去，领导批评方案不代表否定你这个人。把意见一条条记下来，改完这版肯定能行。"
                .to_string(),
        ),
        (
            "周末想约你出来玩，你有空吗".to_string(),
            "周末我一般不安排太满。你想去哪里？公园散步或者找家安静的咖啡馆都行。".to_string(),
        ),
        (
            "我养的猫今天吐了，好担心".to_string(),
            "先观察一下是不是吃太快或者毛球。如果持续吐或者精神不好，尽快带去看医生比较稳妥。"
                .to_string(),
        ),
        (
            "项目又要延期了，客户催得紧".to_string(),
            "延期不全是坏事，说明需求一直在变。跟客户沟通好新的时间点，把预期管理做到位更重要。"
                .to_string(),
        ),
        (
            "晚上总是失眠，脑子里全是工作的事".to_string(),
            "躺下前把明天要做的事写在纸上，清空脑袋再睡。工作的事明天再想也不迟。".to_string(),
        ),
        (
            "今天终于把健身卡办了，坚持了一个月".to_string(),
            "坚持一个月已经很厉害了，最难的起步阶段你已经过了。接下来保持节奏就行，别贪快。"
                .to_string(),
        ),
        (
            "想换个工作，又怕不稳定".to_string(),
            "有换工作的念头说明你在成长。先想清楚你最在意什么——工资、发展还是氛围，排个序再决定。"
                .to_string(),
        ),
        (
            "手机丢了，里面有很多照片".to_string(),
            "照片丢了确实心疼。以后重要的照片记得备份到云端，这次就当买个教训吧。".to_string(),
        ),
        (
            "跟室友吵架了，不知道怎么办".to_string(),
            "先冷静一晚，明天再谈。吵架时说的话都当不得真，等情绪过去再沟通才是正事。".to_string(),
        ),
        (
            "今天加班到十点，累死了".to_string(),
            "辛苦了，早点回去休息。工作是做不完的，身体才是自己的。".to_string(),
        ),
        (
            "第一次做饭，把厨房搞得一团糟".to_string(),
            "第一次做饭都这样，谁都是从炸厨房开始的。能吃就行，下次一定会更好。".to_string(),
        ),
        (
            "准备考研，但一直静不下心".to_string(),
            "学习最难的是开始那半小时。先把手机放远，定个 25 分钟的小目标，进入状态就好了。"
                .to_string(),
        ),
    ]
}

/// 内置事实记忆夹具（(问题, 事件摘要, 事件标题)）。
fn fixture_fact_events() -> Vec<(String, String, String)> {
    let raw = [
        (
            "东京旅行",
            "2024 年 3 月和朋友去了东京，看了樱花，去了浅草寺和秋叶原，非常开心。",
        ),
        (
            "养猫",
            "去年收养了一只三花猫，取名「团子」，现在一岁半，性格粘人。",
        ),
        (
            "跳槽到新公司",
            "2025 年初从上一家公司跳槽，现在做后端开发，团队氛围不错。",
        ),
        (
            "跑步习惯",
            "从今年春天开始每周跑三次五公里，配速从 8 分提高到 6 分半。",
        ),
        (
            "学吉他",
            "去年开始学吉他，已经会弹三首完整的曲子，最喜欢《晴天》。",
        ),
        (
            "搬家",
            "去年秋天搬到了离公司更近的小区，通勤时间从一小时缩短到二十分钟。",
        ),
        (
            "第一次马拉松",
            "上个月完成了人生第一个半程马拉松，用时 2 小时 15 分。",
        ),
        (
            "考驾照",
            "今年六月拿到了驾照，科目二补考了一次，科目三一次过。",
        ),
        (
            "近视手术",
            "前年做了近视手术，现在视力恢复到 1.0，彻底告别眼镜。",
        ),
        (
            "养多肉",
            "办公桌上养了一排多肉，最喜欢那棵熊童子，已经养了两年。",
        ),
        (
            "换手机",
            "今年换了新手机，主要是为了拍照，拍风景和猫都很满意。",
        ),
        ("学游泳", "去年夏天学会了蛙泳，现在每周去一次游泳馆。"),
    ];
    raw.iter()
        .map(|(title, summary)| {
            (
                format!("还记得「{}」这件事吗？", title),
                summary.to_string(),
                title.to_string(),
            )
        })
        .collect()
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use super::*;

    // ---- DeterministicRng ----

    #[test]
    fn rng_same_seed_same_sequence() {
        let mut a = DeterministicRng::new(42);
        let mut b = DeterministicRng::new(42);
        for _ in 0..100 {
            assert_eq!(a.next_u64(), b.next_u64(), "同 seed 序列必须一致");
        }
    }

    #[test]
    fn rng_different_seed_different_sequence() {
        let mut a = DeterministicRng::new(1);
        let mut b = DeterministicRng::new(2);
        let mut same = 0;
        for _ in 0..10 {
            if a.next_u64() == b.next_u64() {
                same += 1;
            }
        }
        assert!(same <= 1, "不同 seed 的序列应几乎完全不同（同次数={same}）");
    }

    #[test]
    fn rng_shuffle_is_permutation() {
        let mut rng = DeterministicRng::new(7);
        let mut items = vec![1, 2, 3, 4, 5];
        rng.shuffle(&mut items);
        let mut sorted = items.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, vec![1, 2, 3, 4, 5], "洗牌必须是排列（不增不减）");
    }

    // ---- sample_with_fallback ----

    #[test]
    fn sample_fallback_uses_fixture_when_no_candidates() {
        let (items, real) = sample_with_fallback::<i32>(&[], &[10, 20, 30], 2, 99);
        assert_eq!(items, vec![10, 20]);
        assert_eq!(real, 0, "无真实候选时 real=0");
    }

    #[test]
    fn sample_fallback_deterministic_same_seed() {
        let cands = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let (a, _) = sample_with_fallback(&cands, &[0], 4, 123);
        let (b, _) = sample_with_fallback(&cands, &[0], 4, 123);
        assert_eq!(a, b, "同 seed 抽样结果必须一致（可复跑）");
        assert_eq!(a.len(), 4);
    }

    #[test]
    fn sample_fallback_pads_with_fixture_when_short() {
        let cands = vec![1, 2];
        let fixture = vec![100, 200, 300];
        let (items, real) = sample_with_fallback(&cands, &fixture, 4, 5);
        assert_eq!(real, 2);
        assert_eq!(items.len(), 4, "不足部分必须用夹具补满");
        // 真实数据在前（洗牌后顺序不定，按集合比较）
        let mut head = items[..2].to_vec();
        head.sort_unstable();
        assert_eq!(head, vec![1, 2], "真实数据应排在前面");
        assert_eq!(&items[2..], &[100, 200], "夹具补齐排在真实数据之后");
    }

    // ---- 档位 ----

    #[test]
    fn default_variants_are_representative_pairs() {
        let variants = default_variants();
        assert_eq!(variants.len(), 4);
        // baseline 即 v3.1 初值
        let base = &variants[0];
        assert_eq!(base.id, "baseline");
        assert_eq!(base.theta_gap_minutes, 30);
        assert_eq!(base.max_msgs_per_block, 40);
        assert_eq!(base.retrieve_top_k, 3);
        // 每个档位只动一个参数（相对 baseline）
        for v in &variants[1..] {
            let changed = [
                v.theta_gap_minutes != 30,
                v.max_msgs_per_block != 40,
                v.retrieve_top_k != 3,
            ]
            .iter()
            .filter(|b| **b)
            .count();
            assert_eq!(changed, 1, "档位 {} 应只变化一个参数", v.id);
        }
    }

    // ---- fixture ----

    #[test]
    fn fixture_data_covers_default_scale() {
        assert!(fixture_tone_pairs().len() >= DEFAULT_QUESTIONS_PER_DIM);
        assert!(fixture_fact_events().len() >= DEFAULT_QUESTIONS_PER_DIM);
    }

    // ---- build_from_fixture ----

    #[test]
    fn build_from_fixture_shape() {
        let ds = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
        assert_eq!(ds.schema_version, DATASET_SCHEMA_VERSION);
        assert_eq!(ds.persona_uid, DEFAULT_PERSONA);
        assert_eq!(ds.source, "fixture");
        assert_eq!(ds.dimensions, vec!["tone", "fact"]);
        assert_eq!(ds.items.len(), DEFAULT_QUESTIONS_PER_DIM * 2);
        assert_eq!(ds.variants.len(), 4);
        // 全部来自夹具
        assert!(ds.items.iter().all(|i| i.source == "fixture"));
        // 每维恰好 qpd 题
        assert_eq!(
            ds.items.iter().filter(|i| i.dimension == "tone").count(),
            DEFAULT_QUESTIONS_PER_DIM
        );
        assert_eq!(
            ds.items.iter().filter(|i| i.dimension == "fact").count(),
            DEFAULT_QUESTIONS_PER_DIM
        );
        // 每题都有 reference 与 id
        for item in &ds.items {
            assert!(item.reference.is_some(), "{} 应有参考回答", item.id);
            assert!(item.id.starts_with("tone-") || item.id.starts_with("fact-"));
        }
        // seed 固定 → 复跑一致
        let again = build_from_fixture(DEFAULT_PERSONA, DEFAULT_QUESTIONS_PER_DIM, DEFAULT_SEED);
        let qs: Vec<&str> = ds.items.iter().map(|i| i.question.as_str()).collect();
        let qs2: Vec<&str> = again.items.iter().map(|i| i.question.as_str()).collect();
        assert_eq!(qs, qs2, "同 seed 复跑必须产生相同测试集");
    }

    // ---- 数据集序列化 roundtrip ----

    #[test]
    fn dataset_roundtrip_json() {
        let ds = build_from_fixture(DEFAULT_PERSONA, 3, 42);
        let json = serde_json::to_string(&ds).expect("序列化失败");
        let back: ProbeDataset = serde_json::from_str(&json).expect("反序列化失败");
        assert_eq!(back.items.len(), ds.items.len());
        assert_eq!(back.items[0].question, ds.items[0].question);
        assert_eq!(back.variants.len(), ds.variants.len());
    }

    // ---- 问题模板 ----

    #[test]
    fn fact_question_template() {
        let (q, _ref, title) = fixture_fact_events()[0].clone();
        assert!(q.contains(&title), "事实记忆问题应包含事件标题");
        assert!(q.contains("还记得"), "事实记忆问题应使用回忆问法");
    }

    // ---- filter_variants ----

    #[test]
    fn filter_variants_selects_and_ignores_unknown() {
        let variants = default_variants();
        let filtered = filter_variants(&variants, Some("baseline,top_k_1,nonexistent"));
        assert_eq!(filtered.len(), 2);
        assert_eq!(filtered[0].id, "baseline");
        assert_eq!(filtered[1].id, "top_k_1");
    }

    #[test]
    fn filter_variants_empty_falls_back_to_all() {
        let variants = default_variants();
        let filtered = filter_variants(&variants, Some("bad,bad2"));
        assert_eq!(filtered.len(), 4, "过滤为空时应回退全部档位");
    }

    // ---- 数据源文件解析 ----

    #[test]
    fn build_from_file_parses_source_json() {
        let tmp = std::env::temp_dir().join(format!("ramaria_probe_src_{}", std::process::id()));
        let path = tmp.join("source.json");
        std::fs::create_dir_all(&tmp).unwrap();
        std::fs::write(
            &path,
            r#"{
                "persona_uid": "char-0009",
                "messages": [
                    {"question": "今天好累", "reply": "早点休息"},
                    {"question": "周末去哪", "reply": "去公园"}
                ],
                "events": [
                    {"title": "学钢琴", "summary": "今年开始学钢琴，会弹《致爱丽丝》"}
                ]
            }"#,
        )
        .unwrap();

        let rt = tokio::runtime::Runtime::new().unwrap();
        let ds = rt
            .block_on(build_from_file(&path, DEFAULT_PERSONA, 3, 7))
            .expect("文件构建应成功");
        let _ = std::fs::remove_dir_all(&tmp);

        assert_eq!(ds.persona_uid, "char-0009", "文件中的 persona_uid 应优先");
        assert_eq!(ds.source, "file");
        assert_eq!(ds.items.len(), 6, "2 维 × 3 题");
        // 真实数据在前（2 条 tone + 1 条 fact）
        assert_eq!(ds.items.iter().filter(|i| i.source == "file").count(), 3);
        assert_eq!(ds.items.iter().filter(|i| i.source == "fixture").count(), 3);
    }

    #[test]
    fn build_from_file_missing_is_err() {
        let rt = tokio::runtime::Runtime::new().unwrap();
        let missing = std::env::temp_dir().join("ramaria_probe_nonexistent.json");
        let result = rt.block_on(build_from_file(&missing, DEFAULT_PERSONA, 3, 7));
        assert!(result.is_err(), "文件不存在应返回 Err（由上层夹具兜底）");
    }
}
