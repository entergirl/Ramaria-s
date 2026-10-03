//! crates/ramaria-desktop/tests/notification_click_spike.rs - Windows 通知点击回调手动验证
//!
//! 设计特点:
//! - 验证 Windows 底层 winrt toast 的 `on_activated` 点击回调链路（通知插件桌面侧不暴露点击回调）
//! - 全部用例 `#[ignore]`：不随常规测试与 CI 运行，由负责人按需手动执行
//! - 串行锁保证多条通知逐条出现，避免手动点击时混淆
//! - 覆盖自定义 AUMID 与 PowerShell 兜底两种应用身份，含通知中心补点场景
//!
//! 手动运行（工作根目录 main/）:
//! ```text
//! cargo test -j 2 -p ramaria-desktop --test notification_click_spike -- --ignored --nocapture --test-threads=1
//! ```
//! 运行后按各用例打印的提示点击系统通知。

#![cfg(windows)]

use std::sync::{Mutex, mpsc};
use std::time::Duration;

use tauri_winrt_notification::{Duration as ToastDuration, Toast};

/// 应用标识（与 tauri.conf.json 的 identifier 一致；安装版通知使用该身份）
const APP_IDENTIFIER: &str = "com.ramaria.app";

/// 单次点击等待上限
const CLICK_TIMEOUT: Duration = Duration::from_secs(120);

/// 串行锁：并行执行时保证通知逐条出现
static SPIKE_LOCK: Mutex<()> = Mutex::new(());

/// 展示一条通知并等待点击回调。
///
/// 返回:
/// - `Some(action)`: 回调触发；`action` 为 `None` 表示点击通知本体，`Some` 为按钮参数
/// - `None`: 超时未收到回调
fn show_and_wait_click(
    app_id: &str,
    title: &str,
    body: &str,
    timeout: Duration,
) -> Option<Option<String>> {
    let (tx, rx) = mpsc::channel();
    Toast::new(app_id)
        .title(title)
        .text1(body)
        .duration(ToastDuration::Short)
        .on_activated(move |action| {
            let _ = tx.send(action);
            Ok(())
        })
        .show()
        .expect("toast 发送失败（请记录完整报错）");
    rx.recv_timeout(timeout).ok()
}

/// 获取串行锁（容忍脏锁：某场景失败不阻塞后续场景）
fn lock_spike() -> std::sync::MutexGuard<'static, ()> {
    SPIKE_LOCK
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
}

/// 场景 1：PowerShell 兜底身份 + 点击通知本体（横幅）。
#[test]
#[ignore = "手动实机验证：需要点击系统通知"]
fn spike_body_click_powershell_identity() {
    let _guard = lock_spike();
    println!("[场景 1/3] 身份 = PowerShell 兜底；操作 = 通知出现后立即点击通知本体（任意位置）");
    let clicked = show_and_wait_click(
        Toast::POWERSHELL_APP_ID,
        "Ramaria 点击验证（1/3）",
        "请点击这条通知本体",
        CLICK_TIMEOUT,
    );
    assert!(clicked.is_some(), "场景 1：超时未收到点击回调");
    println!("[场景 1/3] 通过：收到点击回调");
}

/// 场景 2：应用标识身份（安装版形态）+ 点击通知本体（横幅）。
#[test]
#[ignore = "手动实机验证：需要点击系统通知"]
fn spike_body_click_app_identity() {
    let _guard = lock_spike();
    println!(
        "[场景 2/3] 身份 = com.ramaria.app（安装版身份）；操作 = 通知出现后立即点击通知本体；同时留意通知显示的应用名"
    );
    let clicked = show_and_wait_click(
        APP_IDENTIFIER,
        "Ramaria 点击验证（2/3）",
        "请点击这条通知本体",
        CLICK_TIMEOUT,
    );
    assert!(clicked.is_some(), "场景 2：超时未收到点击回调");
    println!("[场景 2/3] 通过：收到点击回调");
}

/// 场景 3：PowerShell 兜底身份 + 横幅收起后在通知中心补点。
#[test]
#[ignore = "手动实机验证：需要点击系统通知"]
fn spike_action_center_click() {
    let _guard = lock_spike();
    println!(
        "[场景 3/3] 身份 = PowerShell 兜底；操作 = 不要点击横幅，等其收起（约 10 秒）后按 Win+N 打开通知中心，再在通知中心点击该条通知"
    );
    let clicked = show_and_wait_click(
        Toast::POWERSHELL_APP_ID,
        "Ramaria 点击验证（3/3）",
        "先不要点；等横幅收起后到通知中心点",
        CLICK_TIMEOUT,
    );
    assert!(
        clicked.is_some(),
        "场景 3：超时未收到点击回调（通知中心补点）"
    );
    println!("[场景 3/3] 通过：收到点击回调");
}
