/**
 * src/config/schema/define.ts — 设置项元数据类型与构造器
 *
 * 职责:
 * - 定义设置字段 / 设置组的声明结构，以及字段构造的便捷函数
 *
 * 设计特点:
 * - 字段只声明「配置路径 + 标签 + 控件 + 约束」，默认值不在此处（运行时取 `get_default_config`）
 * - `key` 由路径拼接，作为脏值与表单绑定的唯一标识
 * - 组按卡片粒度声明（`[proactive]` 拆 4 卡），`blocks` 只做大组内部小节
 */

/** 字段控件类型 */
export type SettingControl = 'switch' | 'number' | 'text' | 'select' | 'whitelist' | 'order'

/** 生效方式：hot = 保存即生效；restart = 需重启；未声明表示下次使用生效 */
export type SettingApply = 'hot' | 'restart'

/** 下拉 / 白名单 / 排序字段的选项 */
export interface SettingOption {
  value: string
  label: string
}

/** 设置字段声明 */
export interface SettingField {
  /** 完整配置路径（含组前缀），如 `['session', 'l1_idle_minutes']` */
  path: string[]
  /** 点分唯一键，如 `session.l1_idle_minutes` */
  key: string
  label: string
  control: SettingControl
  hint?: string
  unit?: string
  min?: number
  max?: number
  step?: number
  options?: SettingOption[]
  apply?: SettingApply
  /** 开关开启前的确认文案（隐私项） */
  confirm?: string
  /** 栅格占位：full = 独占整行（缺省）；auto = 按列宽自适应 */
  layout?: 'full' | 'auto'
}

/** 组内小节（只影响呈现顺序，不改变保存粒度） */
export interface SettingBlock {
  title: string
  keys: string[]
}

/** 设置组声明（一张卡片） */
export interface SettingGroup {
  /** 组标识（卡片 id） */
  id: string
  title: string
  description?: string
  fields: SettingField[]
  blocks?: SettingBlock[]
  /** 卡片内固定列数；缺省按可用宽度自适应 */
  columns?: 2 | 3 | 4
}

const CONTROL_DEFAULT: Record<SettingControl, Partial<SettingField>> = {
  switch: {},
  number: {},
  text: {},
  select: {},
  whitelist: {},
  order: {},
}

/** 构造字段：路径、标签、控件与附加约束 */
export function field(
  path: string[],
  label: string,
  control: SettingControl,
  extra: Partial<Omit<SettingField, 'path' | 'key' | 'label' | 'control'>> = {},
): SettingField {
  const key = path.join('.')
  return { path, key, label, control, ...CONTROL_DEFAULT[control], ...extra }
}

/** 布尔开关 */
export function toggle(path: string[], label: string, hint?: string): SettingField {
  return field(path, label, 'switch', hint ? { hint } : {})
}

/** 数值输入 */
export function num(
  path: string[],
  label: string,
  extra: Partial<Omit<SettingField, 'path' | 'key' | 'label' | 'control'>> = {},
): SettingField {
  return field(path, label, 'number', extra)
}

/** 文本输入 */
export function text(path: string[], label: string, hint?: string): SettingField {
  return field(path, label, 'text', hint ? { hint } : {})
}

/** 下拉选择 */
export function select(
  path: string[],
  label: string,
  options: SettingOption[],
  hint?: string,
): SettingField {
  return field(path, label, 'select', hint ? { hint, options } : { options })
}

/** 多选白名单 */
export function whitelist(
  path: string[],
  label: string,
  options: SettingOption[],
  hint?: string,
): SettingField {
  return field(path, label, 'whitelist', hint ? { hint, options } : { options })
}

/** 有序优先级列表 */
export function order(
  path: string[],
  label: string,
  options: SettingOption[],
  hint?: string,
): SettingField {
  return field(path, label, 'order', hint ? { hint, options } : { options })
}
