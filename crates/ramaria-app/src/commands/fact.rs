//! crates/ramaria-app/src/commands/fact.rs - 知识层事实用例
//!
//! 设计特点:
//! - 提供 fact list/show 只读查询用例（供 CLI 与 desktop 共用）
//! - 双端无 delete：仅 list / show / 版本链
//! - list: 按 persona（+可选 field）过滤，返回 active 事实
//! - show: 单条事实详情 + 完整版本链
//! - 严格按 persona_uid 隔离

use ramaria_core::error::RamariaResult;
use ramaria_core::types::{PersonaFact, ProfileField};
use std::sync::Arc;

use crate::App;

/// 查询 persona 的全部 active 事实（跨字段），可选 field 过滤。
pub async fn fact_list(
    app: &Arc<App>,
    persona_uid: &str,
    field: Option<ProfileField>,
) -> RamariaResult<Vec<PersonaFact>> {
    let storage = app.storage.as_ref();
    match field {
        Some(f) => storage.list_active_facts_by_field(persona_uid, f).await,
        None => storage.list_active_facts_by_persona(persona_uid).await,
    }
}

/// 查询单条事实（按 id；任意 status，供 show 与版本链）。
pub async fn fact_get(app: &Arc<App>, id: i64) -> RamariaResult<Option<PersonaFact>> {
    app.storage.get_fact_by_id(id).await
}

/// 查询单条事实的完整版本链（含自身，链头最早在前）。
pub async fn fact_versions(app: &Arc<App>, seed_id: i64) -> RamariaResult<Vec<PersonaFact>> {
    app.storage.list_fact_versions(seed_id).await
}

/// 查询 persona 的全部事实（含 superseded/candidate，供 CLI list 总数统计与版本链展开）。
pub async fn fact_list_all(app: &Arc<App>, persona_uid: &str) -> RamariaResult<Vec<PersonaFact>> {
    app.storage.list_all_facts_by_persona(persona_uid).await
}
