/**
 * src/config/settingsSections.ts — 设置页的信息架构
 *
 * 职责:
 * - 编排两级目录：范围（基础 / 进阶）→ 子页 / 类 → 卡片与栅格跨度
 *
 * 设计特点:
 * - 基础 5 子页 / 11 卡片；进阶 8 类 / 21 组（与内容规格 §6 一致）
 * - 卡片分两类：`schema` 由键表驱动，其余为视图自绘（隐私 / 概览 / 名单 / 数据 / 诊断）
 * - 子页 id 即深链段（`#/settings/:scope/:section`），改名视为契约变更
 */

/** 设置范围 */
export type SettingsScope = 'basic' | 'advanced'

/** 12 列栅格跨度 */
export type GridSpan = 4 | 5 | 6 | 7 | 8 | 12

/** 卡片来源：键表驱动或视图自绘 */
export type CardKind =
  | 'schema'
  | 'backend'
  | 'embedding'
  | 'channel'
  | 'mcp'
  | 'privacy'
  | 'proactive-overview'
  | 'proactive-roster'
  | 'data'
  | 'diagnostics'

/** 卡片引用 */
export interface SettingsCardRef {
  id: string
  kind: CardKind
  span: GridSpan
}

/** 由配置组键表驱动的卡片：参与组保存、脏值统计与恢复默认 */
const GROUP_CARD_KINDS: ReadonlySet<CardKind> = new Set([
  'schema',
  'backend',
  'embedding',
  'channel',
  'mcp',
])

/** 卡片是否由配置组键表驱动 */
export function isGroupCard(kind: CardKind): boolean {
  return GROUP_CARD_KINDS.has(kind)
}

/** 子页 / 类 */
export interface SettingsSection {
  id: string
  label: string
  cards: SettingsCardRef[]
}

/** 范围显示名 */
export const SCOPE_LABELS: Record<SettingsScope, string> = {
  basic: '基础设置',
  advanced: '进阶设置',
}

/** 基础设置：5 子页 / 11 卡片（`[proactive]` 基础页只保留总开关与节流） */
export const BASIC_SECTIONS: SettingsSection[] = [
  {
    id: 'model',
    label: '模型与检索',
    cards: [
      { id: 'backend', kind: 'backend', span: 6 },
      { id: 'embedding', kind: 'embedding', span: 6 },
    ],
  },
  {
    id: 'privacy',
    label: '会话与隐私',
    cards: [
      { id: 'session', kind: 'schema', span: 12 },
      { id: 'privacy', kind: 'privacy', span: 12 },
    ],
  },
  {
    id: 'proactive',
    label: '主动对话',
    cards: [
      { id: 'proactive-core', kind: 'schema', span: 8 },
      { id: 'proactive-overview', kind: 'proactive-overview', span: 4 },
      { id: 'proactive-roster', kind: 'proactive-roster', span: 12 },
    ],
  },
  {
    id: 'social',
    label: '社交与接入',
    cards: [
      { id: 'channel', kind: 'channel', span: 12 },
      { id: 'mcp', kind: 'mcp', span: 12 },
    ],
  },
  {
    id: 'data',
    label: '数据与系统',
    cards: [
      { id: 'data', kind: 'data', span: 7 },
      { id: 'diagnostics', kind: 'diagnostics', span: 5 },
    ],
  },
]

/** 进阶设置：8 类 / 21 组 */
export const ADVANCED_SECTIONS: SettingsSection[] = [
  {
    id: 'adv-retrieval',
    label: '检索与索引',
    cards: [
      { id: 'retrieval', kind: 'schema', span: 12 },
      { id: 'index', kind: 'schema', span: 12 },
    ],
  },
  {
    id: 'adv-model',
    label: '模型生成',
    cards: [{ id: 'model-generation', kind: 'schema', span: 12 }],
  },
  {
    id: 'adv-memory',
    label: '记忆管线',
    cards: [
      { id: 'decay', kind: 'schema', span: 6 },
      { id: 'thresholds', kind: 'schema', span: 6 },
      { id: 'event-extraction', kind: 'schema', span: 6 },
      { id: 'l1-progressive', kind: 'schema', span: 6 },
    ],
  },
  {
    id: 'adv-inject',
    label: '注入与装配',
    cards: [
      { id: 'utt', kind: 'schema', span: 6 },
      { id: 'injection-budget', kind: 'schema', span: 6 },
      { id: 'examples', kind: 'schema', span: 6 },
      { id: 'bridge', kind: 'schema', span: 6 },
      { id: 'layer-dedup', kind: 'schema', span: 12 },
    ],
  },
  {
    id: 'adv-persona',
    label: '人格与知识',
    cards: [
      { id: 'inference', kind: 'schema', span: 12 },
      { id: 'knowledge', kind: 'schema', span: 12 },
      { id: 'style', kind: 'schema', span: 12 },
    ],
  },
  {
    id: 'adv-proactive',
    label: '主动对话',
    cards: [
      { id: 'proactive-judge', kind: 'schema', span: 12 },
      { id: 'proactive-rhythm', kind: 'schema', span: 7 },
      { id: 'proactive-greeting', kind: 'schema', span: 5 },
    ],
  },
  {
    id: 'adv-behavior',
    label: '行为与规则',
    cards: [{ id: 'behavior-feedback', kind: 'schema', span: 12 }],
  },
  {
    id: 'adv-system',
    label: '系统',
    cards: [
      { id: 'cache', kind: 'schema', span: 12 },
      { id: 'logging', kind: 'schema', span: 12 },
    ],
  },
]

const SECTIONS: Record<SettingsScope, SettingsSection[]> = {
  basic: BASIC_SECTIONS,
  advanced: ADVANCED_SECTIONS,
}

/** 范围的全部子页 */
export function sectionsOf(scope: SettingsScope): SettingsSection[] {
  return SECTIONS[scope]
}

/** 默认子页（基础「模型与检索」/ 进阶「检索与索引」） */
export function defaultSectionId(scope: SettingsScope): string {
  return SECTIONS[scope][0]?.id ?? 'model'
}

/** 按范围与 id 查子页；未知 id 返回 undefined */
export function findSection(scope: SettingsScope, id: string): SettingsSection | undefined {
  return SECTIONS[scope].find((section) => section.id === id)
}

/** 校验范围字符串 */
export function isSettingsScope(value: string): value is SettingsScope {
  return value === 'basic' || value === 'advanced'
}
