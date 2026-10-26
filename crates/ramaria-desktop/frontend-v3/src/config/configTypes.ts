/**
 * src/config/configTypes.ts — 配置文档的 JSON 形状
 *
 * 职责:
 * - 描述 `get_full_config` / `get_default_config` 返回的 JSON 文本结构
 *
 * 设计特点:
 * - 只声明「可索引的 JSON 文档」，具体配置键由 `settingsSchema` 单源提供
 * - 不镜像 Rust 结构体字段（避免第二份字段定义）；读写一律走路径函数
 */

/** JSON 原始值 */
export type JsonPrimitive = string | number | boolean | null

/** JSON 值 */
export type JsonValue = JsonPrimitive | JsonValue[] | { [key: string]: JsonValue }

/** 配置文档（完整 `RamariaConfig` 的 JSON 形态） */
export interface ConfigDocument {
  [key: string]: JsonValue
}

/** 解析配置 JSON 文本；结构非法时抛错（调用方给可读文案） */
export function parseConfigDocument(text: string): ConfigDocument {
  const value: unknown = JSON.parse(text)
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    throw new Error('配置结构不是对象')
  }
  return value as ConfigDocument
}

/** 序列化配置文档（写回 `update_full_config`） */
export function stringifyConfigDocument(document: ConfigDocument): string {
  return JSON.stringify(document)
}
