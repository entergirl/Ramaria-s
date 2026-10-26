/**
 * src/core/import/form.ts — 导入表单的初值、回填与提交口径
 *
 * 职责:
 * - 定义导入表单结构与默认值
 * - 解析报告回填双方名称（用户已填则不覆盖）
 * - 提交前归一：空白转 null、切割间隔夹到 1~1440
 *
 * 设计特点:
 * - 纯函数，不依赖 Vue / Pinia / Tauri，组件与测试直接调用
 * - 默认「快速导入 + 双方 + 10 分钟」，与后端缺省一致，不因 UI 触发额外 LLM 花费
 * - 空串与纯空白按「沿用后端解析值」处理，对齐 `Option<String>` 语义
 */

import type { ImportAnalysisReport } from '../api/dto'

/** 导入模式 */
export type ImportMode = 'fast' | 'deep'

/** 导入侧过滤 */
export type ImportSide = 'both' | 'self' | 'other'

/** 导入表单 */
export interface ImportForm {
  filePath: string
  mode: ImportMode
  side: ImportSide
  gapMinutes: number
  /** 导出者（我方）persona 名称；空 = 沿用解析值 */
  selfName: string
  /** 导出者 persona UID；空 = 自动生成 */
  selfUid: string
  /** 对方 persona 名称；空 = 沿用解析值 */
  otherName: string
  /** 对方 persona UID；空 = 自动生成 */
  otherUid: string
}

/** 切割间隔允许范围（分钟） */
export const GAP_MIN = 1
export const GAP_MAX = 1440

/** 默认表单：快速导入、双方、10 分钟切割。 */
export function defaultImportForm(): ImportForm {
  return {
    filePath: '',
    mode: 'fast',
    side: 'both',
    gapMinutes: 10,
    selfName: '',
    selfUid: '',
    otherName: '',
    otherUid: '',
  }
}

/** 从完整路径取文件名（兼容 `/` 与 `\`）。 */
export function fileDisplayName(path: string): string {
  const normalized = path.trim()
  if (normalized === '') {
    return ''
  }
  const parts = normalized.split(/[/\\]/)
  return parts[parts.length - 1] || normalized
}

/** 归一空白：纯空白按空串处理。 */
function trimmed(value: string): string {
  return value.trim()
}

/**
 * 解析报告回填表单。
 *
 * 说明:
 * - 仅填充当前为空白的名称字段，用户手填值优先；
 * - UID 不回填（由用户显式填写或交给后端生成）。
 */
export function applyReportDefaults(
  form: ImportForm,
  report: ImportAnalysisReport,
): ImportForm {
  return {
    ...form,
    selfName: trimmed(form.selfName) === '' ? report.self_name : form.selfName,
    otherName: trimmed(form.otherName) === '' ? report.other_name : form.otherName,
  }
}

/** 提交参数（空值转 null，间隔夹到允许范围）。 */
export interface ImportSubmitPayload {
  mode: ImportMode
  side: ImportSide
  gapMinutes: number
  personaName: string | null
  selfPersonaUid: string | null
  otherPersonaName: string | null
  otherPersonaUid: string | null
}

/** 生成 `import_qq_chat` 的提交参数。 */
export function submitPayload(form: ImportForm): ImportSubmitPayload {
  const optional = (value: string): string | null => {
    const text = trimmed(value)
    return text === '' ? null : text
  }
  const gap = Number.isFinite(form.gapMinutes)
    ? Math.min(GAP_MAX, Math.max(GAP_MIN, Math.round(form.gapMinutes)))
    : 10

  return {
    mode: form.mode,
    side: form.side,
    gapMinutes: gap,
    personaName: optional(form.selfName),
    selfPersonaUid: optional(form.selfUid),
    otherPersonaName: optional(form.otherName),
    otherPersonaUid: optional(form.otherUid),
  }
}

/** 表单校验：未选文件时返回提示文案，否则空串。 */
export function formError(form: ImportForm): string {
  return trimmed(form.filePath) === '' ? '请先选择 QQ 聊天记录文件' : ''
}
