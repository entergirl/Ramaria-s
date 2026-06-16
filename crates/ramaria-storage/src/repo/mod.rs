//! rust/crates/ramaria-storage/src/repo/mod.rs - Repository 模块入口
//!
//! 设计特点:
//! - 每个子模块负责一类实体的 SQL 操作和行映射
//! - 提供共享辅助宏和工具函数，减少各模块的重复代码
//! - 所有数据库错误统一转换为 RamariaError::Storage

pub mod backend_config;
pub mod background_jobs;
pub mod bm25_index;
pub mod cluster;
pub mod conflict_queue;
pub mod events;
pub mod examples;
pub mod facts;
pub mod graph;
pub mod keyword;
pub mod memory_l1;
pub mod messages;
pub mod pending_push;
pub mod personas;
pub mod privacy_consent;
pub mod schema_meta;
pub mod sessions;
pub mod settings;
pub mod traits;

use ramaria_core::error::RamariaError;
use ramaria_core::error::RamariaResult;

// =========================================================
// 共享宏: 枚举解析（DB TEXT → 枚举 + 非法值回退）
// =========================================================

/// 生成一个从数据库 TEXT 字段解析枚举值的函数。
///
/// 非法值时记录 `tracing::warn!` 并回退到指定默认变体。
///
/// 用法:
/// ```ignore
/// parse_enum_fallback!(
/// parse_layer, TraitLayer, TraitLayer::Base, "personality_traits", "layer",
/// "base" => Base,
/// "primary" => Primary,
/// "accent" => Accent,
/// );
/// ```
///
/// 参数顺序:
/// 1. 函数名
/// 2. 枚举类型
/// 3. 默认回退变体（非法值时的 fallback）
/// 4. 表名（仅用于日志）
/// 5. 列名（仅用于日志）
///
/// 6.. 映射: "db_value" => EnumVariant
#[macro_export]
macro_rules! parse_enum_fallback {
    ($fn_name:ident, $enum_ty:ty, $default:expr, $table:expr, $column:expr,
     $($str:expr => $variant:ident),+ $(,)?) => {
        fn $fn_name(s: &str) -> $enum_ty {
            match s {
                $($str => <$enum_ty>::$variant,)+
                other => {
                    tracing::warn!(%other, "{}.{} 值非法，回退为 {:?}", $table, $column, $default);
                    $default
                }
            }
        }
    };
}

// =========================================================
// 共享 trait: 为 sqlx::Result 提供统一错误映射
// =========================================================

/// 为 `sqlx::Result<T>` 提供便捷的错误映射到 `RamariaError::Storage`。
///
/// 替代全仓 71 处 `.map_err(|e| RamariaError::storage_with_source("...", e))` 样板代码。
///
/// 用法:
/// ```ignore
/// use crate::repo::StorageResultExt;
/// sqlx::query("...").execute(pool).await.storage_err("保存数据失败")?;
/// ```
pub trait StorageResultExt<T> {
    /// 将 `sqlx::Error` 映射为 `RamariaError::Storage`，携带中文上下文描述。
    fn storage_err(self, context: impl Into<String>) -> RamariaResult<T>;
}

impl<T> StorageResultExt<T> for Result<T, sqlx::Error> {
    fn storage_err(self, context: impl Into<String>) -> RamariaResult<T> {
        self.map_err(|e| RamariaError::storage_with_source(context, e))
    }
}

// =========================================================
// 共享 UUID 解析辅助函数
// =========================================================

/// 从数据库非空 String 列解析 Uuid，解析失败时记录 warn 并传播错误。
///
/// 替代各 repo 文件中重复的 `.inspect_err(|_| tracing::warn!(...))` 模式。
///
/// 返回:
/// - 成功时返回解析后的 Uuid。
/// - 失败时返回 `RamariaError::Validation`，携带原始值和上下文。
#[inline]
pub fn parse_uuid_required(raw: &str, table: &str, column: &str) -> RamariaResult<uuid::Uuid> {
    ramaria_core::types::uuid_from_db(raw).inspect_err(
        |_| tracing::warn!(raw_id = %raw, "{table}.{column} UUID 解析失败，数据可能已损坏"),
    )
}

/// 从数据库 Option<String> 列解析 Option<Uuid>，解析失败时记录 warn 并传播错误。
///
/// 替代各 repo 文件中重复的 `.as_deref.map(uuid_from_db).transpose.inspect_err(...)` 模式。
///
/// 返回:
/// - `Ok(None)`: 数据库列为 NULL。
/// - `Ok(Some(uuid))`: 解析成功。
/// - `Err`: 解析失败。
#[inline]
pub fn parse_uuid_optional(
    raw: &Option<String>,
    table: &str,
    column: &str,
) -> RamariaResult<Option<uuid::Uuid>> {
    raw.as_deref()
        .map(ramaria_core::types::uuid_from_db)
        .transpose()
        .inspect_err(|_| {
            tracing::warn!(
                raw_id = %raw.as_deref().unwrap_or("nil"),
                "{table}.{column} UUID 解析失败，数据可能已损坏"
            )
        })
}
