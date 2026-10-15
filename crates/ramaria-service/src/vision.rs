//! crates/ramaria-service/src/vision.rs - Ramaria 图片理解模块
//!
//! 设计特点:
//! - 理解任务全链路：扫描待处理附件 → 能力门禁 → md5 去重 → 定位读取（相对引用直连
//!   + md5 反查兜底）→ 多模态调用 → 描述清洗 → 回填；逐行独立处置，单行业务失败不中断整批
//! - 能力门禁顺序固定：声明（配置显式开关）→ 隐私确认 → 探测；探测结论按
//!   model_id + base_url 进程内缓存，同键不重复发送探测请求
//! - 降级纪律：声明关 / 探测失败保持 pending 待下次（不落库）；隐私未确认保持 pending
//!   待下次；定位失败与超大文件跳过；调用失败重试一次后标 failed
//! - md5 去重：同图已有完成描述直接批量回填，不重复调用模型
//! - 输入准备：线上 provider 按内容格式与单边像素上限归一化（越界长图缩放重编码）；
//!   本地 provider 原字节透传；不符合后端输入约束时按 skipped 降级（见 `prepare` 子模块）
//! - 下载成功即 best-effort 落盘客户端图片缓存（真图渲染恢复）；落盘失败不影响理解语义
//! - base64 编码自实现；日志不落图片内容与描述原文
//!
//! 模块划分:
//! - `batch`：批量与"继续处理"共用的逐行理解实现；
//! - `locate`：附件定位（多导出根直连 + md5 反查）；
//! - `prepare`：理解输入准备（格式归一化与像素上限）。

// 理解入口经 import 门面（`importer` feature）调用；未启用该 feature 时本模块
// 仅作为引擎探测缓存的类型宿主，不产生未使用代码告警
#![cfg_attr(not(feature = "importer"), allow(dead_code))]

use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::traits::{ChatRequest, LlmProvider};
use ramaria_core::types::AttachmentStatus;
use uuid::Uuid;

use crate::engine::Engine;
use crate::types::ChannelImageOutcome;

mod batch;
mod locate;
mod prepare;

pub(crate) use batch::{MAX_IMAGE_BYTES, understand_attachment_rows, understand_attachments};
pub(crate) use locate::AttachmentLocator;
pub(crate) use prepare::image_mime_from_bytes;

// =========================================================
// 常量
// =========================================================

/// 描述截断上限（按字符数）。
const MAX_DESCRIPTION_CHARS: usize = 200;

/// 理解任务的内置提示词（固定单轮，请求文本不经用户输入）。
const VISION_PROMPT: &str = "请用一两句话简要描述这张图片的内容，直接给出描述，不要额外说明。";

/// 探测请求的提示词。
const PROBE_PROMPT: &str = "请描述这张图片。";

/// 探测用 1x1 PNG 图片（data URI 的 base64 载荷）。
const PROBE_IMAGE_BASE64: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8BQDwAEhQGAhKmMIQAAAABJRU5ErkJggg==";

/// 理解请求的模板版本。
const VISION_TEMPLATE_VERSION: &str = "vision-v1";

/// 探测请求的模板版本。
const PROBE_TEMPLATE_VERSION: &str = "vision-probe-v1";

// =========================================================
// 公开类型
// =========================================================

/// 一轮图片理解的执行统计。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct VisionRunStat {
    /// 扫描到的待处理附件数。
    pub scanned: usize,
    /// 实际调用理解并成功回填的描述数。
    pub done: usize,
    /// 命中已有描述（md5 复用）直接回填的描述数。
    pub reused: usize,
    /// 调用失败（重试后仍失败 / 空描述）的描述数。
    pub failed: usize,
    /// 跳过数（定位失败 / 文件过大 / 门禁批量标记）。
    pub skipped: usize,
}

/// 图片理解能力探测缓存条目（按 model_id + base_url 键控）。
pub(crate) struct VisionProbeState {
    /// 缓存键：`{model_id}/{base_url}`。
    pub(crate) model_key: String,
    /// 探测结论（true = 可用）。
    pub(crate) capable: bool,
}

// =========================================================
// 理解任务
// =========================================================

/// 图片理解能力门禁（内部判定状态）。
enum CapabilityGate {
    /// 声明开启且探测可用。
    Ready,
    /// 配置显式声明当前模型不支持图片理解。
    DeclaredOff,
    /// 线上 provider 隐私确认未完成。
    PrivacyPending,
    /// 能力探测失败（模型不可用 / 不支持多模态）。
    ProbeFailed,
}

// =========================================================
// 单张已下载图片理解（社交通道现场路径）
// =========================================================

/// 理解一张已下载图片（字节直接输入；与批量路径同口径）。
///
/// 流程:
/// 1. 空字节 / 超限 → 标记 skipped；
/// 2. 能力门禁（声明 → 隐私 → 探测）；隐私未确认保持 pending 返回 Skipped；
/// 3. md5 去重：命中已有描述批量回填 → Reused；
/// 4. 魔数判定 MIME → 多模态调用（失败重试一次）→ 描述清洗 → 回填。
///
/// 参数:
/// - `engine`: 服务层引擎。
/// - `attachment_id`: 附件行 id（状态回填目标）。
/// - `md5`: 图片内容指纹（小写 hex；由下载侧计算）。
/// - `image_bytes`: 图片字节。
///
/// 返回:
/// - 单张理解结果（统计口径）；业务失败按标记口径返回，不中断调用方。
pub(crate) async fn understand_downloaded_image(
    engine: &Engine,
    attachment_id: i64,
    md5: &str,
    image_bytes: &[u8],
) -> RamariaResult<ChannelImageOutcome> {
    let storage = engine.storage_ref().clone();

    // ---- 1. 字节边界 ----
    if image_bytes.is_empty() || image_bytes.len() as u64 > MAX_IMAGE_BYTES {
        tracing::warn!(
            attachment_id,
            size = image_bytes.len(),
            "社交通道图片字节为空或超限，跳过"
        );
        storage
            .mark_attachment_status(attachment_id, AttachmentStatus::Skipped)
            .await?;
        return Ok(ChannelImageOutcome::Skipped);
    }

    // 下载成功即 best-effort 落盘客户端图片缓存（真图渲染依赖缓存命中；
    // 与读取侧缓存目录口径一致：数据库文件同级 `image_cache` 子目录）
    if let Some(cache_dir) = engine
        .db_path()
        .parent()
        .map(|parent| parent.join("image_cache"))
    {
        let md5_key = md5.trim();
        let md5_key = (!md5_key.is_empty()).then_some(md5_key);
        crate::attachment_cache::store(&cache_dir, attachment_id, md5_key, image_bytes);
    }

    // ---- 2. 能力门禁（顺序固定：声明 → 隐私 → 探测） ----
    match capability_gate(engine).await? {
        CapabilityGate::DeclaredOff | CapabilityGate::ProbeFailed => {
            // 行保持 pending，待能力可达后由后续路径处理
            tracing::info!(attachment_id, "图片理解能力不可达，社交通道图片保持待处理");
            return Ok(ChannelImageOutcome::Skipped);
        }
        CapabilityGate::PrivacyPending => {
            // 行保持 pending，待隐私确认后由后续路径处理
            tracing::info!(attachment_id, "隐私确认未完成，社交通道图片保持待处理");
            return Ok(ChannelImageOutcome::Skipped);
        }
        CapabilityGate::Ready => {}
    }

    // ---- 3. md5 去重：已有完成描述直接批量回填 ----
    let md5 = md5.trim().to_string();
    if !md5.is_empty() {
        if let Some((description, description_model)) =
            storage.find_attachment_description_by_md5(&md5).await?
        {
            storage
                .fill_attachment_done_by_md5(&md5, &description, &description_model)
                .await?;
            return Ok(ChannelImageOutcome::Reused);
        }
    }

    // ---- 4. 输入准备与多模态调用（失败重试一次，共两次尝试） ----
    let llm = engine.llm();
    let label = model_label(&llm);
    let prepared = match prepare::prepare_image(
        llm.config().provider.is_online(),
        image_bytes,
        prepare::image_mime_from_bytes(image_bytes),
    ) {
        Ok(prepared) => prepared,
        Err(reject) => {
            tracing::warn!(
                attachment_id,
                reason = reject.as_str(),
                "社交通道图片不符合线上理解后端输入约束，跳过"
            );
            storage
                .mark_attachment_status(attachment_id, AttachmentStatus::Skipped)
                .await?;
            return Ok(ChannelImageOutcome::Skipped);
        }
    };
    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: VISION_PROMPT.to_string(),
        temperature: 0.2,
        max_tokens: 256,
        request_id: Uuid::new_v4(),
        template_version: VISION_TEMPLATE_VERSION.to_string(),
        enable_thinking: None,
    };
    let images = vec![prepared.to_data_uri()];
    let reply = match llm.chat_vision(&request, &images).await {
        Ok(reply) => Ok(reply),
        Err(first_error) => {
            tracing::warn!(
                attachment_id,
                error = %first_error,
                "社交通道图片理解调用失败，重试一次"
            );
            llm.chat_vision(&request, &images).await
        }
    };
    let raw = match reply {
        Ok(raw) => raw,
        Err(error) => {
            tracing::warn!(attachment_id, error = %error, "社交通道图片理解调用仍然失败");
            storage
                .mark_attachment_status(attachment_id, AttachmentStatus::Failed)
                .await?;
            return Ok(ChannelImageOutcome::Failed);
        }
    };

    // ---- 5. 清洗与回填（空描述不落库） ----
    let description = sanitize_description(&raw);
    if description.is_empty() {
        tracing::warn!(attachment_id, "社交通道图片理解返回空描述，标记失败");
        storage
            .mark_attachment_status(attachment_id, AttachmentStatus::Failed)
            .await?;
        return Ok(ChannelImageOutcome::Failed);
    }
    if md5.is_empty() {
        storage
            .mark_attachment_done(attachment_id, &description, &label)
            .await?;
    } else {
        storage
            .fill_attachment_done_by_md5(&md5, &description, &label)
            .await?;
    }
    tracing::info!(attachment_id, "社交通道图片理解完成");
    Ok(ChannelImageOutcome::Done)
}

// =========================================================
// 能力门禁与探测
// =========================================================

/// 判定图片理解能力门禁（顺序固定：声明 → 隐私 → 探测）。
async fn capability_gate(engine: &Engine) -> RamariaResult<CapabilityGate> {
    if !engine.config().vision.model_supports_vision {
        return Ok(CapabilityGate::DeclaredOff);
    }
    // 隐私状态为 non_exhaustive：仅"已确认 / 无需确认"放行，其余（含未知状态）保守保持待处理
    if !engine.check_privacy().await?.is_confirmed() {
        return Ok(CapabilityGate::PrivacyPending);
    }
    if probe_once(engine).await {
        Ok(CapabilityGate::Ready)
    } else {
        Ok(CapabilityGate::ProbeFailed)
    }
}

/// 确认当前模型图片理解可用（结果按 model_id + base_url 进程内缓存）。
async fn probe_once(engine: &Engine) -> bool {
    let llm = engine.llm();
    let model_key = format!(
        "{}/{}",
        llm.config().capability.model_id,
        llm.config().capability.base_url
    );

    // 缓存查询（短临界区：锁内只读，不跨 await 持锁）
    {
        let guard = engine.vision_probe_slot().lock().await;
        if let Some(state) = guard.as_ref().filter(|state| state.model_key == model_key) {
            return state.capable;
        }
    }

    let request = ChatRequest {
        system_prompt: String::new(),
        memory_context: None,
        history: Vec::new(),
        user_message: PROBE_PROMPT.to_string(),
        temperature: 0.2,
        max_tokens: 8,
        request_id: Uuid::new_v4(),
        template_version: PROBE_TEMPLATE_VERSION.to_string(),
        enable_thinking: None,
    };
    let images = vec![format!("data:image/png;base64,{PROBE_IMAGE_BASE64}")];
    let capable = match llm.chat_vision(&request, &images).await {
        Ok(_) => true,
        Err(error) => {
            tracing::warn!(error = %error, "图片理解能力探测失败");
            false
        }
    };

    // 结果写回缓存（短临界区）
    {
        let mut guard = engine.vision_probe_slot().lock().await;
        *guard = Some(VisionProbeState { model_key, capable });
    }
    capable
}

// =========================================================
// 内部纯函数
// =========================================================

/// 产生描述的模型标识：优先模型 ID，缺失时用 provider 稳定标识。
fn model_label(llm: &Arc<dyn LlmProvider>) -> String {
    let config = llm.config();
    if config.capability.model_id.is_empty() {
        config.provider.as_str().to_string()
    } else {
        config.capability.model_id.clone()
    }
}

/// 清洗模型返回的描述文本。
///
/// 步骤:
/// - trim 首尾空白，循环剥离成对包裹的引号（`"` / `'` / `“”` / `「」`）；
/// - 换行与控制字符替换为空格；
/// - 折叠连续空白为单个空格；
/// - 按字符截断到 200 字符。
fn sanitize_description(raw: &str) -> String {
    let unquoted = strip_wrapping_quotes(raw);
    let mut spaced = String::with_capacity(unquoted.len());
    for ch in unquoted.chars() {
        if ch.is_control() {
            spaced.push(' ');
        } else {
            spaced.push(ch);
        }
    }
    let collapsed = spaced.split_whitespace().collect::<Vec<_>>().join(" ");
    collapsed.chars().take(MAX_DESCRIPTION_CHARS).collect()
}

/// 循环剥离成对包裹的引号（剥离后重新 trim；不处理嵌套不配对形态）。
fn strip_wrapping_quotes(text: &str) -> &str {
    let mut current = text.trim();
    loop {
        if current.chars().count() < 2 {
            return current;
        }
        let first = current.chars().next();
        let last = current.chars().next_back();
        let matched = matches!(
            (first, last),
            (Some('"'), Some('"'))
                | (Some('\''), Some('\''))
                | (Some('“'), Some('”'))
                | (Some('「'), Some('」'))
        );
        if !matched {
            return current;
        }
        let start = first.map(char::len_utf8).unwrap_or(0);
        let end = current.len() - last.map(char::len_utf8).unwrap_or(0);
        current = current[start..end].trim();
    }
}

/// 标准 base64 编码（RFC 4648 字母表 + '=' 补齐）。仅编码，无解码。
fn base64_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = u32::from(chunk[0]);
        let b1 = u32::from(chunk.get(1).copied().unwrap_or(0));
        let b2 = u32::from(chunk.get(2).copied().unwrap_or(0));
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
        out.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(triple & 0x3F) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests;
