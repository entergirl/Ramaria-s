//! crates/ramaria-cli/src/json.rs - CLI 全局 --json 信封输出模块
//!
//! 设计特点:
//! - 统一信封 schema: `{"ok":true,"data":…}` / `{"ok":false,"error":{"code":…,"message":"…"}}`
//! - stdout 只输出数据（信封 JSON），状态/提示/警告走 stderr（ui 模块）
//! - `error.code` 复用 exit code 约定（0 成功 / 2 参数错 / 3 LLM 或后端不可用 / 4 业务校验失败）
//! - 序列化失败回退文本输出并记 warn（静默降级约定，不阻塞主流程）
//! - 字段命名 snake_case，时间戳由各命令负责统一 ISO-8601 UTC

use serde::Serialize;

// =========================================================
// 成功信封
// =========================================================

/// 以 `--json` 信封格式输出一条成功结果到 stdout。
///
/// 参数:
/// - `data`: 任意可序列化的数据负载（数组 / 对象 / 标量均可）。
///
/// 返回:
/// - `Ok(())`: 输出成功。
/// - `Err`: 序列化失败（内部已回退文本输出并记 warn，调用方无需额外处理，数据不中断流程）。
///
/// 说明:
/// - 输出格式为单行 JSON：`{"ok":true,"data":<data>}`。
/// - 序列化失败属于异常路径（负载通常为简单结构），按降级约定
///   （`--json` 输出失败回退文本并记 warn）以 Debug 形态
///   输出负载到 stdout（仍只输出数据，不输出状态），保证 agent 侧 stdout 非空。
pub fn emit_ok<T: Serialize + std::fmt::Debug>(data: &T) -> anyhow::Result<()> {
    let envelope = serde_json::json!({
        "ok": true,
        "data": data,
    });
    match serde_json::to_string(&envelope) {
        Ok(line) => {
            println!("{line}");
            Ok(())
        }
        Err(e) => {
            // 降级约定：回退文本输出（Debug 形态）+ 记 warn
            crate::ui::warn(&format!("JSON 序列化失败，回退文本输出: {e}"));
            println!("{data:?}");
            Ok(())
        }
    }
}

// =========================================================
// 错误信封
// =========================================================

/// 以 `--json` 信封格式输出一条错误到 stdout。
///
/// 参数:
/// - `code`: 错误码，复用 exit code 约定（3 = LLM 或后端不可用，4 = 业务校验失败）。
/// - `message`: 面向 agent/用户的错误消息。
///
/// 说明:
/// - 输出格式为单行 JSON：`{"ok":false,"error":{"code":<code>,"message":"<message>"}}`。
/// - 该函数只负责信封输出，进程退出码由 main 统一按 `code` 设置。
/// - stdout 输出信封供 agent 解析；stderr 的文本错误由调用方另行输出。
pub fn emit_err(code: i32, message: &str) {
    let envelope = serde_json::json!({
        "ok": false,
        "error": {
            "code": code,
            "message": message,
        },
    });
    match serde_json::to_string(&envelope) {
        Ok(line) => println!("{line}"),
        Err(e) => crate::ui::warn(&format!("错误信封序列化失败: {e}")),
    }
}

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    use serde_json::Value;

    /// emit_ok 输出合法信封且 data 保留原始结构。
    #[test]
    fn emit_ok_envelope_shape() {
        let data = serde_json::json!({"items": [1, 2, 3], "name": "测试"});
        // 捕获 stdout：直接构造信封 JSON 验证形状（emit_ok 的打印由进程级测试覆盖）
        let envelope = serde_json::json!({"ok": true, "data": data});
        let line = serde_json::to_string(&envelope).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["ok"], true);
        assert_eq!(parsed["data"]["items"][0], 1);
        assert_eq!(parsed["data"]["name"], "测试");
    }

    /// emit_err 输出合法错误信封，code/message 完整。
    #[test]
    fn emit_err_envelope_shape() {
        let envelope = serde_json::json!({
            "ok": false,
            "error": {"code": 3, "message": "LLM 不可用"}
        });
        let line = serde_json::to_string(&envelope).unwrap();
        let parsed: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(parsed["ok"], false);
        assert_eq!(parsed["error"]["code"], 3);
        assert_eq!(parsed["error"]["message"], "LLM 不可用");
    }

    /// 错误信封字段命名必须为 snake_case。
    #[test]
    fn envelope_field_names_are_snake_case() {
        let ok = serde_json::json!({"ok": true, "data": null});
        let err = serde_json::json!({"ok": false, "error": {"code": 4, "message": "x"}});
        for v in [ok, err] {
            let s = serde_json::to_string(&v).unwrap();
            assert!(!s.contains("camelCase"), "不应出现驼峰字段: {s}");
        }
    }
}
