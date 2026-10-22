//! crates/ramaria-service/src/engine/usecases_memory.rs - Ramaria 记忆域用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在各自的用例模块（recall / chat / ingest / seal / idle / session），
//!   本层只做入口挂载，保证在线管线与手动触发共用同一份实现
//! - 封存门禁：封存用例与空闲检查入口受封存许可约束（`false` = 只写不封存）
//! - 索引维护：懒加载与强制重建共用同一构建实现，失败保留旧索引并置告警位
//! - 手动触发：L2/L3 检查与 L1 重生成供宿主批量导入后补跑；内部失败只记日志不抛错
//! - 补扫：消费封存失败登记的任务，LLM / 存储恢复后自动补跑 L1 摘要

use std::path::{Path, PathBuf};
use std::sync::Arc;

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{MemoryL1, Session, StickerAsset, now_ms};
use uuid::Uuid;

use crate::proactive::{ProactiveDirective, ProactiveOutcome};
use crate::stream_event::ChatStreamHandle;
use crate::types::{
    ChannelImageOutcome, ChatSendOutcome, ChatSendRequest, ChatStreamRequest, HistoryRequest,
    HistoryResult, IngestOutcome, IngestRequest, LocalImageSendOutcome, LocalImageSendRequest,
    RecallRequest, RecallResult, SealOutcome,
};

use super::Engine;

// =========================================================
// 用例入口
// =========================================================

impl Engine {
    /// 召回用例：按对话片段与分层选择装配可直接使用的记忆上下文。
    ///
    /// 职责:
    /// - 检索（向量 / BM25 / 关键词镜像 / 图谱，与在线管线同一份实现）→ Persona-Aware 过滤
    ///   → 衰减重排 → 分层装配（行为 / 知识 / 表达 / 脉络 / 记忆 / 原文）→ 预算裁剪。
    /// - `query` 与 `messages` 均为空时进入概览模式（时间线返回最近记忆）。
    ///
    /// 返回:
    /// - 成功时返回 `context` / `items` / `stats`。
    /// - 目标人格不在策略白名单时返回 `Privacy` 错误（越权可见性拒绝）。
    pub async fn recall(&self, req: RecallRequest) -> RamariaResult<RecallResult> {
        crate::recall::run(self, req).await
    }

    /// 生成用例：以指定人格回复一条消息（记忆检索 + 五段式装配 + LLM）。
    ///
    /// 职责:
    /// - 与在线管线同源：记忆上下文走共用召回、系统 Prompt 走共用装配（含脉络 / 行为 /
    ///   知识 / 示例素材），生成后把用户消息与助手回复一并落库。
    ///
    /// 返回:
    /// - 成功时返回 `reply` / `session_id` / `chars`。
    pub async fn chat_send(&self, req: ChatSendRequest) -> RamariaResult<ChatSendOutcome> {
        crate::chat::run(self, req).await
    }

    /// 主动生成用例：为指定人格生成一条主动消息（非流式、assistant-only）。
    ///
    /// 说明:
    /// - 输入为选题指令（人格 / 目标会话 / 来源 / 锚点 / 角度 / 语气）；
    /// - 门禁不过（状态未就绪 / 隐私未确认 / 人格不可见 / 目标会话不可用）返回
    ///   `Ok(None)`（静默跳过，不落库不投递）；LLM 或存储失败返回错误；
    /// - 成功时仅写入 1 条 `is_proactive=true` 的 assistant 消息（来源线上）。
    pub(crate) async fn chat_proactive(
        &self,
        directive: ProactiveDirective,
    ) -> RamariaResult<Option<ProactiveOutcome>> {
        crate::chat::run_proactive(self, directive).await
    }

    /// 流式生成用例：以指定人格回复一条消息，返回增量事件流句柄（交互入口消费）。
    ///
    /// 职责:
    /// - 与非流式生成同源：参数校验 / 状态与隐私门禁 / 会话定位 / 历史窗口 / 记忆召回 /
    ///   Prompt 装配 / Token 预算为同一份实现；
    /// - 生成侧差异：用户消息先落库，增量按事件流转发，助手回复仅在无错且非空时落库；
    ///   流打不开时不落库并返回只含一个 Error 事件的流。
    ///
    /// 返回:
    /// - 成功时返回 `ChatStreamHandle`（会话定位 + 事件流）；前置编排失败返回对应错误。
    pub async fn chat_stream(
        self: &Arc<Self>,
        req: ChatStreamRequest,
    ) -> RamariaResult<ChatStreamHandle> {
        crate::chat::stream(self, req).await
    }

    /// 写入用例：把外部对话回流入库（进 L0），使内容在桌面可见并参与后续记忆加工。
    ///
    /// 职责:
    /// - 会话解析（显式标识 > 单流退化）→ 惰性封存体检 → 重发跳过 + 指纹去重落库
    ///   → 可选封存（`finalize`）。
    ///
    /// 返回:
    /// - 成功时返回 `session_id` / `written` / `deduplicated` / `finalized`。
    pub async fn ingest(&self, req: IngestRequest) -> RamariaResult<IngestOutcome> {
        crate::ingest::run(self, req).await
    }

    /// 封存用例：抢占式关闭会话并触发封存链路（L1 → 索引镜像 → utt → examples → 钩子）。
    ///
    /// 职责:
    /// - 条件更新抢占（`ended_at IS NULL`）；仅抢到者生成 L1，未抢到直接返回
    ///   （多进程 / 多线程同时封存时保证 L1 只生成一次）。
    ///
    /// 返回:
    /// - 成功时返回 `sealed` 与本次生成的 `l1_count`。
    pub async fn seal(&self, session_id: Uuid) -> RamariaResult<SealOutcome> {
        crate::seal::run(self, session_id).await
    }

    /// 空闲检查用例：遍历全库活跃会话，对超时者执行封存。
    ///
    /// 返回:
    /// - 成功时返回本次封存的会话数量（抢占失败者不计入）。
    pub async fn tick_idle(&self) -> RamariaResult<usize> {
        crate::idle::tick(self).await
    }

    /// 会话历史用例：按会话或人格读取消息历史（分页）。
    ///
    /// 返回:
    /// - 成功时返回 `messages`（页内时间正序）与分页前的 `total`。
    pub async fn history(&self, req: HistoryRequest) -> RamariaResult<HistoryResult> {
        crate::session::history(self, req).await
    }

    /// 会话归属查询：返回会话归属人格（供入口层做只读可见性校验）。
    ///
    /// 返回:
    /// - `Ok(Some(Some(uid)))`: 会话存在且归属该人格；
    /// - `Ok(Some(None))`: 会话存在但无归属（旧数据 / 未绑定）；
    /// - `Ok(None)`: 会话不存在；
    /// - `Err(..)`: 存储读取失败。
    ///
    /// 说明:
    /// - 入口层不直接感知 sessions 表结构；白名单校验只依赖本用例的返回值。
    pub async fn session_persona_uid(
        &self,
        session_id: Uuid,
    ) -> RamariaResult<Option<Option<String>>> {
        let storage = self.storage_ref();
        match storage.get_session(session_id).await? {
            Some(session) => Ok(Some(session.persona_uid)),
            None => Ok(None),
        }
    }

    /// 会话创建用例：新建空白会话（可绑定人格）。
    ///
    /// 返回:
    /// - 新会话核心记录；宿主自行映射为各自既有响应结构。
    pub async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session> {
        crate::session::create(self, persona_uid).await
    }

    /// 会话删除用例：仅删除会话行本身（关联数据由外键级联规则清理）。
    ///
    /// 说明:
    /// - 会话不存在时幂等成功（与存储层删除语义一致）。
    pub async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()> {
        crate::session::delete(self, session_id).await
    }

    /// 会话级联删除用例：事务内按依赖顺序清理全部关联数据后删除会话行。
    ///
    /// 说明:
    /// - 供一次性合成会话（如探针）用完即删的场景使用：不触发封存 / 学习管线；
    /// - 宿主若持有生命周期容器，需在删除后自行清理活跃指针与活跃时间缓存。
    pub async fn delete_session_cascade(&self, session_id: Uuid) -> RamariaResult<()> {
        crate::session::delete_cascade(self, session_id).await
    }

    /// 解析发送目标会话（会话预检与自动重建）。
    ///
    /// 语义:
    /// - 指定会话存在且未关闭 → 原样返回；
    /// - 指定会话不存在或已关闭 → 新建会话（绑定人格）并返回其 id；
    /// - 存储查询失败 → 保守返回原会话（由生成路径做最终校验）；
    /// - 未指定会话（`None`）→ 新建会话（绑定人格）并返回。
    ///
    /// 用途:
    /// - 交互入口在进入生成前调用，避免前端竞态窗口把已关闭 / 已删除的会话 id
    ///   传入后收到"会话已关闭"错误。
    pub async fn resolve_send_session(
        &self,
        persona_uid: Option<&str>,
        session_id: Option<Uuid>,
    ) -> RamariaResult<Uuid> {
        crate::session::resolve_send_session(self, persona_uid, session_id).await
    }
}

// =========================================================
// 检索索引用例
// =========================================================

impl Engine {
    /// 确保检索索引已加载（懒加载：首次召回前构建一次，重复调用为空操作）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次调用完成了构建。
    /// - `Ok(false)`: 索引此前已加载（或无需构建）。
    ///
    /// 说明:
    /// - 构建失败时置"重建失败"告警位并上抛错误（旧索引保持可用，
    ///   见 [`Engine::is_index_rebuild_failed`]）；
    /// - 构建完成后推进索引状态（Idle/Building → Ready；失败 → Degraded）。
    pub async fn ensure_index_loaded(&self) -> RamariaResult<bool> {
        crate::index::ensure_loaded(self).await
    }

    /// 强制全量重建内存检索索引（跳过懒加载早退与冷却窗口）。
    ///
    /// 职责:
    /// - 供宿主显式刷新（批量导入完成 / 设置变更 / 诊断修复等场景）调用；
    /// - 与懒加载路径共用同一构建实现，重建后立即生效
    ///   （不受 `[index].refresh_interval_seconds` 约束）。
    ///
    /// 返回:
    /// - `Ok(total)`: 重建完成，`total` 为 L1 + L2 文档总数（不含 utt 块）。
    /// - `Err(..)`: 构建失败——旧索引保持可用、告警位置位并上抛错误。
    pub async fn rebuild_index(&self) -> RamariaResult<usize> {
        crate::index::rebuild(self).await
    }
}

// =========================================================
// 记忆管线手动触发
// =========================================================

impl Engine {
    /// 手动触发 L2 事件提取检查（全 persona 扫描 + L3 级联）。
    ///
    /// 用法:
    /// - 宿主手动触发（批量导入后补检查等场景），与后台调度共用同一份实现；
    /// - 内部失败只记日志，不向调用方抛错。
    pub async fn trigger_l2_check(&self) {
        crate::lifecycle::l2_l3::check_all_personas(self).await;
    }

    /// 手动触发指定 persona 的 L3 性格推断检查。
    ///
    /// 用法:
    /// - 宿主手动触发（批量导入后补检查等场景），与后台调度共用同一份实现；
    /// - 未吸收事件达到阈值（或最早事件超龄）时执行推断，否则直接返回；
    /// - 内部失败只记日志，不向调用方抛错。
    pub async fn trigger_l3_check(&self, persona_uid: &str) {
        crate::lifecycle::l2_l3::check_l3_trigger(self, None, persona_uid).await;
    }
}

// =========================================================
// L1 摘要手动重生成与补扫
// =========================================================

impl Engine {
    /// 为指定会话重新生成单段 L1 摘要（手动重试，末尾触发 L2 检查）。
    ///
    /// 用法:
    /// - 供封存中 L1 生成失败后的手动补救；会话可已关闭，也可仍在活跃中；
    /// - 单段口径：即使开启渐进式配置也按单段生成（与封存路径口径不同）。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 生成成功（已写库并增量镜像，可立即召回）；
    /// - `Ok(None)`: 会话无消息（跳过）。
    pub async fn regenerate_l1(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 生成单段 L1 摘要但不触发 L2 级联（幂等；供批量导入场景）。
    ///
    /// 用法:
    /// - 与 [`Engine::regenerate_l1`] 相同，但跳过末尾 L2 检查；
    ///   调用方应在全部 L1 生成完成后自行触发级联；
    /// - 幂等：目标 persona 已有 L1 时不重复生成（返回 `Ok(None)`）。
    ///
    /// 返回:
    /// - `Ok(Some(l1))`: 本次生成成功；
    /// - `Ok(None)`: 会话无消息，或已有目标 persona 的 L1（跳过）。
    pub async fn regenerate_l1_no_cascade(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Option<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1_no_cascade(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 为指定会话重新生成 L1 摘要（渐进式感知口径）。
    ///
    /// 用法:
    /// - 与封存路径口径一致：`[l1.progressive]` 开启且会话触发阈值（消息数 / 时间跨度）时
    ///   按段生成多条 L1，未触发时回退单段摘要；末尾触发 L2 检查。
    ///
    /// 返回:
    /// - `Ok(l1_list)`: 本次生成的全部段 L1（未触发渐进时为 1 条）；
    /// - `Ok(vec![])`: 会话无消息。
    pub async fn regenerate_l1_progressive(
        &self,
        session_id: Uuid,
        persona_uid: Option<&str>,
        user_prefix: Option<&str>,
        assistant_prefix: Option<&str>,
    ) -> RamariaResult<Vec<MemoryL1>> {
        crate::lifecycle::l1::regenerate_l1_progressive(
            self,
            session_id,
            persona_uid,
            user_prefix,
            assistant_prefix,
        )
        .await
    }

    /// 补扫封存失败遗留的 L1 摘要任务（宿主启动与定时消费点）。
    ///
    /// 返回:
    /// - 本轮成功补跑出 L1 摘要的任务数。
    pub async fn retry_pending_l1_jobs(&self) -> usize {
        crate::lifecycle::l1::retry_pending_l1_jobs(self).await
    }
}

// =========================================================
// 本地图片 / 撤回 / 表情资产用例
// =========================================================

impl Engine {
    /// 本地会话发图：消息与附件行一并落库（仅本地会话）。
    ///
    /// 返回:
    /// - 落库消息与附件行定位、占位符与最终正文。
    pub async fn send_local_image(
        &self,
        req: LocalImageSendRequest,
    ) -> RamariaResult<LocalImageSendOutcome> {
        crate::local_image::send(self, req).await
    }

    /// 撤回一条本地消息（仅本地会话、本人消息、窗口内）。
    ///
    /// 返回:
    /// - 标记成功返回 `Ok(())`；校验失败为校验错误。
    pub async fn recall_local_message(&self, message_id: Uuid) -> RamariaResult<()> {
        crate::withdraw::recall_local_message(self, message_id, now_ms()).await
    }

    /// 读取生效的发图目录（默认目录首次使用时创建）。
    ///
    /// 返回:
    /// - 目录真实路径；库内自定义目录失效时回退默认目录。
    pub async fn image_source_dir(&self) -> RamariaResult<PathBuf> {
        crate::image_source::dir(self).await
    }

    /// 设置发图目录（`None` / 空白 = 恢复默认并删除设置键）。
    ///
    /// 返回:
    /// - 生效目录的真实路径；目录不合法时为校验错误。
    pub async fn set_image_source_dir(&self, path: Option<&str>) -> RamariaResult<PathBuf> {
        crate::image_source::set(self, path).await
    }

    /// 校验待发送图片位于发图目录内。
    ///
    /// 返回:
    /// - 规范化后的文件真实路径；目录外 / 非普通文件 / 链接时为校验错误。
    pub async fn validate_image_source_file(&self, path: &Path) -> RamariaResult<PathBuf> {
        let dir = crate::image_source::dir(self).await?;
        crate::image_source::validate_file(&dir, path)
    }

    /// 理解一张本地图片（字节输入；门禁 / 去重 / 回填与批量路径同口径）。
    ///
    /// 返回:
    /// - 单张理解结果（统计口径）。
    pub async fn local_understand_image(
        &self,
        attachment_id: i64,
        md5: &str,
        image_bytes: &[u8],
    ) -> RamariaResult<ChannelImageOutcome> {
        crate::vision::understand_downloaded_image(self, attachment_id, md5, image_bytes).await
    }

    /// 新增表情资产（写文件 + 落库）。
    ///
    /// 返回:
    /// - 落库后的资产行。
    pub async fn sticker_add(
        &self,
        file_name: &str,
        bytes: &[u8],
        description: Option<&str>,
        tags: &[String],
    ) -> RamariaResult<StickerAsset> {
        crate::sticker::add(self, file_name, bytes, description, tags).await
    }

    /// 列出全部表情资产（按 id 升序）。
    pub async fn sticker_list(&self) -> RamariaResult<Vec<StickerAsset>> {
        crate::sticker::list(self).await
    }

    /// 更新表情资产（描述 / 标签；None 保持现值，描述空串清空）。
    ///
    /// 返回:
    /// - 目标行存在且已更新为 true；不存在为 false。
    pub async fn sticker_update(
        &self,
        id: i64,
        description: Option<&str>,
        tags: Option<&[String]>,
    ) -> RamariaResult<bool> {
        crate::sticker::update(self, id, description, tags).await
    }

    /// 删除表情资产（含 best-effort 文件删除）。
    ///
    /// 返回:
    /// - 目标行存在且已删除为 true；不存在为 false。
    pub async fn sticker_remove(&self, id: i64) -> RamariaResult<bool> {
        crate::sticker::remove(self, id).await
    }
}
