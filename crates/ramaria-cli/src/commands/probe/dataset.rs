//! crates/ramaria-cli/src/commands/probe/dataset.rs - 探针测试集构建与内置夹具兜底
//!
//! 设计特点:
//! - `probe build` 数据集构建：来源优先级为数据源文件 > 数据库 > 内置夹具兜底（静默降级）。
//! - tone / emotion / fact 三维候选收集与确定性抽样（seed 固定可复跑），不足部分用夹具补齐。
//! - emotion 候选按用户消息情感线索（负面/正面触发词）筛选情境，复用语气模仿的配对机制。
//! - 内置夹具在无真实数据或构建失败时兜底，仅含问题与参考文本，不包含对话原文。
//! - 确定性伪随机（`DeterministicRng`）与时间戳（`now_iso8601`）复用根模块定义。

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::Context;
use ramaria_core::error::RamariaError;
use ramaria_core::types::{MessageRole, PersonaKind};

use super::types::{
    DATASET_SCHEMA_VERSION, DEFAULT_PERSONA, DatasetItem, ProbeDataset, ProbeVariant,
};
use super::{DeterministicRng, now_iso8601};

// =========================================================
// 默认档位（代表配对，各参数 2 档）
// =========================================================

/// 默认档位组合。
///
/// 设计:
/// - baseline 即当前对照基准（θ_gap=10 / 条数=80 / top_k=3）。
/// - 其余档位每次只动一个参数，便于归因单参数对输出质量的影响。
/// - 档位参数与 `[utt]` 配置组字段一一对应，直接覆盖 `UttConfig` 生效。
pub fn default_variants() -> Vec<ProbeVariant> {
    vec![
        ProbeVariant {
            id: "baseline".to_string(),
            description: "对照基准（θ_gap=10/条数=80/top_k=3）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "theta_gap_60".to_string(),
            description: "θ_gap 上调至 60 分钟（相对基准只动 θ_gap）".to_string(),
            theta_gap_minutes: 60,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "max_msgs_40".to_string(),
            description: "条数上限下调至 40（相对基准只动条数）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
            ablation: None,
        },
        ProbeVariant {
            id: "top_k_1".to_string(),
            description: "top_k 下调至 1（相对基准只动 top_k，更保守的原文注入）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 1,
            ablation: None,
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
pub async fn run_build(
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
    let emotion_cands = collect_emotion_pairs(app, persona_uid).await;

    tracing::info!(
        %persona_uid,
        tone_candidates = tone_pairs.len(),
        fact_candidates = fact_items.len(),
        emotion_candidates = emotion_cands.len(),
        "probe build 从数据库收集候选"
    );

    // 确定性抽样 + 夹具补齐（每维恒有 qpd 题，档位实验规模稳定）
    let fixture_tone = fixture_tone_pairs();
    let fixture_fact = fixture_fact_events();
    let fixture_emotion = fixture_emotion_pairs();

    let (tone_items, tone_real) = sample_with_fallback(&tone_pairs, &fixture_tone, qpd, seed);
    let (fact_cands, fact_real) = sample_with_fallback(&fact_items, &fixture_fact, qpd, seed);
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_cands,
        &fixture_emotion
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
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
    for (idx, (question, reference, src_ref)) in emotion_cands.into_iter().enumerate() {
        let is_real = idx < emotion_real;
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: src_ref,
        });
    }

    let any_real = tone_real > 0 || fact_real > 0 || emotion_real > 0;
    let source = if any_real { "db" } else { "fixture" };

    if !any_real {
        tracing::warn!(%persona_uid, "probe build 无真实数据，测试集全部使用内置夹具");
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
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
pub async fn build_from_file(
    path: &Path,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let text = std::fs::read_to_string(path).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "读取数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;
    let raw: ProbeSourceFile = serde_json::from_str(&text).map_err(|e| {
        anyhow::anyhow!(RamariaError::validation(format!(
            "解析数据源文件失败: {}: {e}",
            path.display()
        )))
    })?;

    let persona = raw.persona_uid.unwrap_or_else(|| persona_uid.to_string());

    // tone（全部 messages）与 emotion（仅含情感线索的 messages）同源筛选；
    // messages 需要非空 question 才能配对。
    let tone_pairs: Vec<(String, String, Option<String>)> = raw
        .messages
        .iter()
        .filter(|m| !m.question.trim().is_empty())
        .map(|m| (m.question.clone(), m.reply.clone(), m.source_ref.clone()))
        .collect();
    let emotion_pairs: Vec<(String, String, Option<String>)> = tone_pairs
        .iter()
        .filter(|(q, _, _)| has_emotion_cue(q))
        .cloned()
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
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_pairs,
        &fixture_emotion_pairs()
            .into_iter()
            .map(|(q, r)| (q, r, None))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
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
    for (idx, (question, reference, src_ref)) in emotion_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if idx < emotion_real {
                "file"
            } else {
                "fixture"
            }
            .to_string(),
            source_ref: src_ref,
        });
    }

    Ok(ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona,
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
        questions_per_dimension: qpd,
        source: "file".to_string(),
        generated_at: now_iso8601(),
        variants: default_variants(),
        items,
    })
}

/// 全部使用内置夹具构建测试集（兜底路径）。
pub fn build_from_fixture(persona_uid: &str, qpd: usize, seed: u64) -> ProbeDataset {
    let (tone_items, _) = sample_with_fallback(&[], &fixture_tone_pairs(), qpd, seed);
    let (fact_cands, _) = sample_with_fallback(&[], &fixture_fact_events(), qpd, seed);
    let (emotion_cands, _) = sample_with_fallback(&[], &fixture_emotion_pairs(), qpd, seed);

    let mut items = Vec::with_capacity(qpd * 3);
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
    for (idx, (question, reference)) in emotion_cands.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: "fixture".to_string(),
            source_ref: None,
        });
    }

    ProbeDataset {
        schema_version: DATASET_SCHEMA_VERSION,
        seed,
        persona_uid: persona_uid.to_string(),
        dimensions: vec![
            "tone".to_string(),
            "fact".to_string(),
            "emotion".to_string(),
        ],
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
///
/// 语义（不按发言量选择）:
/// - 白名单 kind 过滤（Char/Anim/Oc/Hist）天然排除"我方"（kind=user），
///   探针目标始终为"对方" persona；
/// - 多个对方 persona 时取列表第一个（稳定可复跑），不引入发言量排序。
async fn resolve_target_persona(app: &Arc<ramaria_app::App>, explicit: Option<&str>) -> String {
    match app.storage().list_personas().await {
        Ok(personas) => select_target_persona(&personas, explicit),
        Err(e) => {
            tracing::warn!(%e, "读取 persona 列表失败，使用默认 persona");
            DEFAULT_PERSONA.to_string()
        }
    }
}

/// 从 persona 列表中选择探针目标（纯函数，便于确定性测试）。
///
/// 优先级:
/// 1. 显式 `explicit` → 直接使用（不校验 kind，尊重用户指定）。
/// 2. 白名单 kind（Char/Anim/Oc/Hist）内第一个 persona —— 对方语义；
///    我方（kind=User）与助手（kind=Rama）不入选。
/// 3. 无匹配 → `DEFAULT_PERSONA`（char-0001，夹具数据以此编写）。
pub fn select_target_persona(
    personas: &[ramaria_core::types::Persona],
    explicit: Option<&str>,
) -> String {
    if let Some(uid) = explicit {
        return uid.to_string();
    }
    let whitelisted = [
        PersonaKind::Char,
        PersonaKind::Anim,
        PersonaKind::Oc,
        PersonaKind::Hist,
    ];
    for p in personas {
        if whitelisted.contains(&p.kind) {
            tracing::info!(persona_uid = %p.uid, "probe build 自动选择白名单 persona");
            return p.uid.clone();
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

/// 收集情感表达维度候选：用户消息含情感线索 → persona 原回复配对。
///
/// 返回 `(question, reference, source_ref)`：
/// - `question` = 情绪化用户消息（情感线索命中）；
/// - `reference` = persona 原回复（golden 参考，供人工/judge 校准）；
/// - `source_ref` = 溯源标识（当前 None，保留扩展位）。
///
/// 数据来源: 复用语气模仿的"user → persona 回复"配对机制（`collect_tone_pairs`），
/// 再按用户消息的情感关键词（难过/生气/担心/开心等）筛出情绪化情境——
/// 情感维度评估"persona 面对情绪化用户消息时的回应恰当性"（rubric 0/0.5/1），
/// 而非事实召回。
async fn collect_emotion_pairs(
    app: &Arc<ramaria_app::App>,
    persona_uid: &str,
) -> Vec<(String, String, Option<String>)> {
    let pairs = collect_tone_pairs(app, persona_uid).await;
    pairs
        .into_iter()
        .filter(|(q, _)| has_emotion_cue(q))
        .map(|(q, r)| (q, r, None))
        .collect()
}

/// 文本是否含情感线索（负面/正面情绪触发词）。
///
/// 情感维度候选筛选用：仅当用户消息带有明显情绪色彩时才属于
/// "需要情感回应"的情境。中性消息（普通询问/陈述）不入选。
pub fn has_emotion_cue(text: &str) -> bool {
    has_negative_cue(text) || has_positive_cue(text)
}

/// 文本是否含负面情感触发词（难过/生气/担心等）。
pub fn has_negative_cue(text: &str) -> bool {
    EMOTION_NEGATIVE_CUES.iter().any(|w| text.contains(w))
}

/// 文本是否含正面情感触发词（开心/高兴/成功等）。
pub fn has_positive_cue(text: &str) -> bool {
    EMOTION_POSITIVE_CUES.iter().any(|w| text.contains(w))
}

/// 负面情绪触发词（情境侧：用户消息）。
const EMOTION_NEGATIVE_CUES: [&str; 24] = [
    "难过",
    "伤心",
    "哭",
    "郁闷",
    "烦躁",
    "生气",
    "愤怒",
    "气死",
    "气得",
    "担心",
    "焦虑",
    "紧张",
    "害怕",
    "怕",
    "委屈",
    "失望",
    "崩溃",
    "累",
    "烦",
    "不开心",
    "痛苦",
    "压力",
    "孤独",
    "自责",
];

/// 正面情绪触发词（情境侧：用户消息）。
const EMOTION_POSITIVE_CUES: [&str; 10] = [
    "开心",
    "高兴",
    "太好了",
    "兴奋",
    "中奖",
    "升职",
    "成功了",
    "通过",
    "好消息",
    "惊喜",
];

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
pub fn sample_with_fallback<T: Clone>(
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

/// 文本模式打印数据集摘要（stdout 仅输出数据，提示走 stderr）。
fn print_dataset_summary(dataset: &ProbeDataset) {
    let dim_count = |dim: &str| dataset.items.iter().filter(|i| i.dimension == dim).count();
    let tone = dim_count("tone");
    let fact = dim_count("fact");
    let emotion = dim_count("emotion");
    let real = dataset
        .items
        .iter()
        .filter(|i| i.source != "fixture")
        .count();
    println!(
        "probe 测试集: persona={} | 维度=tone({})/fact({})/emotion({}) | 档位={} | 真实数据 {} 题 / 夹具 {} 题 | source={}",
        dataset.persona_uid,
        tone,
        fact,
        emotion,
        dataset.variants.len(),
        real,
        dataset.items.len() - real,
        dataset.source
    );
    println!("seed={}（相同 seed 可复跑相同测试集）", dataset.seed);
    for v in &dataset.variants {
        let ablation = v
            .ablation
            .as_deref()
            .map(|a| format!(" [消融:{a}]"))
            .unwrap_or_default();
        println!(
            "  档位 {:<14} θ_gap={:<3} 条数={:<3} top_k={}{}  — {}",
            v.id,
            v.theta_gap_minutes,
            v.max_msgs_per_block,
            v.retrieve_top_k,
            ablation,
            v.description
        );
    }
    crate::ui::info("运行 `ramaria probe run --dataset <文件>` 执行档位实验");
}

// =========================================================
// 内置测试夹具数据（构建失败时的兜底）
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
pub fn fixture_tone_pairs() -> Vec<(String, String)> {
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
pub fn fixture_fact_events() -> Vec<(String, String, String)> {
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

/// 内置情感表达夹具（(情绪化用户消息, persona 原回复)）。
///
/// 覆盖四类情境：难过/委屈（需安慰）、生气（需安抚）、担心/焦虑（需宽慰）、
/// 开心/成功（需分享喜悦）。每个 question 必须命中情感线索（`has_emotion_cue`），
/// 否则不会被 emotion 维收集/评估语义判定为情绪化情境。
pub fn fixture_emotion_pairs() -> Vec<(String, String)> {
    vec![
        (
            "今天上班被领导当着全组批评，太难过了".to_string(),
            "被当众批评确实不好受。先深呼吸，别急着自我否定——领导针对的是方案不是人，我们把问题一条条改掉就好。"
                .to_string(),
        ),
        (
            "和最好的朋友吵架了，很伤心".to_string(),
            "吵架不代表友谊结束。等彼此冷静下来，你可以先发条消息说声在乎这段关系，很多时候一个小台阶就能和好。"
                .to_string(),
        ),
        (
            "养了五年的猫生病住院，好担心".to_string(),
            "别太担心，现代兽医能处理很多问题。先配合医生把检查做完，你陪着它，它会更有安全感。".to_string(),
        ),
        (
            "项目上线前出 bug，被气得想摔电脑".to_string(),
            "先离开电脑五分钟，气头上做决定容易后悔。bug 总归能修，身体和气坏了不划算。".to_string(),
        ),
        (
            "准备了三个月的考试差两分没过，很崩溃".to_string(),
            "差两分说明你已经在门口了，这很可惜但也说明方向没错。休息两天，复盘下错题，下次一定行。".to_string(),
        ),
        (
            "我好像把事情搞砸了，很自责".to_string(),
            "别把一次失误放大成自我否定。搞砸了就一起想办法补救，你愿意承担已经很了不起了。".to_string(),
        ),
        (
            "深夜一个人待着，觉得很孤独".to_string(),
            "夜深人静的时候情绪容易放大。你不是一个人，随时可以来找我聊聊，明天太阳照常升起。".to_string(),
        ),
        (
            "最近工作压力好大，有点撑不住".to_string(),
            "压力大的时候先允许自己歇一歇。把任务拆小，一天只做好一件，你会发现没有想象中那么难。".to_string(),
        ),
        (
            "我升职了！太开心了".to_string(),
            "恭喜！这是你应得的。记得好好庆祝一下，也别忘了感谢一路支持你的人。".to_string(),
        ),
        (
            "终于跑完人生第一个半马，太兴奋了".to_string(),
            "太棒了！半马可不是随便谁都能完成的，为自己骄傲一下，好好犒劳自己。".to_string(),
        ),
        (
            "收到理想公司的 offer 了，好高兴".to_string(),
            "真替你高兴！这是实力加运气的证明。入职前好好放松几天，新旅程会很好的。".to_string(),
        ),
        (
            "我种的向日葵开花了，很开心".to_string(),
            "亲手养大的花开出来最有成就感了。拍张照留个纪念，这份喜悦值得好好记住。".to_string(),
        ),
    ]
}
