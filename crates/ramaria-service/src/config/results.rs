//! crates/ramaria-service/src/config/results.rs - Ramaria 配置双写用例结果类型
//!
//! 设计特点:
//! - 不一致明细（`MismatchEntry`）：一致性校验时以文件为准的单条记录（键 / 文件值 / DB 值）
//! - 加载结果（`SyncOutcome`）：生效配置、文件存在性、解析错误与同步明细
//! - 写入结果（`SyncWriteResult`）：统一写入口双侧结果（file_ok / db_ok / 失败明细）
//! - 纯数据无 I/O：失败降级与日志语义由用例层（`mod.rs`）实现
//! - 首启 / 损坏路径的"以 DB 为准"语义经 `SyncOutcome` 字段对外表达

use ramaria_core::config::RamariaConfig;

// =========================================================
// 结果类型
// =========================================================

/// 单条不一致记录（一致性校验时以文件为准）。
#[derive(Debug, Clone)]
pub struct MismatchEntry {
    /// 配置键（如 `utt.theta_gap_minutes`、`backend.provider`）
    pub key: String,
    /// 文件侧值（canonical）
    pub file_value: String,
    /// DB 侧值
    pub db_value: String,
}

/// 加载 + 一致性校验结果。
#[derive(Debug)]
pub struct SyncOutcome {
    /// 合并后的生效配置（正常路径以文件为准；首启/损坏路径以 DB 为准）
    pub config: RamariaConfig,
    /// 配置文件是否存在（false 表示本次自动生成了文件：DB 非空时为 merged，DB 空时为模板）
    pub file_existed: bool,
    /// 文件解析错误（解析失败时回退默认值，不阻塞）
    pub file_parse_errors: Vec<String>,
    /// 不一致项（已按文件为准写回 DB 侧）
    pub mismatches: Vec<MismatchEntry>,
    /// DB 侧写回失败（降级不阻塞，日志告警）
    pub db_write_failures: Vec<String>,
}

/// 统一写入口（`save_config`）的结果。
#[derive(Debug, Default)]
pub struct SyncWriteResult {
    /// config.toml 写入是否成功
    pub file_ok: bool,
    /// DB 侧（backend_config + settings）写入是否全部成功
    pub db_ok: bool,
    /// 失败明细（供 UI 提示与日志）
    pub failures: Vec<String>,
}

impl SyncWriteResult {
    /// 是否完全成功（文件与 DB 双侧均无失败）。
    pub fn is_ok(&self) -> bool {
        self.file_ok && self.db_ok
    }
}
