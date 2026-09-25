//! crates/ramaria-service/tests/parity/support/golden.rs - 对照基线（golden）冻结与比对
//!
//! 设计特点:
//! - 基线即证据：把规范化快照冻结为 `tests/parity/golden/<场景名>.json`，作为回归基准
//! - 缺失即生成：基线文件不存在时写入本次快照并给出 warn 提示（首次冻结需人工审阅提交）
//! - 不等即失败：与基线不一致时 panic 并输出逐路径差异报告（沿用 `snapshot::diff_report`）
//! - 显式更新：确认行为变更属预期时，设置 `PARITY_UPDATE_GOLDEN=1` 重跑覆盖基线；
//!   否则任何差异都必须先解释清楚
//! - 无网络无外部依赖：只做文件读写，目录随写入自动创建

use std::path::{Path, PathBuf};

use serde_json::Value;

use super::error::{ParityError, ParityResult};
use super::snapshot::{Snapshot, canon, diff_report};

/// 基线文件名扩展名。
const GOLDEN_EXTENSION: &str = "json";

/// 基线冻结结果。
///
/// 字段约定:
/// - `Matched`: 本次快照与已冻结基线一致；
/// - `Recorded`: 基线此前不存在，本次首次生成；
/// - `Updated`: 基线不一致且处于更新模式，本次已覆盖。
#[derive(Debug)]
pub enum GoldenOutcome {
    /// 与基线一致（携带基线文件路径）。
    Matched(PathBuf),
    /// 首次生成基线（携带写入路径）。
    Recorded(PathBuf),
    /// 更新模式下覆盖基线（携带写入路径）。
    Updated(PathBuf),
}

impl GoldenOutcome {
    /// 本次是否为"更新模式覆盖"（用于断言基线未被意外改写）。
    pub fn is_updated(&self) -> bool {
        matches!(self, Self::Updated(_))
    }

    /// 涉及的基线文件路径（命中 / 生成 / 覆盖三种结果均携带）。
    pub fn path(&self) -> &Path {
        match self {
            Self::Matched(path) | Self::Recorded(path) | Self::Updated(path) => path,
        }
    }
}

/// 基线仓库：定位并读写 `tests/parity/golden/` 下的场景基线。
pub struct GoldenStore {
    root: PathBuf,
}

impl GoldenStore {
    /// 构造基线仓库（根目录指向 crate 内 `tests/parity/golden/`）。
    pub fn new() -> ParityResult<Self> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("parity")
            .join("golden");
        Ok(Self { root })
    }

    /// 场景基线的文件路径。
    pub fn path_of(&self, name: &str) -> PathBuf {
        self.root.join(format!("{name}.{GOLDEN_EXTENSION}"))
    }

    /// 断言快照与冻结基线一致；基线缺失时生成、更新模式下覆盖。
    ///
    /// 用法:
    /// - 每条对照路径的"基线一致"测试调用一次；
    /// - 基线不一致且未开启更新模式时直接 panic，报告含差异路径与更新指引。
    ///
    /// 返回:
    /// - [`GoldenOutcome`] 说明本次是命中 / 首次生成 / 覆盖更新。
    pub fn assert_or_record(&self, snapshot: &Snapshot) -> ParityResult<GoldenOutcome> {
        let path = self.path_of(snapshot.label());

        let Some(frozen_value) = self.read(&path)? else {
            self.write(&path, snapshot)?;
            tracing::warn!(
                path = %path.display(),
                "对照基线首次生成（请人工审阅后随代码提交，作为回归基准）"
            );
            return Ok(GoldenOutcome::Recorded(path));
        };

        let frozen = Snapshot::new(snapshot.label(), frozen_value);
        let hint = format!(
            "差异若确认为预期行为变更，设置环境变量 PARITY_UPDATE_GOLDEN=1 重跑以更新基线（{}）；\
             否则应修复实现或修正场景 fixture。",
            path.display()
        );
        let report = diff_report(
            snapshot.label(),
            &frozen,
            snapshot,
            "冻结基线",
            "本次执行",
            &hint,
        );
        if let Some(report) = report {
            if update_requested() {
                self.write(&path, snapshot)?;
                tracing::warn!(path = %path.display(), "对照基线已按更新模式覆盖");
                return Ok(GoldenOutcome::Updated(path));
            }
            panic!("{report}");
        }
        Ok(GoldenOutcome::Matched(path))
    }

    // =========================================================
    // 文件读写（私有）
    // =========================================================

    /// 读取基线文件：不存在返回 `None`，解析失败返回结构化错误。
    fn read(&self, path: &Path) -> ParityResult<Option<Value>> {
        match std::fs::read_to_string(path) {
            Ok(text) => serde_json::from_str::<Value>(&text)
                .map(|value| Some(canon(value)))
                .map_err(|e| ParityError::golden(path, format!("基线文件解析失败：{e}"))),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(ParityError::golden(path, format!("读取基线文件失败：{e}"))),
        }
    }

    /// 写入基线文件（目录不存在时自动创建；统一以换行结尾）。
    fn write(&self, path: &Path, snapshot: &Snapshot) -> ParityResult<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|e| ParityError::golden(parent, format!("创建基线目录失败：{e}")))?;
        }
        let text = format!("{}\n", snapshot.to_pretty());
        std::fs::write(path, text)
            .map_err(|e| ParityError::golden(path, format!("写入基线文件失败：{e}")))
    }
}

/// 是否开启基线更新模式（`PARITY_UPDATE_GOLDEN` 非空且非假值）。
fn update_requested() -> bool {
    match std::env::var("PARITY_UPDATE_GOLDEN") {
        Ok(value) => !matches!(value.trim(), "" | "0" | "false" | "no"),
        Err(_) => false,
    }
}
