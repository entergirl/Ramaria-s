/**
 * src/core/persona/select.ts — 人格筛选的默认选中（纯函数）
 *
 * 职责:
 * - 在人格列表里按优先级挑出要展示的人格
 *
 * 设计特点:
 * - 候选值按调用方给定的顺序命中，跨视图跳转的一次性预选优先生效
 * - 回退顺序固定：活跃的 rama 人格 → 任一 rama 人格 → 列表首个
 * - 只读入参与返回值，不读 store、不触发请求
 */

import type { PersonaSummary } from '../api/dto'

/**
 * 挑选默认展示的人格。
 *
 * 参数:
 * - `personas`: 已装载的人格列表
 * - `candidates`: 优先级从高到低的候选 UID（不存在于列表中的会被跳过）
 *
 * 返回:
 * - 命中的 UID；列表为空时为 `null`
 */
export function selectPersonaUid(
  personas: PersonaSummary[],
  candidates: Array<string | null | undefined> = [],
): string | null {
  for (const candidate of candidates) {
    if (candidate && personas.some((persona) => persona.uid === candidate)) {
      return candidate
    }
  }
  const fallback =
    personas.find((persona) => persona.kind === 'rama' && persona.is_active) ??
    personas.find((persona) => persona.kind === 'rama') ??
    personas[0]
  return fallback?.uid ?? null
}
