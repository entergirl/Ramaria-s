//! crates/ramaria-service/src/import/l0/tools.rs - Ramaria 导入内部工具
//!
//! 设计特点:
//! - 画像名回读不阻塞导入：未命中或失败均回退调用方提供的展示名

use ramaria_core::privacy::mask_id;
use sqlx::SqlitePool;

// =========================================================
// 内部工具
// =========================================================

/// 回读 persona 在库内的实际注册名（不阻塞导入）。
///
/// 说明:
/// - persona 可能按 uid / ref_id 复用既有条目，上报名以库内值为准；
/// - `persona_uid` 为 None（导入侧过滤跳过）或回读失败时返回 `fallback`
///   （请求覆盖名 / 文件解析名），仅记 warn。
pub(crate) async fn resolve_persona_name(
    pool: &SqlitePool,
    persona_uid: Option<&str>,
    fallback: &str,
) -> String {
    let Some(uid) = persona_uid else {
        return fallback.to_string();
    };
    match ramaria_storage::repo::personas::get_by_uid(pool, uid).await {
        Ok(Some(persona)) => persona.name,
        Ok(None) => {
            tracing::warn!(
                persona_uid = %mask_id(uid),
                "回读 persona 名称未命中（回退请求覆盖名 / 文件解析名）"
            );
            fallback.to_string()
        }
        Err(error) => {
            tracing::warn!(
                persona_uid = %mask_id(uid),
                error = %error,
                "回读 persona 名称失败（回退请求覆盖名 / 文件解析名，不阻塞导入）"
            );
            fallback.to_string()
        }
    }
}
