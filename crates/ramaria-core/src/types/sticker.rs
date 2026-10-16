//! crates/ramaria-core/src/types/sticker.rs - Ramaria 表情资产数据类型模块
//!
//! 设计特点:
//! - 描述聊天平台表情图片的本地资产登记信息（文件名 / 路径 / 内容校验）
//! - tags 以文本数组承载人工或模型标注的检索标签
//! - 时间字段统一 Unix 毫秒（created_at / updated_at），与全体实体一致
//! - 支持 serde，供 CLI、IPC、存储层共享

use serde::{Deserialize, Serialize};

/// 表情资产登记行。
///
/// 职责:
/// - 记录一张表情图片在资产目录中的落盘位置与内容特征。
/// - 供表情包管理界面与图片理解链路按 md5 复用描述。
///
/// 字段约定:
/// - `file_path`: 资产目录内的文件名（不含目录前缀）；
/// - `md5`: 图片内容 md5（小写 hex）；未计算时为 None；
/// - `size`: 文件字节数；未知时为 None；
/// - `description`: 图片理解生成的描述文本；未生成为 None；
/// - `tags`: 标签列表，库内以 JSON 文本数组存储。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StickerAsset {
    pub id: i64,
    pub file_name: String,
    pub file_path: String,
    pub md5: Option<String>,
    pub size: Option<u64>,
    pub description: Option<String>,
    pub tags: Vec<String>,
    pub created_at: i64,
    pub updated_at: i64,
}
