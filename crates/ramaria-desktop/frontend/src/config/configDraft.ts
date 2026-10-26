/**
 * src/config/configDraft.ts — 配置草稿的读写与比较
 *
 * 职责:
 * - 按路径读写配置文档、按字段集合取基线 / 计算脏值、把草稿字段合入最新配置
 *
 * 设计特点:
 * - 全部纯函数：写入返回新文档（结构共享），视图用返回值替换状态即可触发更新
 * - 脏值按字段键集合计算，保存成功后由调用方重置基线
 * - 与视图解耦：不读取 Pinia、不发命令，便于单测覆盖
 */

import type { ConfigDocument, JsonValue } from './configTypes'
import type { SettingField } from './schema/define'

/** 读取路径值；中间节点缺失或类型不符返回 undefined */
export function readPath(document: ConfigDocument, path: string[]): JsonValue | undefined {
  let current: JsonValue = document
  for (const segment of path) {
    if (!current || typeof current !== 'object' || Array.isArray(current)) {
      return undefined
    }
    current = current[segment]
    if (current === undefined) {
      return undefined
    }
  }
  return current
}

/** 写入路径值，返回新文档（沿途对象逐层浅拷贝） */
export function writePath(
  document: ConfigDocument,
  path: string[],
  value: JsonValue,
): ConfigDocument {
  const [head, ...rest] = path
  if (head === undefined) {
    return document
  }
  if (rest.length === 0) {
    return { ...document, [head]: value }
  }
  const child = document[head]
  const base: ConfigDocument =
    child && typeof child === 'object' && !Array.isArray(child)
      ? (child as ConfigDocument)
      : {}
  return { ...document, [head]: writePath(base, rest, value) }
}

/** 深拷贝文档（仅 JSON 值） */
export function cloneDocument(document: ConfigDocument): ConfigDocument {
  return JSON.parse(JSON.stringify(document)) as ConfigDocument
}

/** JSON 深比较 */
export function jsonEqual(left: JsonValue | undefined, right: JsonValue | undefined): boolean {
  if (left === right) {
    return true
  }
  if (left === undefined || right === undefined) {
    return false
  }
  if (typeof left !== 'object' || typeof right !== 'object') {
    return false
  }
  if (left === null || right === null) {
    return left === right
  }
  if (Array.isArray(left) || Array.isArray(right)) {
    if (!Array.isArray(left) || !Array.isArray(right) || left.length !== right.length) {
      return false
    }
    return left.every((item, index) => jsonEqual(item, right[index]))
  }
  const leftKeys = Object.keys(left)
  const rightKeys = Object.keys(right)
  if (leftKeys.length !== rightKeys.length) {
    return false
  }
  return leftKeys.every((key) => jsonEqual(left[key], right[key]))
}

/** 取字段集合在各文档中的值（键为点分路径） */
export function snapshotFields(
  document: ConfigDocument,
  fields: SettingField[],
): Record<string, JsonValue | undefined> {
  const snapshot: Record<string, JsonValue | undefined> = {}
  for (const field of fields) {
    snapshot[field.key] = readPath(document, field.path)
  }
  return snapshot
}

/** 草稿相对基线的脏字段键（按字段声明顺序） */
export function dirtyFieldKeys(
  draft: ConfigDocument,
  baseline: Record<string, JsonValue | undefined>,
  fields: SettingField[],
): string[] {
  return fields
    .filter((field) => !jsonEqual(readPath(draft, field.path), baseline[field.key]))
    .map((field) => field.key)
}

/**
 * 把草稿中的字段值合入目标文档；缺失的字段跳过（不写入 undefined）。
 *
 * 说明:
 * - 目标文档应是保存前重新读取的最新配置，避免用页面打开时的旧值覆盖他处改动
 */
export function mergeFields(
  target: ConfigDocument,
  source: ConfigDocument,
  fields: SettingField[],
): ConfigDocument {
  let merged = target
  for (const field of fields) {
    const value = readPath(source, field.path)
    if (value === undefined) {
      continue
    }
    merged = writePath(merged, field.path, value)
  }
  return merged
}

/** 把默认配置中的字段值填回草稿（恢复默认：只改表单，不落库） */
export function applyDefaults(
  draft: ConfigDocument,
  defaults: ConfigDocument,
  fields: SettingField[],
): ConfigDocument {
  return mergeFields(draft, defaults, fields)
}
