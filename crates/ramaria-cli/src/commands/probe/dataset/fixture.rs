//! crates/ramaria-cli/src/commands/probe/dataset/fixture.rs - 探针 内置夹具与三维候选收集
//!
//! 设计特点:
//! - 内置 tone / emotion / fact 夹具（无真实数据时兜底）
//! - tone / emotion 配对的上下文携带与情感线索筛选
//! - 确定性抽样与夹具补齐

use super::super::DeterministicRng;
use super::super::now_iso8601;
use super::super::types::ContextTurn;
use super::super::types::DATASET_SCHEMA_VERSION;
use super::super::types::DatasetItem;
use super::super::types::ItemRegister;
use super::super::types::ProbeDataset;
use super::build::{CONTEXT_TURNS, default_variants};
use anyhow::Context;
use ramaria_core::types::Message;
use ramaria_core::types::MessageRole;
use ramaria_service::Engine;
use std::sync::Arc;

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
            // 夹具题无语境依赖，不带上文
            context: Vec::new(),
            register: ItemRegister::Chat,
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
            context: Vec::new(),
            register: ItemRegister::Chat,
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
            context: Vec::new(),
            register: ItemRegister::Chat,
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

/// 收集语气模仿维度候选：persona 发言与其同会话前一条 user 消息配对。
///
/// 返回 `(question, reference, context)` 列表：
/// - `question` = 用户消息；
/// - `reference` = persona 原回复；
/// - `context` = 该 question 之前紧邻的上文（时间正序，最多 `CONTEXT_TURNS` 条，
///   不含 question 本身），供 run 时预置到本轮对话历史以恢复社交语境。
///
/// 查询失败按会话跳过（记 warn），不中断整体构建。
pub(crate) async fn collect_tone_pairs(
    engine: &Arc<Engine>,
    persona_uid: &str,
) -> Vec<(String, String, Vec<ContextTurn>)> {
    let sessions = match engine.storage().list_sessions().await {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(%e, "probe build 读取会话列表失败，语气模仿维度无候选");
            return Vec::new();
        }
    };

    let mut pairs = Vec::new();
    for session in &sessions {
        let messages = match engine.storage().list_messages(session.id).await {
            Ok(m) => m,
            Err(e) => {
                tracing::warn!(session_id = %session.id, %e, "probe build 读取会话消息失败，跳过该会话");
                continue;
            }
        };
        pairs.extend(tone_pairs_from_messages(&messages, persona_uid));
    }
    pairs
}

/// 从单个会话的消息流提取"user 消息 → 目标 persona 回复"配对及其上文（纯函数）。
///
/// 配对语义（与既有口径一致）:
/// - `user` 消息记为待配对 question；遇到 `persona_uid` 等于目标 persona 的
///   非 user 消息即配对，并消费该 question（下一条 user 消息到来前不重复配对）。
///
/// 上文语义:
/// - 取该 question 之前紧邻的窗口内容（时间正序，容量上限 `CONTEXT_TURNS`，
///   超限弹出最早一条），不含 question 本身、也不含本轮 persona 回复。
/// - 已消费的 question 与其 persona 回复继续计入窗口，供后续题项复用，
///   与真实会话历史形态一致。
/// - 角色映射: `User` → "user"，`Assistant` → "assistant"；`System`/`Tool` 等
///   非对话角色既不参与配对也不进窗口。
/// - 内容为库内原文（不剥离 `[名字] ` 说话人前缀）。
pub(crate) fn tone_pairs_from_messages(
    messages: &[Message],
    persona_uid: &str,
) -> Vec<(String, String, Vec<ContextTurn>)> {
    let mut pairs = Vec::new();
    // 已见过的消息窗口（时间正序，容量上限 CONTEXT_TURNS）
    let mut window: Vec<ContextTurn> = Vec::new();
    // 当前待配对 question 之前紧邻的窗口快照（配对时作为该题项的上文）
    let mut pending_context: Vec<ContextTurn> = Vec::new();
    let mut last_user: Option<String> = None;

    for m in messages {
        match m.role {
            MessageRole::User => {
                last_user = Some(m.content.clone());
                // 先取快照再入窗口：上文不含 question 本身
                pending_context = window.clone();
                push_context_turn(&mut window, "user", &m.content);
            }
            MessageRole::Assistant => {
                if m.persona_uid.as_deref() == Some(persona_uid)
                    && let Some(question) = last_user.take()
                {
                    pairs.push((question, m.content.clone(), pending_context.clone()));
                }
                push_context_turn(&mut window, "assistant", &m.content);
            }
            // System / Tool 及未来扩展角色：既不作配对回复，也不进上文窗口
            // （避免把系统提示等非对话内容当作历史对话轮次喂给模型）。
            _ => {}
        }
    }
    pairs
}

/// 把一条消息计入上文窗口（超出 `CONTEXT_TURNS` 时弹出最早一条）。
pub(crate) fn push_context_turn(window: &mut Vec<ContextTurn>, role: &str, content: &str) {
    if window.len() >= CONTEXT_TURNS {
        window.remove(0);
    }
    window.push(ContextTurn {
        role: role.to_string(),
        content: content.to_string(),
    });
}

/// 收集情感表达维度候选：用户消息含情感线索 → persona 原回复配对。
///
/// 返回 `(question, reference, source_ref, context)`：
/// - `question` = 情绪化用户消息（情感线索命中）；
/// - `reference` = persona 原回复（golden 参考，供人工/judge 校准）；
/// - `source_ref` = 溯源标识（当前 None，保留扩展位）；
/// - `context` = 该 question 之前紧邻的上文（同语气模仿维度口径）。
///
/// 数据来源: 复用语气模仿的"user → persona 回复"配对机制（`collect_tone_pairs`），
/// 再按用户消息的情感关键词（难过/生气/担心/开心等）筛出情绪化情境——
/// 情感维度评估"persona 面对情绪化用户消息时的回应恰当性"（rubric 0/0.5/1），
/// 而非事实召回。
pub(crate) async fn collect_emotion_pairs(
    engine: &Arc<Engine>,
    persona_uid: &str,
) -> Vec<(String, String, Option<String>, Vec<ContextTurn>)> {
    let pairs = collect_tone_pairs(engine, persona_uid).await;
    pairs
        .into_iter()
        .filter(|(q, _, _)| has_emotion_cue(q))
        .map(|(q, r, context)| (q, r, None, context))
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
pub(crate) const EMOTION_NEGATIVE_CUES: [&str; 24] = [
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
pub(crate) const EMOTION_POSITIVE_CUES: [&str; 10] = [
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
pub(crate) async fn collect_fact_events(
    engine: &Arc<Engine>,
    persona_uid: &str,
) -> Vec<(String, String, String)> {
    let events = match engine
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
///
/// 说明: `-` 直出 stdout（含库内原文，口径见模块头 CR-SEC-102 登记）。
pub(crate) fn write_dataset_file(out: &str, dataset: &ProbeDataset) -> anyhow::Result<()> {
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
pub(crate) fn print_dataset_summary(dataset: &ProbeDataset) {
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

/// 内置语气模仿夹具（(用户问题, persona 原回复) 配对）。
///
/// 内容说明: 示例角色 persona（char-0001）的典型回应风格——
/// 工作吐槽安抚、生活建议、情绪陪伴，覆盖「行为驱动」的目标情境。
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
