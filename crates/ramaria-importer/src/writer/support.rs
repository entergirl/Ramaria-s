//! crates/ramaria-importer/src/writer/support.rs - 导入写入层的失败补偿与字段口径工具
//!
//! 设计特点:
//! - 补偿删除只记日志，不覆盖调用方的原始错误
//! - 空串统一视为缺失，落库为 NULL 身份列

/// 补偿删除本批新建的 session（写入失败回滚）。
///
/// 参数:
/// - `pool`: 数据库连接池。
/// - `session_id`: 需要删除的 session UUID。
///
/// 说明:
/// - 供 `write_l0` 在查重或批量写入失败时调用，保持"失败不留半成品会话"。
/// - 删除失败仅记录 error 日志（提示重跑导入可能重复创建该会话），不覆盖调用方的原始错误。
pub(super) async fn rollback_created_session(pool: &sqlx::SqlitePool, session_id: uuid::Uuid) {
    if let Err(del_err) = ramaria_storage::repo::sessions::delete(pool, session_id).await {
        tracing::error!(
            session_id = %session_id,
            error = %del_err,
            "补偿删除已创建 session 失败，重跑导入可能重复创建该会话"
        );
    }
}

/// 空串视为缺失，统一转为 None（身份列 NULL 口径）。
pub(super) fn non_empty_opt(value: &str) -> Option<String> {
    if value.is_empty() {
        None
    } else {
        Some(value.to_string())
    }
}
