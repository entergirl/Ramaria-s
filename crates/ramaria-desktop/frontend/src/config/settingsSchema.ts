/**
 * src/config/settingsSchema.ts — 设置项单一事实源
 *
 * 职责:
 * - 汇总基础 / 进阶两组设置声明，提供按键、按组、按目录取字段的检索入口
 *
 * 设计特点:
 * - 键面覆盖：界面键 = `config/default.toml` 键面减去版本字段与 `[paths]`（由测试机器校验）
 * - 默认值不在此处：运行时经 `get_default_config` 读取 Rust `Default`，避免两份默认值
 * - 卡片顺序与栅格跨度在 `settingsSections.ts`，本文件只管「组里有什么」
 */

import { ADVANCED_GROUPS } from './schema/advanced'
import { BASIC_GROUPS } from './schema/basic'
import type { SettingField, SettingGroup } from './schema/define'

export type { SettingField, SettingGroup } from './schema/define'

/** 全部设置组（基础 6 卡 + 进阶 21 组，顺序固定便于调试） */
export const SETTINGS_GROUPS: SettingGroup[] = [...BASIC_GROUPS, ...ADVANCED_GROUPS]

const GROUPS_BY_ID = new Map(SETTINGS_GROUPS.map((group) => [group.id, group]))

/** 全部字段（扁平） */
export const SETTINGS_FIELDS: SettingField[] = SETTINGS_GROUPS.flatMap((group) => group.fields)

const FIELDS_BY_KEY = new Map(SETTINGS_FIELDS.map((field) => [field.key, field]))

/** 按组 id 取组；未知 id 返回 undefined */
export function groupById(id: string): SettingGroup | undefined {
  return GROUPS_BY_ID.get(id)
}

/** 按组 id 取字段（未知组返回空数组） */
export function fieldsOfGroup(id: string): SettingField[] {
  return GROUPS_BY_ID.get(id)?.fields ?? []
}

/** 按字段键取声明 */
export function fieldByKey(key: string): SettingField | undefined {
  return FIELDS_BY_KEY.get(key)
}

/** 全部界面键（点分路径） */
export function schemaKeys(): string[] {
  return SETTINGS_FIELDS.map((field) => field.key)
}

/** 界面上不出现的配置键（版本字段与文件路径） */
export const INTERNAL_KEYS = [
  'version',
  'schema_version',
  'paths.data_dir',
  'paths.config_dir',
  'paths.log_dir',
  'paths.vector_index_dir',
] as const
