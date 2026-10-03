//! crates/ramaria-core/src/traits/store_crud.rs - Ramaria 存储后端业务 CRUD 抽象模块
//!
//! 设计特点:
//! - 承载会话/消息/L0-L1 记忆、画像、L2 事件与溯源、L3 性格与证据等业务对象
//! - 覆盖人格 facts/style/example/cluster 与原文话语块（utt）的主 CRUD 与查询
//! - 契约关键方法未覆写时显式返回 Unsupported，避免静默空结果
//! - 分页查询默认委托全量加载，SQL 后端可覆写为高效实现
//! - 具体实现位于 ramaria-storage，不泄露 sqlx 连接池或表结构

use std::collections::HashMap;

use async_trait::async_trait;
use uuid::Uuid;

use crate::error::RamariaResult;
use crate::keyword::{KeywordPoolRow, PendingAliasRow};
use crate::types::{
    ClusterSnapshot, EventBatchWrite, EventRelation, EventSource, MemoryEvent, MemoryL1, Message,
    MessageKey, Persona, PersonaEventAggregate, PersonaExample, PersonaFact, PersonaStyleStats,
    PersonalityTrait, ProfileField, Session, TraitEvidence, TraitStatus, UttBlock,
};

// =========================================================
// 存储后端抽象层
// =========================================================

/// 存储后端抽象 trait（业务对象 CRUD 分组）。
///
/// 职责:
/// - 承载会话/消息/L0-L1 记忆、画像（persona）、L2 事件与溯源、L3 性格与证据、
///   人格画像（facts/style/example/cluster）、原文话语块（utt）、行为规则与反馈日志等
///   业务对象的主 CRUD 与查询。
/// - 与 [`StoreInfrastructure`]（基础设施）分离，避免单一巨 trait 承载全部 27 张表能力。
///
/// 实现要求:
/// - 具体实现位于 `ramaria-storage`。
/// - 所有可恢复错误应转换为 `RamariaError::Storage` 或更精确分类。
///
/// ID 类型约定:
/// - TEXT 主键表（sessions/messages/memory_l1）使用 Uuid
/// - INTEGER AUTOINCREMENT 表使用 i64
/// - FK 列类型与目标表 PK 类型一致
#[async_trait]
pub trait StoreCrud: Send + Sync {
    // -- Session --
    /// 创建新 session，可选的 persona_uid 用于 Session-Persona 绑定。
    ///
    /// 参数:
    /// - `persona_uid`: 对话人格标识（None 兼容存量调用）。
    async fn create_session(&self, persona_uid: Option<&str>) -> RamariaResult<Session>;
    async fn close_session(&self, session_id: Uuid) -> RamariaResult<()>;
    async fn get_session(&self, session_id: Uuid) -> RamariaResult<Option<Session>>;
    async fn list_active_sessions(&self) -> RamariaResult<Vec<Session>>;
    async fn list_sessions(&self) -> RamariaResult<Vec<Session>>;
    async fn delete_session(&self, session_id: Uuid) -> RamariaResult<()>;

    /// 级联删除指定会话及其全部关联数据（消息 / utt 块 / L1 / 反馈日志等）。
    ///
    /// 职责:
    /// - 供一次性合成会话（如 probe run 每题自动创建的测试 session）用完即删
    ///   的场景使用：删除 session 前先按依赖顺序清理可能引用该 session 的
    ///   关联数据，避免外键约束导致删除失败或遗留孤儿数据。
    /// - 与 [`delete_session`] 的差异：本方法保证 session 及其关联数据整体移除，
    ///   不要求调用方先手动清理子表。
    ///
    /// 说明:
    /// - 仅做数据删除，**不触发**生命周期 / 封存 / 学习管线（调用方需在
    ///   App 层另行清理 lifecycle 对已删除 session 的活跃引用）。
    /// - 必须显式覆写：默认实现返回 `Unsupported`，由实现方明确声明
    ///   "是否支持级联删除"；不提供"悄悄退化为只删 session 行"的默认路径，
    ///   避免未覆写后端调用后残留孤儿数据却无任何错误可见。
    /// - `ramaria-storage` 覆写为事务内按依赖顺序显式删除各关联表。
    async fn delete_session_cascade(&self, _session_id: Uuid) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现会话级联删除（delete_session_cascade 需显式覆写）",
        ))
    }

    /// 回写绑定会话的 persona_uid（存量 NULL 会话归属修复）。
    ///
    /// 职责:
    /// - 会话创建时未绑定（`persona_uid=NULL`）的场景，在发送消息时
    ///   由 `resolve_session` 用前端传入的 persona_uid 回写 DB。
    /// - 幂等：重复绑定同一 uid 不产生副作用。
    ///
    /// 默认实现返回 `Unsupported` 错误（存量 mock 无需实现即可编译；
    /// 回写失败在调用方降级为 warn，不阻塞消息发送）。
    async fn bind_session_persona_uid(
        &self,
        _session_id: Uuid,
        _persona_uid: &str,
    ) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现 session persona_uid 回写绑定",
        ))
    }

    /// 创建带来源通道的 session（外部入口专用）。
    ///
    /// 职责:
    /// - 供外部入口（MCP / 未来社交通道）创建带 `channel` / `external_ref` 标识的会话：
    ///   桌面端可按来源区分会话，外部入口可按标识续写同一对话。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`：显式区分"未实现通道能力"与"创建成功"，
    ///   避免静默退化为无通道会话导致来源信息丢失。
    async fn create_session_in_channel(
        &self,
        _persona_uid: Option<&str>,
        _channel: &str,
        _external_ref: Option<&str>,
    ) -> RamariaResult<Session> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现带通道 session 创建（需覆写 create_session_in_channel）",
        ))
    }

    /// 按 `(channel, external_ref)` 查询活跃会话。
    ///
    /// 职责:
    /// - 外部入口续写定位：同一外部对话标识优先复用未关闭的会话。
    ///
    /// 返回:
    /// - `Ok(Some(session))`: 命中的活跃会话（若数据异常存在多条，返回最近开始的一条）。
    /// - `Ok(None)`: 无匹配活跃会话。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（错误可见，避免调用方把"未实现"误读为"无会话"而重复建会话）。
    async fn find_active_session_by_channel(
        &self,
        _channel: &str,
        _external_ref: Option<&str>,
    ) -> RamariaResult<Option<Session>> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现按通道查询活跃 session（需覆写 find_active_session_by_channel）",
        ))
    }

    /// 条件更新抢占式关闭 session（幂等封存入口）。
    ///
    /// 职责:
    /// - 多进程 / 多线程同时封存同一会话时，仅一个调用方"抢到"关闭权
    ///   （`UPDATE ... SET ended_at = ? WHERE id = ? AND ended_at IS NULL`）。
    /// - 抢到者继续生成 L1 摘要；未抢到者直接返回，避免重复摘要。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次调用完成了关闭（`ended_at` 由 NULL 变为当前时间）。
    /// - `Ok(false)`: 会话已关闭或不存在（未抢占到）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（幂等关闭语义由实现方显式声明，不提供静默退化路径）。
    async fn close_session_if_active(&self, _session_id: Uuid) -> RamariaResult<bool> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现条件关闭 session（需覆写 close_session_if_active）",
        ))
    }

    // -- Message (L0) --
    async fn save_message(&self, message: &Message) -> RamariaResult<()>;
    /// 全量加载指定 session 的全部消息。
    ///
    /// 说明:
    /// - **全量加载**，一次返回该 session 所有消息，供需要完整会话数据的
    ///   离线分析/重建路径（L1 摘要、utt 切分、导出等）使用。
    /// - 浏览/展示场景请使用 `list_messages_paginated`（分页），避免超长会话全量回内存。
    async fn list_messages(&self, session_id: Uuid) -> RamariaResult<Vec<Message>>;
    /// 全量加载指定 persona 的全部消息。
    ///
    /// 说明:
    /// - **全量加载**，一次返回该 persona 所有消息，供一次性离线分析/重建路径
    ///   （表达层风格统计、导入管线重建等）使用。
    /// - 浏览/展示场景请使用 `list_messages_by_persona_paginated`（分页），
    ///   避免大 persona 库全量回内存。
    async fn list_messages_by_persona(&self, persona_uid: &str) -> RamariaResult<Vec<Message>>;

    /// 按创建时间降序分页加载最近消息。
    ///
    /// 职责:
    /// - 替代 `list_messages` 全量加载，支持按 limit/offset 分页
    /// - 返回的消息按 `created_at DESC` 排序（最新在前），调用方按需反转
    ///
    /// 参数:
    /// - `session_id`: 会话 ID。
    /// - `limit`: 每页最大条数。
    /// - `offset`: 分页偏移量（第一页为 0）。
    ///
    /// 返回:
    /// - 按 `created_at DESC` 排序的消息列表。
    ///
    /// 默认实现:
    /// - 委托 `list_messages` 全量加载后手动排序截断（兼容存量实现）。
    /// - 子 crate（ramaria-storage）应覆写为高效 SQL（`ORDER BY created_at DESC LIMIT ? OFFSET ?`）。
    async fn list_messages_paginated(
        &self,
        session_id: Uuid,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        let mut all = self.list_messages(session_id).await?;
        all.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        let start = offset as usize;
        let end = (offset + limit).min(all.len() as i64) as usize;
        Ok(all
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect())
    }

    /// 按创建时间降序分页加载指定 persona 的消息（浏览场景专用）。
    ///
    /// 职责:
    /// - 替代 `list_messages_by_persona` 全量加载，支持按 limit/offset 分页。
    /// - 返回按 `created_at DESC`（最新在前）排序，调用方按需反转。
    ///
    /// 参数:
    /// - `persona_uid`: 目标 persona 的 UID。
    /// - `limit`: 每页最大条数。
    /// - `offset`: 分页偏移量（第一页为 0）。
    ///
    /// 返回:
    /// - 按 `created_at DESC` 排序的当前页消息列表。
    ///
    /// 默认实现:
    /// - 委托 `list_messages_by_persona` 全量加载后手动排序截断（兼容存量实现）。
    /// - 子 crate（ramaria-storage）应覆写为高效 SQL（`ORDER BY created_at DESC LIMIT ? OFFSET ?`）。
    async fn list_messages_by_persona_paginated(
        &self,
        persona_uid: &str,
        limit: i64,
        offset: i64,
    ) -> RamariaResult<Vec<Message>> {
        let mut all = self.list_messages_by_persona(persona_uid).await?;
        all.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        let start = offset as usize;
        let end = (offset + limit).min(all.len() as i64) as usize;
        Ok(all
            .into_iter()
            .skip(start)
            .take(end.saturating_sub(start))
            .collect())
    }

    /// 获取指定 session 最后一条消息的时间（Unix 毫秒）。
    ///
    /// 职责:
    /// - 供空闲检测线程判断 session 是否超过空闲阈值。
    ///
    /// 返回:
    /// - `Ok(Some(ms))`: 最后消息时间戳。
    /// - `Ok(None)`: session 无消息（仅已覆写实现可能返回）。
    /// - `Err(Unsupported)`: 未覆写——显式区分"未实现"与"确实无消息"，
    ///   调用方据此回退 `list_messages` 全量加载（`ramaria-service` 空闲检查）。
    async fn get_last_message_time(&self, _session_id: Uuid) -> RamariaResult<Option<i64>> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现最后消息时间查询（需覆写为 SELECT MAX(created_at)）",
        ))
    }

    /// 查询指定 persona 的最近对话时间（该 persona 会话中的最大消息时间）。
    ///
    /// 职责:
    /// - 供主动对话调度判断"距上次对话的时长"（最小空闲门禁）。
    ///
    /// 语义:
    /// - 会话归属以 `sessions.persona_uid` 为准（会话创建时绑定），取会话内消息
    ///   的 `MAX(created_at)`；用户消息（`messages.persona_uid IS NULL`）同样计入。
    /// - 会话存在但无消息时不计入；该 persona 无任何消息时返回 None。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    ///
    /// 返回:
    /// - `Ok(Some(ms))`: 最近一条消息的 Unix 毫秒时间戳。
    /// - `Ok(None)`: 无对话历史（含未覆写 mock —— 按"无历史"处理，不阻塞调度）。
    async fn last_message_time_by_persona(&self, _persona_uid: &str) -> RamariaResult<Option<i64>> {
        Ok(None)
    }

    /// 统计指定 session 的消息数量。
    ///
    /// 职责:
    /// - 供前端 session 列表展示每条 session 的真实消息数。
    /// - 默认实现通过 `list_messages` 的 len 计算，子 crate 应覆写为 `SELECT COUNT(*)`。
    ///
    /// 返回:
    /// - 消息数量（无消息时为 0）。
    async fn count_messages(&self, session_id: Uuid) -> RamariaResult<u32> {
        Ok(self.list_messages(session_id).await?.len() as u32)
    }

    /// 聚合各会话的消息数量（会话列表一次取回全部计数）。
    ///
    /// 职责:
    /// - 供会话浏览列表展示每条会话的真实消息数，替代逐会话调用
    ///   `count_messages` 的 N+1 查询。
    ///
    /// 返回:
    /// - 会话 ID → 消息条数的映射；**无消息的会话不出现在映射中**
    ///   （调用方对缺失项按 0 处理，与 SQL `GROUP BY` 的结果形态一致）。
    ///
    /// 默认实现:
    /// - 遍历 `list_sessions()` 逐会话 `count_messages`（兼容非 SQL 后端）；
    ///   `ramaria-storage` 覆写为单条 `GROUP BY` 聚合查询。
    async fn count_messages_by_session(&self) -> RamariaResult<HashMap<Uuid, u32>> {
        let sessions = self.list_sessions().await?;
        let mut counts = HashMap::with_capacity(sessions.len());
        for session in sessions {
            let count = self.count_messages(session.id).await?;
            if count > 0 {
                counts.insert(session.id, count);
            }
        }
        Ok(counts)
    }

    /// 按导入去重指纹查询消息（跨批次 / 跨会话去重）。
    ///
    /// 职责:
    /// - 外部入口（导入 / MCP 回流）重复提交同一批消息时，凭指纹判定"该消息是否已入库"，
    ///   避免重复落库与全局 UNIQUE 冲突。
    ///
    /// 返回:
    /// - `Ok(Some(message))`: 指纹已存在（调用方跳过该条）。
    /// - `Ok(None)`: 未命中（默认实现恒返回 None——未覆写的 mock 视为"库中无此指纹"）。
    async fn find_message_by_fingerprint(
        &self,
        _fingerprint: &str,
    ) -> RamariaResult<Option<Message>> {
        Ok(None)
    }

    /// 按来源通道 + 外部对话标识读取消息去重键（外部入口重复提交去重）。
    ///
    /// 职责:
    /// - 外部对话（MCP / 未来社交通道）按 `(channel, external_ref)` 取回其**全部**消息键
    ///   （角色 + 正文，时间升序）：同一对话可能跨多个会话（空闲封存后另起），
    ///   本查询跨会话取回，使去重范围覆盖整段对话。
    /// - 供回流用例做"重发前缀跳过 + 指纹序数计算"（重复提交安全）。
    ///
    /// 参数:
    /// - `channel`: 来源通道（如 `mcp`）。
    /// - `external_ref`: 外部对话标识；`None` 表示该通道下无标识的单流会话。
    ///
    /// 返回:
    /// - 按 `created_at ASC` 排列的消息键列表（默认实现返回空列表：未覆写的 mock 无历史）。
    ///
    /// 说明:
    /// - 只取两列（role / content）而非整行：长对话（数千条）也能廉价全量取回，
    ///   避免"读取窗口截断导致指纹序数失准"的重复写入风险；
    /// - 调用方保证 `content` 已 trim（与 `messages.content` 写入口径一致）。
    async fn list_message_keys_by_channel_ref(
        &self,
        _channel: &str,
        _external_ref: Option<&str>,
    ) -> RamariaResult<Vec<MessageKey>> {
        Ok(Vec::new())
    }

    // -- Memory L1 --
    async fn save_memory_l1(&self, memory: &MemoryL1) -> RamariaResult<()>;
    async fn list_memory_l1(&self, session_id: Uuid) -> RamariaResult<Vec<MemoryL1>>;
    async fn get_memory_l1(&self, id: Uuid) -> RamariaResult<Option<MemoryL1>>;
    async fn mark_l1_absorbed(&self, l1_ids: &[Uuid]) -> RamariaResult<()>;
    /// 删除指定 session 中 persona_uid 为 NULL 的 L1 摘要（仅清理导入残留）
    async fn delete_memory_l1_by_session(&self, session_id: Uuid) -> RamariaResult<usize> {
        let _ = session_id;
        Ok(0) // 默认空实现：存量 mock 无需修改即可编译
    }
    async fn list_unabsorbed_l1(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryL1>>;

    /// 查询未吸收的"无主"L1 摘要（`persona_uid IS NULL`）。
    ///
    /// 用途:
    /// - 导入产生的 L1 不绑定特定画像（避免记忆视图污染），
    ///   但重建检索索引时仍需加载，否则导入数据在对话中永不可检索。
    /// - 检索侧对 NULL persona 文档不做 persona 过滤（任何画像可命中），
    ///   因此加载无主 L1 与"按画像隔离"不冲突。
    ///
    /// 默认实现:
    /// - 返回空 Vec（存量 mock 无需修改即可编译）。
    async fn list_unabsorbed_l1_unbound(&self) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    /// 批量把"无主"L1（`persona_uid IS NULL` 且未吸收）归属到指定 persona。
    ///
    /// 背景:
    /// - 导入产生的 L1 固定 `persona_uid=NULL`（摘要不应被特定画像独占），
    ///   导致按 persona 的 L2 触发查询永远查不到这些 L1 → L2 事件恒为 0。
    /// - 本方法在 L2 触发时把候选无主 L1 归属到来源 session 的 persona，
    ///   打通 L1→L2→行为事件链路。
    ///
    /// 幂等:
    /// - 仅更新 `persona_uid IS NULL AND absorbed = 0` 的记录：
    ///   不覆盖既有归属，不触碰已吸收数据。
    /// - 重复调用对已归属记录无副作用（实际更新数为 0）。
    ///
    /// 返回:
    /// - 实际更新的条数（可能小于 `l1_ids.len()`：部分已归属/已吸收时）。
    ///
    /// 默认实现:
    /// - 返回 `Ok(0)`（存量 mock 无需修改即可编译）。
    async fn assign_l1_persona_uid(
        &self,
        _l1_ids: &[Uuid],
        _persona_uid: &str,
    ) -> RamariaResult<usize> {
        Ok(0)
    }

    /// 按创建时间降序获取指定 persona 的最近 N 条 L1 摘要。
    ///
    /// 职责:
    /// - 供跨 session 上下文注入：新 session 创建时自动加载最近对话摘要。
    /// - 不区分 absorbed 状态——即使已被 L2 吸收，近期摘要仍有叙事价值。
    ///
    /// 参数:
    /// - `persona_uid`: 人格标识。
    /// - `limit`: 最多返回条数（建议 3-5）。
    ///
    /// 返回:
    /// - 按 `created_at DESC` 排序的 MemoryL1 列表。
    ///
    /// 默认实现:
    /// - 返回空 Vec，子 crate 应覆写为高效 SQL（`ORDER BY created_at DESC LIMIT ?`）。
    async fn list_recent_l1_by_persona(
        &self,
        _persona_uid: &str,
        _limit: u32,
    ) -> RamariaResult<Vec<MemoryL1>> {
        Ok(Vec::new())
    }

    /// 更新 L1 摘要的最后访问时间（检索命中接线，激活 `[decay] recent_boost_*` 访问加成）。
    ///
    /// 背景:
    /// - 决策 D-V17-006 / 备忘 §二 7：`last_accessed_at` 之前只在写入时填充、永不再更新，
    ///   导致 `[decay] enable_access_boost` 访问加成特性静默失效。
    /// - 检索命中后由上层（`retrieve_memory` stage）调用，把命中 L1 的访问时间刷新到当前，
    ///   使近期被检索的记忆在衰减排序中保底（`recent_boost_floor`）。
    ///
    /// 参数:
    /// - `l1_ids`: 命中的 L1 摘要 id 列表。
    /// - `now_ms`: 访问时间戳（Unix 毫秒），由调用方统一取 `now_ms()` 保证一致。
    ///
    /// 返回:
    /// - `Ok(())`: 更新成功（空列表也视为成功）。
    ///
    /// 默认实现:
    /// - 空操作 `Ok(())`，存量 mock 无需修改即可编译。
    async fn touch_l1(&self, _l1_ids: &[Uuid], _now_ms: i64) -> RamariaResult<()> {
        Ok(())
    }

    // -- Personas (id: i64) --
    async fn create_persona(&self, persona: &Persona) -> RamariaResult<i64>;
    async fn get_persona_by_uid(&self, uid: &str) -> RamariaResult<Option<Persona>>;
    async fn list_personas(&self) -> RamariaResult<Vec<Persona>>;
    /// 更新 persona 的可变字段（name/avatar/config/description）。
    /// uid 为业务标识，不可变更。
    /// 所有可选字段：`None` 表示保持旧值不变，不设为 NULL。
    async fn update_persona(
        &self,
        uid: &str,
        name: &str,
        avatar: Option<&str>,
        config: Option<&str>,
        description: Option<&str>,
    ) -> RamariaResult<()>;

    // -- Memory Events (L2 事件层, id: i64) --
    async fn save_event(&self, event: &MemoryEvent) -> RamariaResult<i64>;
    /// 按 id 查询单条事件（证据链溯源用）。
    ///
    /// 默认实现返回 `Ok(None)`（存量 mock 无需实现即可编译）。
    async fn get_event(&self, _id: i64) -> RamariaResult<Option<MemoryEvent>> {
        Ok(None)
    }
    async fn list_events_by_persona(
        &self,
        persona_uid: &str,
        offset: i64,
        limit: i64,
    ) -> RamariaResult<Vec<MemoryEvent>>;
    async fn list_unabsorbed_events(&self, persona_uid: &str) -> RamariaResult<Vec<MemoryEvent>>;

    /// 统计指定 persona 的事件数量（事件浏览的分页总数）。
    ///
    /// 职责:
    /// - 供事件浏览在分页查询时取回分页前的总条数（调用方据此判断是否还有更早数据）。
    ///
    /// 返回:
    /// - 该 persona 的事件总数（无事件时为 0）。
    ///
    /// 默认实现:
    /// - 委托 `list_events_by_persona` 全量加载后取长度（兼容非 SQL 后端）；
    ///   `ramaria-storage` 覆写为 `SELECT COUNT(*)`。
    async fn count_events_by_persona(&self, persona_uid: &str) -> RamariaResult<u64> {
        Ok(self
            .list_events_by_persona(persona_uid, 0, i64::MAX)
            .await?
            .len() as u64)
    }

    /// 标记事件已被 L3 推断吸收。
    ///
    /// 参数:
    /// - `event_ids`: 要标记的事件 ID 列表。
    ///
    /// 说明:
    /// - 将 `memory_events.absorbed` 设为 1，使这些事件不再出现在 `list_unabsorbed_events` 中。
    /// - 幂等操作：已标记的事件重复调用无副作用。
    async fn mark_events_absorbed(&self, event_ids: &[i64]) -> RamariaResult<()>;

    /// 聚合除目标 persona 外各 persona 的事件级经验分布（跨用户冷启动先验的数据源）。
    ///
    /// 语义:
    /// - 返回"系统内其他已有人格画像"对 `memory_events` 的原始聚合行
    ///   （n / valence、share 事件级均值 / presentation 三态占比），
    ///   供 L3 分层收缩构造跨用户经验先验。
    /// - 存储层只做原始 SQL 聚合；样本量阈值过滤与跨 persona 加权合并
    ///   由调用方（ramaria-memory）按业务语义负责。
    ///
    /// 默认实现返回空列表——存量 mock / 未接入 SQL 聚合的后端无需改动即可编译；
    /// 调用方拿到空列表时回退当前 persona 内先验（与 v1.7 行为等价）。
    ///
    /// 参数:
    /// - `exclude_persona_uid`: 需排除的目标 persona（其自身事件不参与先验聚合）。
    async fn aggregate_persona_event_priors(
        &self,
        _exclude_persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaEventAggregate>> {
        Ok(Vec::new())
    }

    // -- Event Relations (from_id/to_id: i64) --
    async fn save_event_relation(&self, rel: &EventRelation) -> RamariaResult<i64>;

    /// 按 persona_uid 查询该角色相关的所有事件关系。
    ///
    /// 通过 JOIN memory_events 过滤，返回 from 事件属于该 persona 的关系。
    /// 默认返回空列表——不会破坏已有 mock 实现。
    async fn list_event_relations_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<EventRelation>> {
        Ok(Vec::new())
    }

    // -- Event Sources (event_id: i64, l1_id: Uuid) --
    async fn save_event_source(&self, event_id: i64, l1_id: Uuid, weight: f64)
    -> RamariaResult<()>;

    /// 单事务写入事件批次（事件 + 来源 + 关系 + L1 吸收标记）。
    ///
    /// 语义:
    /// - 全事务：任一步失败整体回滚，杜绝"事件半写入/证据链缺失"；
    /// - `EventBatchWrite` 内的下标越界返回 `Validation` 错误；
    /// - 默认实现返回 `Unsupported`（未覆写的 mock 调用时错误可见）。
    ///
    /// 返回:
    /// - 按 `batch.events` 顺序一一对应的事件数据库 id。
    async fn save_event_batch(&self, batch: &EventBatchWrite) -> RamariaResult<Vec<i64>> {
        let _ = batch;
        Err(crate::error::RamariaError::unsupported("save_event_batch"))
    }

    /// 查询指定事件的所有溯源 L1 记录。
    ///
    /// 职责:
    /// - 用于前端性格画像证据链展开：事件 → L1 摘要 → evidence_notes。
    /// - 返回该事件关联的全部 L1 source 记录（含 weight）。
    ///
    /// 默认实现返回空列表，子 crate 应覆写为 SQL 查询。
    async fn list_event_sources_by_event(&self, _event_id: i64) -> RamariaResult<Vec<EventSource>> {
        Ok(Vec::new())
    }

    /// 批量查询事件所属会话映射（`event_sources → memory_l1.session_id`）。
    ///
    /// 职责:
    /// - 事件跟进类主动对话需要定位"事件所属会话"作为投递落点：
    ///   经 `event_sources.l1_id → memory_l1.session_id` 反查。
    ///
    /// 口径:
    /// - 一个事件可能有多条溯源（跨会话）：取来源权重最高者的会话；
    ///   同权重取 `l1_id` 升序首个（确定性，脏数据下结果稳定）。
    ///
    /// 参数:
    /// - `event_ids`: 目标事件 id 列表（空列表直接返回空映射）。
    ///
    /// 返回:
    /// - event_id → session_id 的映射；无来源 / 来源会话解析失败的事件不出现在映射中
    ///   （含未覆写 mock —— 空映射表示"无映射可用"，调用方跳过事件跟进类选题）。
    async fn list_event_session_map(
        &self,
        _event_ids: &[i64],
    ) -> RamariaResult<HashMap<i64, Uuid>> {
        Ok(HashMap::new())
    }

    // -- Persona Facts (id: i64) --
    async fn save_fact(&self, fact: &PersonaFact) -> RamariaResult<i64>;
    /// 按 persona_uid 和字段分类查询事实。
    /// `field` 使用 `ProfileField` 枚举以确保类型安全，避免传入非法字段名。
    ///
    /// 语义: 返回该字段的**全部**版本（含 superseded/candidate），供版本链展示。
    async fn list_facts_by_persona(
        &self,
        persona_uid: &str,
        field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>>;
    /// 按 persona 查询**全部字段**的 **active** 事实
    /// （版本链中仅当前生效参与检索与注入；知识卡片分组/判定器检索使用）。
    async fn list_active_facts_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }
    /// 按 persona + field 查询 **active** 事实（同 field 召回/判重候选读取）。
    async fn list_active_facts_by_field(
        &self,
        _persona_uid: &str,
        _field: ProfileField,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }
    /// 按 persona 查询**全部**事实（含 superseded/candidate，CLI list 用）。
    async fn list_all_facts_by_persona(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }
    /// 按 id 查询单条事实（CLI show / 版本链跳转）。
    async fn get_fact_by_id(&self, _id: i64) -> RamariaResult<Option<PersonaFact>> {
        Ok(None)
    }
    /// 事务化版本链覆盖写入——旧事实置 superseded + 新事实写入（version_of 指向旧 id）。
    ///
    /// 说明:
    /// - 同一事务内完成"旧 superseded + 新 insert"，避免中间态（覆盖写原子化）。
    /// - `old` 为被覆盖事实，`f` 为新事实（id 应为 0，由存储层回填）。
    /// - 返回新事实 id。
    async fn save_fact_with_version(
        &self,
        _old: &PersonaFact,
        _f: &PersonaFact,
    ) -> RamariaResult<i64> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现事实版本链覆盖写",
        ))
    }
    /// 查询某事实的完整版本链（含自身，按 created_at 升序；链头最早在前）。
    async fn list_fact_versions(&self, _seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
        Ok(Vec::new())
    }
    /// 按 persona_uid 一次性统计所有字段的 fact 数量（GROUP BY）。
    ///
    /// 返回:
    /// - `Vec<(ProfileField, usize)>`：每个字段及其对应的 fact 数量。
    /// - 某字段无记录时结果为 0。
    ///
    /// 性能:
    /// - 单次 SQL GROUP BY 查询，替代原来的 N+1 循环查询。
    /// - 用于冷启动已有画像的 fact 计数。
    async fn count_all_facts_for_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<(ProfileField, usize)>> {
        // 默认实现：委托 list_facts_by_persona 逐字段查询（兼容非 SQL 后端）
        let fields = [
            ProfileField::BasicInfo,
            ProfileField::PersonalStatus,
            ProfileField::Interests,
            ProfileField::Social,
            ProfileField::History,
            ProfileField::RecentContext,
            ProfileField::SpeakingStyle,
        ];
        let mut result = Vec::with_capacity(fields.len());
        for &field in &fields {
            let count = self.list_facts_by_persona(persona_uid, field).await?.len();
            result.push((field, count));
        }
        Ok(result)
    }

    // -- Style Stats (persona_style_stats 表，表达层 A3) --
    /// 按 persona 单行 upsert 风格统计（五维参数 + 样本量 + 基线引用 + 规则文本）。
    ///
    /// 说明:
    /// - `persona_style_stats` 为增量表（只增不删，v1.7 非破坏变更）。
    /// - 默认实现返回 `Unsupported`（存量 mock 无需实现即可编译）。
    async fn upsert_style_stats(&self, _stats: &PersonaStyleStats) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现 persona_style_stats upsert",
        ))
    }
    /// 按 persona 查询风格统计（注入侧读取规则文本 / 状态判断）。
    ///
    /// 默认实现返回 `Ok(None)`（存量 mock 无需实现即可编译）。
    async fn get_style_stats(
        &self,
        _persona_uid: &str,
    ) -> RamariaResult<Option<PersonaStyleStats>> {
        Ok(None)
    }

    // -- Personality Traits (L3 性格层, id: i64) --
    async fn save_trait(&self, t: &PersonalityTrait) -> RamariaResult<i64>;
    async fn list_traits_by_persona(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<PersonalityTrait>>;
    /// `id` 为 personality_traits 表的 INTEGER 主键。
    async fn update_trait_confidence(
        &self,
        id: i64,
        confidence: f64,
        evidence: f64,
        consistency: f64,
    ) -> RamariaResult<()>;
    /// `id` 为 personality_traits 表的 INTEGER 主键。
    async fn update_trait_status(&self, id: i64, status: TraitStatus) -> RamariaResult<()>;

    // -- Trait Evidence (trait_id/event_id: i64) --
    async fn save_evidence(&self, e: &TraitEvidence) -> RamariaResult<i64>;
    /// `trait_id` 为 personality_traits 表的 INTEGER 主键。
    async fn list_evidence_by_trait(&self, trait_id: i64) -> RamariaResult<Vec<TraitEvidence>>;

    // -- Persona Examples (id: i64) --
    async fn save_example(&self, e: &PersonaExample) -> RamariaResult<i64>;
    async fn list_selected_examples(&self, persona_uid: &str)
    -> RamariaResult<Vec<PersonaExample>>;
    /// 查询 persona 的全部示例候选（不区分 selected，供评分轮换注入）。
    ///
    /// 默认实现：存量 mock 无需改动即可编译。
    async fn list_all_examples(&self, _persona_uid: &str) -> RamariaResult<Vec<PersonaExample>> {
        Ok(Vec::new())
    }
    /// 按 (partner, reply) 精确查重（examples 写侧幂等判定）。
    ///
    /// 默认实现：存量 mock 无需改动即可编译。
    async fn find_example_by_pair(
        &self,
        _persona_uid: &str,
        _partner: &str,
        _reply: &str,
    ) -> RamariaResult<Option<PersonaExample>> {
        Ok(None)
    }

    // -- Utt Blocks (原文话语块) --
    /// 插入一条 utt 话语块，返回自增 id。
    ///
    /// 默认实现返回 `Unsupported` 错误（存量 mock 无需实现即可编译）。
    async fn insert_utt_block(&self, _block: &UttBlock) -> RamariaResult<i64> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现 utt_blocks 写入",
        ))
    }
    /// 按 persona 查询全部话语块（原文按 persona 严格隔离）。
    async fn list_utt_blocks_by_persona(&self, _persona_uid: &str) -> RamariaResult<Vec<UttBlock>> {
        Ok(Vec::new())
    }
    /// 获取指定会话的最新话语块（桥接取上一会话尾部原文）。
    async fn get_latest_utt_block_by_session(
        &self,
        _session_id: Uuid,
    ) -> RamariaResult<Option<UttBlock>> {
        Ok(None)
    }
    /// 删除单个 utt 话语块（增量构建重切时移除过期尾块）。
    ///
    /// 默认实现返回 `Unsupported` 错误（存量 mock 无需实现即可编译）。
    async fn delete_utt_block(&self, _id: i64) -> RamariaResult<()> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现单块 utt_blocks 删除",
        ))
    }
    /// 删除指定会话的全部话语块，返回删除行数。
    async fn delete_utt_blocks_by_session(&self, _session_id: Uuid) -> RamariaResult<usize> {
        Ok(0)
    }

    // -- Persona Cluster Snapshots (id: i64) --
    async fn save_cluster_snapshot(&self, s: &ClusterSnapshot) -> RamariaResult<i64>;
    async fn get_current_snapshots(
        &self,
        persona_uid: &str,
        category: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>>;
    /// 查询该 persona 的所有历史快照（含非 current），仅返回有 semantic_label_embedding 的条目。
    /// 用于跨版本簇匹配。
    async fn get_all_snapshots_with_embeddings(
        &self,
        persona_uid: &str,
    ) -> RamariaResult<Vec<ClusterSnapshot>> {
        let _ = persona_uid;
        Ok(Vec::new()) // 默认空实现，保持向后兼容
    }

    // -- Keyword Pool --
    async fn upsert_keyword(&self, keyword: &str) -> RamariaResult<()>;
    async fn list_keywords(&self) -> RamariaResult<Vec<String>>;

    /// 幂等手工注入规范词（已存在保持现状：不递增 use_count、不改别名状态）。
    ///
    /// 参数:
    /// - `keyword`: 标准化后的规范词文本。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次新插入（use_count 从 0 起，表示未经自然出现累积）；
    /// - `Ok(false)`: 词条已存在（未做任何修改）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（写语义由实现方显式声明）；
    ///   `ramaria-storage` 覆写为主键冲突 DO NOTHING 的插入（并发幂等）。
    async fn seed_keyword_canonical(&self, _keyword: &str) -> RamariaResult<bool> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现关键词幂等注入",
        ))
    }

    /// 列出 keyword_pool 全部规范词文本（canonical_id IS NULL 的词条）。
    ///
    /// 说明: 生产词典装载（BM25 增强分词等）请优先用 `list_established_keywords`
    /// （canonical + 已确认 alias，与内存侧 `KeywordPool::established_terms` 同口径）；
    /// 本方法保留给仅需规范词集合的调用方。
    /// 默认实现返回空列表（存量 mock 无需实现即可编译）。
    async fn list_canonical_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }

    /// 列出 keyword_pool 已确认词表文本（canonical + 已确认 alias，排除 pending）。
    ///
    /// 用途: 与内存侧 `KeywordPool::established_terms` 同口径的词表装载
    /// （BM25 词典增强分词等）；口径映射见 storage 侧 repo 实现注释。
    /// 默认实现返回空列表（存量 mock 无需实现即可编译）。
    async fn list_established_keywords(&self) -> RamariaResult<Vec<String>> {
        Ok(Vec::new())
    }

    /// 列出 keyword_pool 全部词条行（含 rowid / 别名状态 / 规范词指向）。
    ///
    /// 用途: KeywordService 装载内存词典镜像（keyword_pool → KeywordPool 三态状态机）。
    /// 默认实现返回 `Unsupported`：显式区分"未接线"与"词表确实为空"，
    /// 调用方（镜像装载 / 风格词典增强）拿到错误后按降级处理并记日志。
    async fn list_keyword_pool_entries(&self) -> RamariaResult<Vec<KeywordPoolRow>> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现 keyword_pool 词条装载查询",
        ))
    }

    /// 列出 keyword_pool 中全部待确认别名冲突（`alias_status='pending'`）。
    ///
    /// 返回:
    /// - 待确认别名行列表（别名 rowid / 别名文本 / 指向的规范词 rowid 与文本 / 登记时间）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`：显式区分"未接线"与"确无待确认项"，避免调用方
    ///   把未实现误读为"没有待裁决冲突"；`ramaria-storage` 覆写为 join 规范词文本的查询。
    async fn list_pending_aliases(&self) -> RamariaResult<Vec<PendingAliasRow>> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现待确认别名查询",
        ))
    }

    /// 确认别名：把 `alias_status='pending'` 的词条迁移为 `'alias'`（合并到规范词）。
    ///
    /// 参数:
    /// - `alias_id`: 别名词条 rowid（来自 `list_pending_aliases` / 词条行视图）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次完成迁移（条件更新命中）。
    /// - `Ok(false)`: 未命中（词条不存在 / 非 pending / 缺规范词指向——状态已变化，未写库）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（写语义由实现方显式声明）；
    ///   `ramaria-storage` 覆写为条件 UPDATE（仅命中 pending 行）。
    async fn confirm_keyword_alias(&self, _alias_id: i64) -> RamariaResult<bool> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现别名确认迁移",
        ))
    }

    /// 驳回别名：把 `alias_status='pending'` 的词条晋升为独立规范词（清除规范词指向）。
    ///
    /// 参数:
    /// - `alias_id`: 别名词条 rowid（来自 `list_pending_aliases` / 词条行视图）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次完成迁移（条件更新命中）。
    /// - `Ok(false)`: 未命中（词条不存在 / 非 pending——状态已变化，未写库）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（写语义由实现方显式声明）；
    ///   `ramaria-storage` 覆写为条件 UPDATE（仅命中 pending 行）。
    async fn reject_keyword_alias(&self, _alias_id: i64) -> RamariaResult<bool> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现别名驳回迁移",
        ))
    }

    /// 幂等登记待确认别名（`alias_status='pending'`，指向建议合并的规范词）。
    ///
    /// 参数:
    /// - `alias`: 标准化后的别名文本。
    /// - `canonical_id`: 建议合并到的规范词 rowid。
    /// - `use_count`: 观测使用计数（≥1；仅首次插入写入）。
    ///
    /// 返回:
    /// - `Ok(true)`: 本次新插入；
    /// - `Ok(false)`: 词条已存在（任意状态，未做任何修改）。
    ///
    /// 默认实现:
    /// - 返回 `Unsupported`（写语义由实现方显式声明）；
    ///   `ramaria-storage` 覆写为主键冲突 DO NOTHING 的插入
    ///   （并发幂等；不把已建立状态改写为 pending）。
    async fn upsert_pending_alias(
        &self,
        _alias: &str,
        _canonical_id: i64,
        _use_count: u32,
    ) -> RamariaResult<bool> {
        Err(crate::error::RamariaError::unsupported(
            "StoreCrud 未实现待确认别名登记",
        ))
    }
}
