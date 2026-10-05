//! crates/ramaria-service/src/engine/usecases_proactive.rs - Ramaria 主动对话名单用例挂载
//!
//! 设计特点:
//! - 用例薄壳：实现体在 `proactive::roster`，本层只做入口挂载
//! - 读用例为实时计算：不物化写入，每次按人格列表 + 开关 + 对话存在性重新推导
//! - 写用例先校验（值域 / uid 存在 / 非 user）后落开关

use ramaria_core::error::RamariaResult;

use crate::proactive::ProactivePersonaView;

use super::Engine;

// =========================================================
// 主动消息名单用例
// =========================================================

impl Engine {
    /// 主动消息名单读用例：列出活跃人格的开关状态与生效结论。
    ///
    /// 返回:
    /// - 按存储层人格列表排序（kind, seq）的名单条目；任一查询失败上抛。
    pub async fn proactive_persona_list(&self) -> RamariaResult<Vec<ProactivePersonaView>> {
        crate::proactive::list_personas(self).await
    }

    /// 主动消息名单写用例：校验（值域 / uid 存在 / 非 user）后保存开关。
    ///
    /// 参数:
    /// - `uid`: 人格业务标识（两侧空白容忍）。
    /// - `mode`: 开关文本（auto / on / off，两侧空白容忍）。
    ///
    /// 返回:
    /// - 校验失败为业务校验错误；查询 / 写入失败为存储错误。
    pub async fn proactive_persona_set_mode(&self, uid: &str, mode: &str) -> RamariaResult<()> {
        crate::proactive::set_persona_mode(self, uid, mode).await
    }
}
