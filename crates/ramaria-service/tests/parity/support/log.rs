//! crates/ramaria-service/tests/parity/support/log.rs - 对照测试日志（按需输出到 stderr）
//!
//! 设计特点:
//! - 默认静默：未设置 `PARITY_LOG` 时不注册 subscriber，tracing 调用保持零开销，
//!   成功路径不产生噪声（`cargo test` 输出保持干净）
//! - 按需开启：`PARITY_LOG=1`（或 `info` / `debug` / `trace` / `warn` / `error`）时注册
//!   一个最小 subscriber，把服务层用例的 tracing 事件输出到 stderr，供排查行为漂移
//! - 无外部依赖：手工实现 `tracing::Subscriber` 的最小面，不引入 tracing-subscriber
//! - 只注册一次：多测试并发调用时用 `Once` 保护，注册失败（如已有全局 subscriber）不阻塞用例
//! - 输出格式固定：`[parity][级别] 目标: 字段列表`，字段按事件记录顺序拼接

use std::fmt;
use std::sync::Once;

use tracing::field::{Field, Visit};
use tracing::level_filters::LevelFilter;
use tracing::span::{Attributes, Id, Record};
use tracing::{Event, Level, Metadata, Subscriber};

/// 注册入口的幂等保护。
static INIT: Once = Once::new();

/// 按环境变量注册对照测试日志（幂等；未设置 `PARITY_LOG` 时为空操作）。
///
/// 用法:
/// - 用例执行前调用一次（`ParityEnv` 构建时统一调用），无需关心重复注册。
///
/// 说明:
/// - 级别映射：`trace` / `debug` / `warn` / `error` 原样，其余非空值按 `info` 处理；
/// - 已存在全局 subscriber 时注册失败仅提示，不影响测试结论。
pub fn init_parity_log() {
    INIT.call_once(|| {
        let Some(raw) = std::env::var_os("PARITY_LOG") else {
            return;
        };
        let level = match raw.to_string_lossy().to_ascii_lowercase().as_str() {
            "trace" => Level::TRACE,
            "debug" => Level::DEBUG,
            "warn" => Level::WARN,
            "error" => Level::ERROR,
            _ => Level::INFO,
        };

        if let Err(e) = tracing::subscriber::set_global_default(StderrSubscriber { level }) {
            eprintln!("[parity] tracing subscriber 注册失败（日志保持静默）：{e}");
            return;
        }
        // 全局发布成功后，tracing 按 subscriber 的 `max_level_hint` 自动收敛 Interest 缓存，
        // 无需额外设置全局级别。
        eprintln!("[parity] 日志已开启（级别 {level}，输出到 stderr）");
    });
}

// =========================================================
// 最小 stderr subscriber
// =========================================================

/// 最小输出 subscriber：把事件按固定格式写到 stderr。
///
/// 字段约定:
/// - `level`: 输出阈值；低于该级别的元数据直接判定为未启用（配合 `max_level_hint` 早退）。
struct StderrSubscriber {
    level: Level,
}

impl Subscriber for StderrSubscriber {
    fn enabled(&self, metadata: &Metadata<'_>) -> bool {
        metadata.level() <= &self.level
    }

    fn max_level_hint(&self) -> Option<LevelFilter> {
        Some(LevelFilter::from_level(self.level))
    }

    fn new_span(&self, _span: &Attributes<'_>) -> Id {
        // 只消费事件不消费 span：固定返回根 span id，避免分配
        Id::from_u64(1)
    }

    fn record(&self, _span: &Id, _values: &Record<'_>) {}

    fn record_follows_from(&self, _span: &Id, _follows: &Id) {}

    fn event(&self, event: &Event<'_>) {
        let metadata = event.metadata();
        let mut visitor = FieldsVisitor::default();
        event.record(&mut visitor);
        eprintln!(
            "[parity][{}] {}: {}",
            metadata.level(),
            metadata.target(),
            visitor.render()
        );
    }

    fn enter(&self, _span: &Id) {}

    fn exit(&self, _span: &Id) {}
}

/// 事件字段收集器：把 `message` 字段渲染为正文，其余字段渲染为 `键=值`。
#[derive(Default)]
struct FieldsVisitor {
    rendered: Vec<String>,
}

impl FieldsVisitor {
    /// 拼接全部字段（无字段时返回空串）。
    fn render(&self) -> String {
        self.rendered.join(" ")
    }
}

impl Visit for FieldsVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        if field.name() == "message" {
            self.rendered.push(format!("{value:?}"));
        } else {
            self.rendered.push(format!("{}={value:?}", field.name()));
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        if field.name() == "message" {
            self.rendered.push(value.to_string());
        } else {
            self.rendered.push(format!("{}={value}", field.name()));
        }
    }
}
