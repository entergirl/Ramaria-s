/**
 * src/core/memory/context.ts — L1 分组上下文解析（纯函数）
 *
 * 职责:
 * - 从 `context_json` 解析参与者列表与消息条数，供 L1 卡片的采集信息使用
 *
 * 设计特点:
 * - 解析失败或字段缺失一律降级为空结果，不抛错、不阻塞卡片渲染
 * - 只接受字符串数组与正整数，忽略其它形态，避免脏数据进入界面
 * - 参与者为后端写入的人格 UID，显示名由调用方按人格列表映射
 */

/** 解析后的 L1 分组上下文 */
export interface L1Context {
  /** 参与者人格 UID（保持后端顺序、去重） */
  partners: string[]
  /** 会话消息条数；缺失为 null */
  messageCount: number | null
}

const EMPTY: L1Context = { partners: [], messageCount: null }

/** 解析 `context_json`；空值或非对象返回空结果 */
export function parseL1Context(raw: string | null | undefined): L1Context {
  if (!raw) {
    return EMPTY
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(raw)
  } catch {
    return EMPTY
  }
  if (typeof parsed !== 'object' || parsed === null || Array.isArray(parsed)) {
    return EMPTY
  }

  const record = parsed as Record<string, unknown>
  const partners: string[] = []
  const rawPartners = record.chat_partners
  if (Array.isArray(rawPartners)) {
    for (const entry of rawPartners) {
      if (typeof entry !== 'string') {
        continue
      }
      const uid = entry.trim()
      if (uid.length > 0 && !partners.includes(uid)) {
        partners.push(uid)
      }
    }
  }

  const rawCount = record.message_count
  const messageCount =
    typeof rawCount === 'number' && Number.isFinite(rawCount) && rawCount > 0
      ? Math.round(rawCount)
      : null

  return { partners, messageCount }
}
