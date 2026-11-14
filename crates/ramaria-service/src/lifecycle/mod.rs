//! crates/ramaria-service/src/lifecycle/mod.rs - Ramaria 会话生命周期能力（活跃指针 / 空闲检查 / L2-L3 调度 / 关停）
//!
//! 设计特点:
//! - 与传输无关的生命周期容器：活跃指针、手动关闭、空闲检测、L2/L3 调度、主动对话调度与关停统一装配
//! - 宿主差异全部由 [`LifecycleOptions`] 表达（长驻宿主 / 仅空闲检查 / 单次执行），后台循环按选项拉起
//! - `l1`：L1 摘要生成与重试（手动重生成、封存失败遗留任务的补扫消费点）
//! - `l2_l3`：L2 事件提取触发（含无主 L1 归属）与 L3 性格推断级联
//! - `idle`：空闲检查线程（阈值热更新、全库扫描、活跃指针一致性、状态刷新）
//! - 主动对话调度：`proactive::spawn` 经 container 装配（仅桌面宿主启用）
//! - 停止语义：原子停止位供轮次内判定，`watch` 广播把等待中的循环立即叫醒；关停等待收敛（超时只记日志）
//!
//! 模块划分:
//! - `options`：装配选项（宿主差异全部由 [`LifecycleOptions`] 表达）；
//! - `container`：生命周期容器 [`Lifecycle`]（装配、活跃指针、空闲检查、手动关闭与关停）；
//! - `l1` / `l2_l3` / `idle`：级联用例与后台循环实现。

pub mod idle;
pub mod l1;
pub mod l2_l3;

mod container;
mod options;

pub use container::Lifecycle;
pub use options::LifecycleOptions;

/// 等待停止广播；返回 `true` 表示循环应立即退出。
///
/// 说明:
/// - 已置位时立即返回 `true`；
/// - 发送端被丢弃（宿主提前释放容器）同样返回 `true`，避免循环在已失效的广播上空转；
/// - 停止位仍由原子标志承担轮次内判定，本函数只负责把等待中的任务叫醒。
pub(crate) async fn wait_shutdown(rx: &mut tokio::sync::watch::Receiver<bool>) -> bool {
    if *rx.borrow() {
        return true;
    }
    match rx.changed().await {
        Ok(()) => *rx.borrow(),
        Err(_) => true,
    }
}

#[cfg(test)]
mod tests;
