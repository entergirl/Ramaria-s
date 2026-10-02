//! crates/ramaria-cli/src/commands/probe/run/session.rs - 探针 单题执行与有效性自检
//!
//! 设计特点:
//! - 生效配置加载与按档位参数重建 utt 块
//! - 单题执行、隐私确认与失败清理
//! - run 有效性诊断与题型语域题面

use super::super::types::ContextTurn;
use super::super::types::DatasetItem;
use super::super::types::ItemRegister;
use super::super::types::ProbeDataset;
use super::super::types::ProbeMetrics;
use super::super::types::ProbeRunDiagnostics;
use super::super::types::ProbeRunItem;
use futures::StreamExt;
use ramaria_core::config::RamariaConfig;
use ramaria_core::lock::read_recover;
use ramaria_core::traits::ChatMessage;
use ramaria_core::types::MessageRole;
use ramaria_core::types::now_ms;
use ramaria_memory::retriever::SearchRequest;
use ramaria_memory::utt::builder::UttBuilder;
use ramaria_service::ChatStreamRequest;
use ramaria_service::Engine;
use ramaria_service::StreamEvent;
use std::sync::Arc;
use std::time::Instant;

/// 加载生效配置（config.toml + DB 双写合并，与 `blocks rebuild` 一致）。
/// 加载失败记 warn 并降级为引擎默认配置（不阻塞探针）。
pub(crate) async fn load_effective_config(engine: &Arc<Engine>) -> RamariaConfig {
    match engine.load_full_config().await {
        Ok(cfg) => cfg,
        Err(e) => {
            tracing::warn!(%e, "读取生效配置失败，档位基准使用引擎默认配置");
            engine.config().as_ref().clone()
        }
    }
}

/// 按档位参数重建 utt 块（与 `blocks rebuild --force` 语义一致）。
///
/// 说明:
/// - 清空全部 utt 块后按目标参数全量重切（增量语义不会按新参数重切旧块）。
/// - embedding 不可用时块照常入库（仅无向量，检索退化为关键词通道）。
/// - 失败返回 Err，由调用方记 warn 后继续（档位实验不中断）。
pub(crate) async fn rebuild_utt_for_config(
    engine: &Arc<Engine>,
    config: &RamariaConfig,
) -> anyhow::Result<()> {
    let sessions = engine.storage().list_sessions().await?;
    for session in &sessions {
        engine
            .storage()
            .delete_utt_blocks_by_session(session.id)
            .await?;
    }
    let builder = UttBuilder::from_config(&config.utt);
    let embedding = engine.embedding();
    let embedder: Option<&dyn ramaria_core::EmbeddingProvider> =
        embedding.as_ref().map(|arc| arc.as_ref());
    builder
        .rebuild_all(engine.storage().as_ref(), embedder)
        .await?;
    engine.rebuild_index().await?;
    Ok(())
}

/// 依据检索器文档数、自检查询命中数与 embedding 可用性判定本轮有效性与告警。
///
/// 判定:
/// - 检索器文档数为 0 → 无效（记忆/知识通道空转），告警；
/// - 文档数 > 0 但四通道命中合计为 0 → 视为有效但告警（检索可能不可用）；
/// - embedding 不可用 → 追加告警（向量通道缺失，事实维退化）。
pub(crate) fn run_validity(
    retriever_doc_count: usize,
    fused_hits: usize,
    embeddings_available: bool,
) -> (bool, Vec<String>) {
    let mut warnings = Vec::new();
    if retriever_doc_count == 0 {
        warnings.push("检索器文档数为 0：记忆/知识通道空转，本轮 RAG 指标无效".to_string());
    } else if fused_hits == 0 {
        warnings.push("自检查询四通道命中均为 0：检索可能不可用，请核对索引与开关".to_string());
    }
    if !embeddings_available {
        warnings.push("embedding 不可用：向量通道缺失，事实维退化为纯关键词".to_string());
    }
    (retriever_doc_count > 0, warnings)
}

/// 采集本轮检索器有效性自检元数据（文档数 + 自检查询各通道命中数）。
///
/// 说明:
/// - 静态计数在检索器/关键词服务的读锁内同步取值，不跨 `.await` 持锁。
/// - 自检查询取数据集前 5 题的问题文本，先锁外做 embedding，再分别经关键词镜像
///   自由文本查询与检索器融合检索统计各通道命中；命中为 0 且文档数 > 0 时给出告警。
/// - 检索器文档数为 0 直接判定本轮无效（RAG/知识通道空转）。
pub(crate) async fn collect_run_diagnostics(
    engine: &Arc<Engine>,
    dataset: &ProbeDataset,
    persona_uid: &str,
) -> ProbeRunDiagnostics {
    // 1. 静态计数（读锁内同步取值；索引尚未加载时按 0 计）
    let retriever = engine.retriever_slot();
    let (retriever_doc_count, utt_doc_count) = {
        let guard = read_recover(&retriever, "probe_run.retriever");
        match guard.as_ref() {
            Some(retriever) => (retriever.doc_count(), retriever.utt_doc_count()),
            None => (0, 0),
        }
    };
    let keyword_service = engine.keyword_mirror();
    let (keyword_doc_count, keyword_pool_len, composite, pool) = {
        let guard = read_recover(&keyword_service, "probe_run.keyword_service");
        (
            guard.doc_count(),
            guard.pool_len(),
            guard.composite_arc(),
            guard.pool_snapshot(),
        )
    };

    let embeddings_available = engine.is_embedding_available();
    let embedder = engine.embedding();

    // 2. 自检查询：数据集前 5 题
    let queries: Vec<String> = dataset
        .items
        .iter()
        .take(5)
        .map(|item| item.question.clone())
        .collect();
    let mut bm25_hits = 0usize;
    let mut vector_hits = 0usize;
    let mut graph_hits = 0usize;
    let mut keyword_hits = 0usize;
    let mut fused_hits = 0usize;
    for query in &queries {
        // 查询向量：embedding 可用时生成，不可用则跳过向量通道（锁外 await）
        let query_vec = match embedder.as_deref() {
            Some(provider) => provider.embed(query).await.ok().filter(|v| !v.is_empty()),
            None => None,
        };
        // 关键词镜像命中（锁外异步；词典增强 + 别名归一 + 三层降级）
        let kw_hits = ramaria_memory::keyword::service::query_text_labels(
            &composite,
            &pool,
            query,
            Some(persona_uid),
            embedder.as_deref(),
            5,
        )
        .await;
        keyword_hits += kw_hits.len();
        // 检索器融合检索（含关键词通道）；读锁内仅同步检索，不跨 await
        let results = {
            let guard = read_recover(&retriever, "probe_run.retriever");
            match guard.as_ref() {
                Some(retriever) => retriever.search_with_keyword_hits(
                    &SearchRequest {
                        query: query.clone(),
                        persona_uid: Some(persona_uid.to_string()),
                        top_k: 5,
                        filter_share: false,
                    },
                    query_vec.as_deref(),
                    Some(kw_hits),
                ),
                None => Vec::new(),
            }
        };
        fused_hits += results.len();
        bm25_hits += results.iter().filter(|r| r.bm25_score.is_some()).count();
        vector_hits += results.iter().filter(|r| r.vector_score.is_some()).count();
        graph_hits += results.iter().filter(|r| r.graph_score.is_some()).count();
    }

    // 3. 有效性判定与告警
    let (valid, warnings) = run_validity(retriever_doc_count, fused_hits, embeddings_available);
    if !valid {
        tracing::warn!(
            retriever_doc_count,
            utt_doc_count,
            "probe run 有效性自检未通过：检索器空载"
        );
    }

    ProbeRunDiagnostics {
        retriever_doc_count,
        utt_doc_count,
        keyword_doc_count,
        keyword_pool_len,
        embeddings_available,
        probe_queries: queries.len(),
        bm25_hits,
        vector_hits,
        graph_hits,
        keyword_hits,
        fused_hits,
        valid,
        warnings,
    }
}

/// 把数据集题项的上文转为管线历史消息（role 字符串 → `MessageRole`）。
///
/// 说明:
/// - `"user"` → `MessageRole::User`，其余（含 "assistant" 及未知取值）→ `MessageRole::Assistant`。
/// - 保持输入顺序（时间正序）；内容原样透传（含库内 `[名字] ` 说话人前缀）。
pub(crate) fn seed_history_from_context(context: &[ContextTurn]) -> Vec<ChatMessage> {
    context
        .iter()
        .map(|turn| ChatMessage {
            role: if turn.role == "user" {
                MessageRole::User
            } else {
                MessageRole::Assistant
            },
            content: turn.content.clone(),
        })
        .collect()
}

// =========================================================
// 题项语域切换（statement 陈述说明轨）
// =========================================================

/// 陈述说明体裁的题面引导（`statement` 题在原始题面后追加，`chat` 题不追加）。
///
/// 口径:
/// - 只做语域切换：把题项从社交聊天切换为陈述说明，不携带任何字数/篇幅要求
///   （不得出现"字 / 句 / 篇幅 / 长度"等表述），避免引导被理解为复述长度
///   约束而污染事实维评估；对应单测锁定该口径。
/// - `chat` 题不追加，题面逐字不变，保证既有社交语域结果可比。
pub(crate) const STATEMENT_REGISTER_LEAD: &str =
    "（本题请以陈述说明的方式回答，把你记得的、与该问题相关的事实都讲出来。）";

/// 题项实际送入对话管线的题面（按 `register` 切换语域）。
///
/// 说明:
/// - `Chat`（缺省）→ 原始题面逐字不变（社交聊天语域）。
/// - `Statement` → 原始题面 + 换行 + `STATEMENT_REGISTER_LEAD`（陈述说明语域）。
/// - 只生成本次要送入管线的题面；`ProbeRunItem.question` 仍记录原始题面，
///   评分侧参考兜底与情境判定依赖未追加引导的原题面。
pub(crate) fn effective_question(item: &DatasetItem) -> String {
    match item.register {
        ItemRegister::Chat => item.question.clone(),
        ItemRegister::Statement => format!("{}\n{}", item.question, STATEMENT_REGISTER_LEAD),
    }
}

/// 跑单题对话并收集输出与指标。
///
/// 降级策略:
/// - 前置编排失败（状态/隐私/索引装载）→ 记录 error，指标置零。
/// - 流内 Error 事件 → 记录 error，reply 保留已收到的部分。
///
/// 语境补全:
/// - 题项携带的 `context`（question 之前紧邻的上文）经 `seed_history_from_context`
///   预置到本轮历史，使碎片化用户消息不再被孤立发给模型；预置内容不落库、
///   不进生命周期、不触发学习（与既有探针 session 清理口径一致）。
///
/// 语域补全:
/// - `statement` 题经 `effective_question` 追加陈述说明引导后送入管线（社交基调
///   由调用方按题关闭）；`ProbeRunItem.question` 仍记录原始题面。
///
/// 残留清理:
/// - 每题以 `session_id=None` 由服务层新建测试 session；残留 session 会被桌面端
///   空闲检测误当真实对话关闭，触发 L1/风格统计/L2 学习，把模型自答的合成对话
///   当成 persona 真实社交记录。
/// - 本函数在题跑完（含流内错误路径）后按句柄会话删除本次新建的测试 session
///   （含消息），不进入生命周期、不触发学习。
pub(crate) async fn run_single_question(
    engine: &Arc<Engine>,
    config: &RamariaConfig,
    persona_uid: &str,
    item: &DatasetItem,
) -> ProbeRunItem {
    let start = Instant::now();
    let mut reply = String::new();
    let mut total_chars = 0usize;
    let mut error: Option<String> = None;

    // 预置上文（仅历史段，不落库）：隐私红线要求只记条数、不记内容
    let seed_history = seed_history_from_context(&item.context);
    tracing::debug!(
        item_id = %item.id,
        context_turns = seed_history.len(),
        "probe run 单题预置上文"
    );

    // 送入管线的题面按语域切换（statement 追加陈述引导）；`ProbeRunItem.question`
    // 仍记录原始题面（评分侧参考兜底 / 情境判定依赖原题面）。
    let question = effective_question(item);
    // 调用窗口起点：前置编排失败时用于定位本次可能新建的测试 session
    let call_started_ms = now_ms();
    let handle = match engine
        .chat_stream(ChatStreamRequest {
            message: question,
            persona: Some(persona_uid.to_string()),
            session_id: None,
            seed_history,
            config_override: Some(Arc::new(config.clone())),
        })
        .await
    {
        Ok(handle) => handle,
        Err(e) => {
            // 前置编排失败：会话可能已在创建后中断（召回前置的索引装载故障），
            // 按调用窗口尽力清理本次新建的测试 session。
            cleanup_probe_session_after_error(engine, persona_uid, call_started_ms).await;
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

    // 句柄携带本次流式生成的会话定位；流消费完成后按该 id 清理
    let session_id = handle.session_id;
    let mut stream = handle.events;
    while let Some(event_result) = stream.next().await {
        match event_result {
            Ok(event) => match event {
                StreamEvent::Delta { content, .. } => {
                    reply.push_str(&content);
                }
                StreamEvent::Done {
                    total_chars: tc, ..
                } => {
                    total_chars = tc;
                }
                StreamEvent::Error { error: e, .. } if error.is_none() => {
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

    // 流消费完毕（成功或流内错误）：删除本次新建的测试 session（含消息）。
    // 注意必须在流结束后执行——转发任务保存消息完成后才关闭通道，
    // 此时删除不会与后台保存产生竞态。
    if let Err(e) = engine.delete_session_cascade(session_id).await {
        tracing::warn!(
            %session_id,
            %e,
            "probe run 清理测试 session 失败（该 session 可能残留）"
        );
    }

    let reply_chars = reply.chars().count();
    let reply = if total_chars > 0 {
        ramaria_core::text::truncate_chars_bare(&reply, total_chars)
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

/// 前置编排失败后清理本次可能新建的测试会话（尽力而为）。
///
/// 说明:
/// - 生成用例在会话定位之后仍可能失败（召回前置的索引装载故障）：此时测试会话
///   已创建但调用方拿不到句柄，按"调用窗口内创建 + 本次人格 + 无消息"定位候选；
/// - 定位不到或删除失败仅记 warn（极端存储故障下的残余由负责人后续清理）；
///   有消息的会话绝不删除（不触碰真实对话）。
pub(crate) async fn cleanup_probe_session_after_error(
    engine: &Arc<Engine>,
    persona_uid: &str,
    call_started_ms: i64,
) {
    let sessions = match engine.storage().list_active_sessions().await {
        Ok(sessions) => sessions,
        Err(e) => {
            tracing::warn!(%e, "probe run 失败后读取活跃会话失败，跳过残留清理");
            return;
        }
    };
    for session in sessions {
        if session.started_at < call_started_ms {
            continue;
        }
        if session.persona_uid.as_deref() != Some(persona_uid) {
            continue;
        }
        let messages = match engine.storage().list_messages(session.id).await {
            Ok(messages) => messages,
            Err(_) => continue,
        };
        if !messages.is_empty() {
            continue;
        }
        if let Err(e) = engine.delete_session_cascade(session.id).await {
            tracing::warn!(
                session_id = %session.id,
                %e,
                "probe run 清理前置失败时创建的测试 session 失败（该 session 可能残留）"
            );
        } else {
            tracing::info!(
                session_id = %session.id,
                "probe run 已清理前置失败时创建的测试 session"
            );
        }
    }
}

/// 线上 provider 的隐私确认（服务层引擎路径）。
///
/// 说明:
/// - 本地 LM Studio 直接通过（不触发确认流程）；
/// - 线上 provider（DeepSeek/OpenAI）需要用户交互确认，`--yes` 自动确认；
/// - 非 TTY 且无 `--yes` 时确认直接失败不挂起。
pub(crate) async fn ensure_privacy_with_engine(
    engine: &Arc<Engine>,
    auto_yes: bool,
) -> ramaria_core::error::RamariaResult<()> {
    use ramaria_service::PrivacyStatus;

    let status = engine.check_privacy().await?;

    match status {
        PrivacyStatus::NotNeeded => {
            tracing::debug!("本地 provider，无需隐私确认");
            Ok(())
        }
        PrivacyStatus::Confirmed { .. } => {
            tracing::info!("隐私已确认，继续");
            Ok(())
        }
        PrivacyStatus::NeedsConfirmation {
            provider_name,
            base_url,
        } => {
            if auto_yes {
                tracing::warn!(
                    provider = %provider_name,
                    base_url = %base_url,
                    "--yes 自动确认隐私提醒"
                );
                eprintln!("\x1b[33m⚠ 隐私提醒: 消息将发送至 {provider_name} ({base_url})\x1b[0m");
                eprintln!("  使用 --yes 已自动确认。数据将离开本机。");
                engine.confirm_privacy(true).await?;
                return Ok(());
            }

            eprintln!();
            eprintln!("\x1b[33m══════════════════════════════════════════\x1b[0m");
            eprintln!("\x1b[33m  隐私提醒\x1b[0m");
            eprintln!("\x1b[33m══════════════════════════════════════════\x1b[0m");
            eprintln!();
            eprintln!("  你正在使用线上 AI 服务：");
            eprintln!("    服务商: {provider_name}");
            eprintln!("    地址  : {base_url}");
            eprintln!();
            eprintln!("  你的对话内容将发送至该服务商的服务器。");
            eprintln!("  请确认你已阅读并同意该服务商的隐私政策。");
            eprintln!();

            let confirmed = crate::ui::confirm("是否同意将数据发送至线上服务？", auto_yes)
                .map_err(|e| {
                    ramaria_core::error::RamariaError::validation(format!("隐私确认失败: {e}"))
                })?;

            if !confirmed {
                tracing::warn!(provider = %provider_name, "用户拒绝隐私确认");
                return Err(ramaria_core::error::RamariaError::validation(
                    "用户拒绝隐私确认。无法使用线上 AI 服务。请切换为本地 LM Studio 或重新确认。",
                ));
            }

            let persistent = crate::ui::confirm("是否记住此选择（下次不再询问）？", auto_yes)
                .map_err(|e| {
                    ramaria_core::error::RamariaError::validation(format!("隐私确认失败: {e}"))
                })?;
            engine.confirm_privacy(persistent).await?;

            crate::ui::success("隐私确认完成");
            Ok(())
        }
        _ => {
            // PrivacyStatus 为 #[non_exhaustive]，保守拒绝未知状态
            Err(ramaria_core::error::RamariaError::validation(
                "未知的隐私状态。请检查应用配置后重试。",
            ))
        }
    }
}
