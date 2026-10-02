//! crates/ramaria-cli/src/commands/probe/dataset/build.rs - 探针 数据集构建（数据源 / 数据库 / 输出）
//!
//! 设计特点:
//! - 默认档位代表配对与消融数据集构建入口
//! - 从数据库或数据源文件构建，含抽样与目标 persona 选择
//! - 数据集写出与文本摘要输出

use super::super::now_iso8601;
use super::super::types::ContextTurn;
use super::super::types::DATASET_SCHEMA_VERSION;
use super::super::types::DEFAULT_PERSONA;
use super::super::types::DatasetItem;
use super::super::types::ItemRegister;
use super::super::types::ProbeDataset;
use super::super::types::ProbeVariant;
use super::super::types::VariantOverrides;
use super::super::types::ablation_variants;
use super::fixture::{
    build_from_fixture, collect_emotion_pairs, collect_fact_events, collect_tone_pairs,
    fixture_emotion_pairs, fixture_fact_events, fixture_tone_pairs, has_emotion_cue,
    print_dataset_summary, sample_with_fallback, write_dataset_file,
};
use ramaria_core::error::RamariaError;
use ramaria_core::types::PersonaKind;
use ramaria_service::Engine;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

/// 题项携带的上文轮数（取 question 之前紧邻的最近 N 条消息，时间正序）。
pub(crate) const CONTEXT_TURNS: usize = 6;

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
            overrides: VariantOverrides::default(),
        },
        ProbeVariant {
            id: "theta_gap_60".to_string(),
            description: "θ_gap 上调至 60 分钟（相对基准只动 θ_gap）".to_string(),
            theta_gap_minutes: 60,
            max_msgs_per_block: 80,
            retrieve_top_k: 3,
            ablation: None,
            overrides: VariantOverrides::default(),
        },
        ProbeVariant {
            id: "max_msgs_40".to_string(),
            description: "条数上限下调至 40（相对基准只动条数）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 40,
            retrieve_top_k: 3,
            ablation: None,
            overrides: VariantOverrides::default(),
        },
        ProbeVariant {
            id: "top_k_1".to_string(),
            description: "top_k 下调至 1（相对基准只动 top_k，更保守的原文注入）".to_string(),
            theta_gap_minutes: 10,
            max_msgs_per_block: 80,
            retrieve_top_k: 1,
            ablation: None,
            overrides: VariantOverrides::default(),
        },
    ]
}

// =========================================================
// probe build：构建测试集
// =========================================================

/// 构建测试集（可选追加消融档位；含 fixture 兜底降级）。
///
/// 数据来源优先级:
/// 1. `--source <file>`: 显式指定的数据源文件（JSON，含 messages/events）。
/// 2. 数据库: 从导入数据构建（tone 用 persona 发言配对、fact 用 L2 事件）。
/// 3. 内置夹具: 上述路径无真实数据或构建失败时兜底（静默降级 + warn）。
///
/// 参数:
/// - `source`: 显式数据源文件（None = 从数据库构建）。
/// - `ablation`: 为 true 时在默认 4 档之后追加 `ablation_variants()` 的 15 档消融
///   Profile（共 19 档）；消融只改注入层闸门、不改 utt 切分（与 `ablation_variants()`
///   口径一致）。
///
/// 返回:
/// - 恒成功：文件/数据库路径失败时自动降级为内置夹具（静默降级 + warn）。
pub async fn build_dataset_with_ablation(
    engine: &Arc<Engine>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<&Path>,
    ablation: bool,
) -> ProbeDataset {
    let qpd = questions_per_dim.max(1);
    let target = resolve_target_persona(engine, persona.as_deref()).await;

    // 按数据来源构建：文件 > 数据库 > fixture 兜底
    let mut ds = match source {
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
        None => match build_from_db(engine, &target, qpd, seed).await {
            Ok(ds) => ds,
            Err(e) => {
                tracing::warn!(%e, "probe build 数据库构建失败，降级为内置夹具数据");
                build_from_fixture(&target, qpd, seed)
            }
        },
    };

    // `--ablation`：默认档位之后追加 15 档消融 Profile（id 即 Profile 名）
    if ablation {
        ds.variants.extend(ablation_variants());
    }
    ds
}

/// 构建测试集（默认档位；等价于 `build_dataset_with_ablation(..., false)`）。
pub async fn build_dataset(
    engine: &Arc<Engine>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<&Path>,
) -> ProbeDataset {
    build_dataset_with_ablation(engine, persona, questions_per_dim, seed, source, false).await
}

/// 执行 `probe build`（构建 + 输出）。
// 参数为 `probe build` 的完整输入集合（含输出模式与消融开关），合并会降低可读性。
#[allow(clippy::too_many_arguments)]
pub async fn run_build(
    engine: &Arc<Engine>,
    persona: Option<String>,
    questions_per_dim: usize,
    seed: u64,
    source: Option<PathBuf>,
    output: Option<String>,
    json: bool,
    ablation: bool,
) -> anyhow::Result<()> {
    let dataset = build_dataset_with_ablation(
        engine,
        persona,
        questions_per_dim,
        seed,
        source.as_deref(),
        ablation,
    )
    .await;

    // 输出：--output 写数据集文件；--json 输出信封；文本模式打印摘要
    if let Some(out) = output.as_deref() {
        // `-` + --json：stdout 只出一行信封，原始数据集放 data.raw（避免两段 JSON）
        if out == "-" && json {
            let data = serde_json::json!({
                "file": "-",
                "persona_uid": dataset.persona_uid,
                "source": dataset.source,
                "items": dataset.items.len(),
                "variants": dataset.variants.len(),
                "raw": &dataset,
            });
            return crate::json::emit_ok(&data);
        }
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
pub(crate) async fn build_from_db(
    engine: &Arc<Engine>,
    persona_uid: &str,
    qpd: usize,
    seed: u64,
) -> anyhow::Result<ProbeDataset> {
    let tone_pairs = collect_tone_pairs(engine, persona_uid).await;
    let fact_items = collect_fact_events(engine, persona_uid).await;
    let emotion_cands = collect_emotion_pairs(engine, persona_uid).await;

    tracing::info!(
        %persona_uid,
        tone_candidates = tone_pairs.len(),
        fact_candidates = fact_items.len(),
        emotion_candidates = emotion_cands.len(),
        "probe build 从数据库收集候选"
    );

    // 高情感选题：真实候选不足时显式告警（每维分别提示），并提示可用 --source
    // 手动补充 JSON 数据源（替代纯人工造题，高情感选题口径）。
    for (dim, n) in [
        ("tone", tone_pairs.len()),
        ("fact", fact_items.len()),
        ("emotion", emotion_cands.len()),
    ] {
        if n < qpd {
            tracing::warn!(
                %persona_uid,
                dimension = dim,
                candidates = n,
                required = qpd,
                "probe build 维度真实候选不足，将用内置夹具补齐；\
                 如需真实数据，可导入高情感记录后重跑或用 --source 手动补 JSON"
            );
        }
    }

    // 确定性抽样 + 夹具补齐（每维恒有 qpd 题，档位实验规模稳定）
    let fixture_tone = fixture_tone_pairs();
    let fixture_fact = fixture_fact_events();
    let fixture_emotion = fixture_emotion_pairs();

    let (tone_items, tone_real) = sample_with_fallback(
        &tone_pairs,
        &fixture_tone
            .into_iter()
            .map(|(q, r)| (q, r, Vec::new()))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );
    let (fact_cands, fact_real) = sample_with_fallback(&fact_items, &fixture_fact, qpd, seed);
    let (emotion_cands, emotion_real) = sample_with_fallback(
        &emotion_cands,
        &fixture_emotion
            .into_iter()
            .map(|(q, r)| (q, r, None, Vec::new()))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference, context)) in tone_items.into_iter().enumerate() {
        let is_real = idx < tone_real;
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: None,
            context,
            register: ItemRegister::Chat,
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
            // 事实维为模板化问句，不依赖即时上文
            context: Vec::new(),
            register: ItemRegister::Chat,
        });
    }
    for (idx, (question, reference, src_ref, context)) in emotion_cands.into_iter().enumerate() {
        let is_real = idx < emotion_real;
        items.push(DatasetItem {
            id: format!("emotion-{:04}", idx + 1),
            dimension: "emotion".to_string(),
            question,
            reference: Some(reference),
            source: if is_real { "db" } else { "fixture" }.to_string(),
            source_ref: src_ref,
            context,
            register: ItemRegister::Chat,
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
///   "messages": [{
///     "question": "...",
///     "reply": "...",
///     "source_ref": "...",
///     "context": [{"role": "user", "content": "..."}]
///   }],
///   "events":   [{"title": "...", "summary": "..."}]
/// }
/// ```
///
/// 说明: `messages[].context` 为该 question 之前紧邻的上文（时间正序），
/// 可选；缺省为空即不加下文。
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
    let tone_pairs: Vec<(String, String, Option<String>, Vec<ContextTurn>)> = raw
        .messages
        .iter()
        .filter(|m| !m.question.trim().is_empty())
        .map(|m| {
            (
                m.question.clone(),
                m.reply.clone(),
                m.source_ref.clone(),
                m.context.clone(),
            )
        })
        .collect();
    let emotion_pairs: Vec<(String, String, Option<String>, Vec<ContextTurn>)> = tone_pairs
        .iter()
        .filter(|(q, _, _, _)| has_emotion_cue(q))
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
            .map(|(q, r)| (q, r, None, Vec::new()))
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
            .map(|(q, r)| (q, r, None, Vec::new()))
            .collect::<Vec<_>>(),
        qpd,
        seed,
    );

    let mut items = Vec::with_capacity(qpd * 3);
    for (idx, (question, reference, src_ref, context)) in tone_items.into_iter().enumerate() {
        items.push(DatasetItem {
            id: format!("tone-{:04}", idx + 1),
            dimension: "tone".to_string(),
            question,
            reference: Some(reference),
            source: if idx < tone_real { "file" } else { "fixture" }.to_string(),
            source_ref: src_ref,
            context,
            register: ItemRegister::Chat,
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
            // 事实维为模板化问句，不依赖即时上文
            context: Vec::new(),
            register: ItemRegister::Chat,
        });
    }
    for (idx, (question, reference, src_ref, context)) in emotion_cands.into_iter().enumerate() {
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
            context,
            register: ItemRegister::Chat,
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

/// 解析目标 persona：
/// 1. 显式指定 → 用之；
/// 2. 未指定 → 数据库第一个白名单内角色类 persona（char/anim/oc/hist）；
/// 3. 无匹配 → 默认 char-0001（夹具数据以此编写）。
///
/// 语义（不按发言量选择）:
/// - 白名单 kind 过滤（Char/Anim/Oc/Hist）天然排除"我方"（kind=user），
///   探针目标始终为"对方" persona；
/// - 多个对方 persona 时取列表第一个（稳定可复跑），不引入发言量排序。
pub(crate) async fn resolve_target_persona(engine: &Arc<Engine>, explicit: Option<&str>) -> String {
    match engine.storage().list_personas().await {
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

/// 数据源文件输入格式（probe build --source）。
#[derive(Debug, serde::Deserialize)]
pub(crate) struct ProbeSourceFile {
    persona_uid: Option<String>,
    #[serde(default)]
    messages: Vec<SourceMessage>,
    #[serde(default)]
    events: Vec<SourceEvent>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SourceMessage {
    question: String,
    #[serde(default)]
    reply: String,
    #[serde(default)]
    source_ref: Option<String>,
    /// 该 question 之前紧邻的上文（时间正序，可选）。
    #[serde(default)]
    context: Vec<ContextTurn>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) struct SourceEvent {
    title: String,
    #[serde(default)]
    summary: String,
}
