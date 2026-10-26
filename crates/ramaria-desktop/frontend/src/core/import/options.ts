/**
 * src/core/import/options.ts — 导入表单的可选项与说明文案
 *
 * 职责:
 * - 导入模式卡与导入侧下拉的单一文案来源
 *
 * 设计特点:
 * - 文案写明代价与边界（是否调用模型、是否创建对方人格），避免只写标签
 * - 与后端 `mode` / `side` 取值一一对应
 */

/** 导入模式选项 */
export const IMPORT_MODE_OPTIONS = [
  {
    value: 'fast',
    title: '快速导入',
    description: '只写入对话记录（L0），不触发记忆管线；适合先浏览历史对话。',
  },
  {
    value: 'deep',
    title: '深度导入',
    description: '写入后继续生成摘要、事件与画像，耗时更长且会调用对话模型。',
  },
] as const

/** 导入侧选项 */
export const IMPORT_SIDE_OPTIONS = [
  { value: 'both', label: '双方（both）' },
  { value: 'self', label: '仅我方（self）' },
  { value: 'other', label: '仅对方（other）' },
] as const

/** 导入侧说明 */
export const IMPORT_SIDE_HINT =
  '只导入某一侧的消息；被跳过一侧不入库、不创建对应 persona。'
