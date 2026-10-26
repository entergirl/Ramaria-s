/**
 * src/core/chat/receipt.ts — 命令回执解析（纯函数）
 *
 * 职责:
 * - 把「保存对话」与「L1 重试」返回的 JSON 文本解析为视图可用的回执
 *
 * 设计特点:
 * - 解析失败返回 null，由调用方按「未知结果」处理，不误报成功
 * - 只校验关键枚举值，其余字段按存在性透出
 */
import type { L1RetryReceipt, SaveSessionReceipt } from '../api/dto'

function parseJson(raw: string): unknown {
  try {
    return JSON.parse(raw) as unknown
  } catch {
    return null
  }
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null
}

/** 解析保存对话回执 */
export function parseSaveReceipt(raw: string): SaveSessionReceipt | null {
  const value = parseJson(raw)
  if (!isRecord(value)) {
    return null
  }
  const status = value.status
  if (status !== 'ok' && status !== 'no_active_session') {
    return null
  }
  return value as unknown as SaveSessionReceipt
}

/** 解析 L1 重试回执 */
export function parseL1Receipt(raw: string): L1RetryReceipt | null {
  const value = parseJson(raw)
  if (!isRecord(value) || typeof value.l1_generated !== 'boolean') {
    return null
  }
  return value as unknown as L1RetryReceipt
}
