//! crates/ramaria-eval/src/baseline/tests.rs - Ramaria 评测基线入口单元测试
//!
//! 设计特点:
//! - 只覆盖参数前置校验：缺档位在建库 / 读文件之前即失败
//! - 不装配引擎、不读语料、不写 `.eval/` 产物

use ramaria_core::error::RamariaError;

use super::backfill;

/// 回填映射缺档位时返回校验错误，文案提示该参数。
#[tokio::test]
async fn backfill_requires_explicit_tier() {
    let error = backfill(None, None, None, None)
        .await
        .expect_err("缺档位应报错");
    assert!(
        matches!(&error, RamariaError::Validation { .. }),
        "应为 Validation，实际 {error:?}"
    );
    assert!(
        error.to_string().contains("--tier"),
        "文案应提示档位参数，实际 {error}"
    );
}
