/**
 * src/core/persona/draft.ts — 人格编辑草稿（纯函数）
 *
 * 职责:
 * - 由人格视图派生编辑草稿、判断是否有未保存修改、校验名称
 *
 * 设计特点:
 * - 草稿字段全部为字符串：缺失的头像 / 描述按空串编辑，提交时空串表达清空
 * - 脏判据以草稿与后端值逐字段比对，首尾空白不计入头像差异
 * - 名称 / 描述的字符上限在此收口，输入框只做截断调用
 */

import type { PersonaFull } from '../api/dto'

/** 名称最大字符数 */
export const NAME_MAX = 24

/** 描述最大字符数 */
export const DESCRIPTION_MAX = 200

/** 编辑草稿 */
export interface PersonaDraft {
  name: string
  avatar: string
  description: string
}

/** 按后端值生成草稿 */
export function draftOf(persona: PersonaFull): PersonaDraft {
  return {
    name: persona.name,
    avatar: persona.avatar ?? '',
    description: persona.description ?? '',
  }
}

/** 草稿与后端值是否存在差异 */
export function draftChanged(draft: PersonaDraft, persona: PersonaFull): boolean {
  const base = draftOf(persona)
  return (
    draft.name !== base.name ||
    draft.avatar.trim() !== base.avatar.trim() ||
    draft.description !== base.description
  )
}

/** 名称校验：空名返回错误文案，合法返回空串 */
export function nameError(name: string): string {
  const trimmed = name.trim()
  if (trimmed.length === 0) {
    return '名称不能为空'
  }
  return trimmed.length > NAME_MAX ? `名称最多 ${NAME_MAX} 字` : ''
}

/** 按上限截断输入（用于输入事件，避免超长写入草稿） */
export function clampText(value: string, limit: number): string {
  return value.length > limit ? value.slice(0, limit) : value
}
