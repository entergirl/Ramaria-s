/**
 * src/core/chat/merge.ts — 消息列表合并（纯函数）
 *
 * 职责:
 * - 把服务端返回的最新消息页并入本地列表，替换乐观消息与流式占位
 * - 主动消息到达时按消息标识去重追加
 *
 * 设计特点:
 * - 以服务端页首条消息为接缝：接缝之前保留本地更早的消息，接缝之后整段以服务端为准
 * - 找不到接缝（新会话、乐观消息尚未落库）时整页替换，避免保留临时标识
 * - 不改写入参数组，始终返回新数组，便于响应式更新
 */
import type { SessionMessage } from '../api/dto'

/**
 * 合并服务端消息页。
 *
 * 参数:
 * - `existing`: 本地已有消息（正序）
 * - `page`: 服务端返回的最新一页（正序，条数受限）
 *
 * 返回:
 * - 合并后的新列表
 */
export function mergeServerPage(
  existing: SessionMessage[],
  page: SessionMessage[],
): SessionMessage[] {
  if (page.length === 0) {
    return existing
  }
  // 取服务端页中第一个落在本地列表内的消息作为接缝：页首被编辑或补齐时仍能对齐
  const existingIds = new Set(existing.map((message) => message.id))
  const seamMessage = page.find((message) => existingIds.has(message.id))
  if (!seamMessage) {
    return [...page]
  }
  const cut = existing.findIndex((message) => message.id === seamMessage.id)
  return [...existing.slice(0, cut), ...page]
}

/**
 * 追加一条消息（按标识去重）。
 *
 * 参数:
 * - `list`: 当前消息列表
 * - `message`: 待追加消息
 *
 * 返回:
 * - 追加后的新列表；已存在同标识消息时原样返回
 */
export function appendMessage(
  list: SessionMessage[],
  message: SessionMessage,
): SessionMessage[] {
  if (list.some((item) => item.id === message.id)) {
    return list
  }
  return [...list, message]
}
