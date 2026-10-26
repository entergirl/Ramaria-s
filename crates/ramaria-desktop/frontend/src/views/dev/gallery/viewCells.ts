/**
 * src/views/dev/gallery/viewCells.ts — 视图缩略的载体参数与文案
 *
 * 职责:
 * - 给出每个视图的骨架形状与空态形态，并为格位解析展示文案
 *
 * 设计特点:
 * - 已登记状态文案的视图直接读 `stateCopy`，与实机完全同源
 * - 未登记文案的视图只给载体级说明，不提前发明视图文案
 * - 文案按「载体 + 细分」解析，同一载体在不同视图下可给不同说明
 */

import { stateCopy } from '../../../core/stateCopy'
import type { ViewCell } from './catalog'

/** 载体参数 */
export interface ThumbSpec {
  /** 加载态骨架形状 */
  skeleton: 'text' | 'avatar' | 'card' | 'bubble' | 'table'
  /** 空态默认形态 */
  emptyVariant: 'guide' | 'action' | 'blocked'
}

/** 缩略文案 */
export interface ThumbCopy {
  title: string
  description: string
  action?: string
}

const SPECS: Record<string, ThumbSpec> = {
  chat: { skeleton: 'bubble', emptyVariant: 'guide' },
  memory: { skeleton: 'card', emptyVariant: 'action' },
  persona: { skeleton: 'card', emptyVariant: 'action' },
  rules: { skeleton: 'card', emptyVariant: 'guide' },
  import: { skeleton: 'table', emptyVariant: 'action' },
  settings: { skeleton: 'card', emptyVariant: 'guide' },
  debug: { skeleton: 'table', emptyVariant: 'action' },
  setup: { skeleton: 'card', emptyVariant: 'guide' },
  error: { skeleton: 'text', emptyVariant: 'blocked' },
}

/** 已登记到状态文案表的格位 */
const REGISTERED_KEYS: Record<string, string> = {
  'chat:empty:no-data': 'chat.empty.no-data',
  'chat:error:main': 'chat.error.load',
  'chat:degraded:index': 'chat.degraded.index.building',
  'chat:degraded:llm': 'chat.degraded.llm.unavailable',
  'chat:degraded:privacy': 'chat.degraded.privacy',
  'chat:restricted:readonly': 'chat.restricted.readonly',
  'chat:restricted:locked': 'chat.restricted.locked',
  'memory:empty:no-data': 'memory.empty.no-data',
  'memory:error:main': 'memory.error.load',
  'persona:empty:no-data': 'persona.empty.no-data',
  'persona:error:main': 'persona.error.load',
  'persona:degraded:profile': 'persona.degraded.profile',
  'persona:degraded:llm': 'persona.degraded.llm',
  'rules:empty:no-data': 'rules.empty.no-data',
  'rules:error:main': 'rules.error.load',
  'rules:degraded:llm': 'rules.degraded.llm',
  'import:empty:no-data': 'import.empty.no-data',
  'import:error:parse': 'import.error.parse',
  'import:error:execute': 'import.error.execute',
  'import:degraded:skipped': 'import.degraded.skipped',
  'import:restricted:locked': 'import.restricted.locked',
  'setup:error:inline': 'setup.error.connection',
  'error:error:detail': 'error.detail',
  'error:error:retrying': 'error.detail',
}

/** 载体级说明：不替代视图文案 */
const CARRIER_COPY: Record<string, ThumbCopy> = {
  default: { title: '内容区', description: '按视图真实内容呈现，缩略只示意版式与密度。' },
  'loading:': { title: '加载中', description: '骨架形状与真实内容一致，切页签保留旧内容。' },
  'empty:no-data': {
    title: '暂无数据',
    description: '引导型空态：说明为什么空，最多给一个主操作。',
  },
  'empty:filter': {
    title: '没有匹配结果',
    description: '筛选型空态：给「清除筛选」，不写「暂无数据」。',
  },
  'empty:capability': {
    title: '能力不可用',
    description: '能力型空态：写清原因并给「去设置」。',
  },
  'error:main': { title: '请求失败', description: '错误块：发生了什么 + 一句原因 + 重试。', action: '重试' },
  'error:card': { title: '这个区块读取失败', description: '卡内错误条：其余区块照常可用。', action: '重试' },
  'error:parse': { title: '文件解析失败', description: '写明停在哪个阶段，可换文件或重试。', action: '换文件' },
  'error:execute': { title: '导入未完成', description: '已写入的内容保留，可续跑剩余部分。', action: '继续处理' },
  'error:inline': { title: '连接测试未通过', description: '步骤内错误条：一句原因 + 重试。', action: '重试' },
  'error:detail': { title: '无法继续', description: '错误码 + 一句原因 + 至多两个动作。', action: '重试' },
  'error:retrying': { title: '无法继续', description: '重试按钮进入加载态，不改变版式。', action: '重试' },
  'degraded:index': { title: '索引构建中', description: '卡内标记：召回降级但可继续对话。', action: '查看进度' },
  'degraded:embedding': { title: '语义检索关闭', description: '卡内标记：浏览与对话不受影响。', action: '去设置' },
  'degraded:llm': { title: '对话模型不可用', description: '依赖模型的入口禁用，读取仍可用。', action: '去设置' },
  'degraded:profile': { title: '画像尚未生成', description: '卡内标记：可手动重载画像。', action: '重载' },
  'degraded:privacy': { title: '需要确认隐私范围', description: '横幅：说明数据范围与去向。', action: '去确认' },
  'degraded:partial': { title: '部分向量缺失', description: '卡内标记：少数记忆暂不参与语义召回。', action: '重建索引' },
  'degraded:skipped': { title: '部分内容未处理', description: '横幅：附件或摘要失败，可继续处理。', action: '继续处理' },
  'degraded:dir': { title: '评估目录不可读', description: '卡内标记：请重新选择目录。', action: '选择目录' },
  'restricted:readonly': {
    title: '只读',
    description: '横幅 + 隐藏输入 + 底部只读条，只留一个动作。',
  },
  'restricted:locked': {
    title: '处理中',
    description: '控件禁用，不叠加第二个主操作。',
  },
}

/** 取视图的载体参数 */
export function viewThumbSpec(view: string): ThumbSpec {
  return SPECS[view] ?? { skeleton: 'card', emptyVariant: 'guide' }
}

/** 解析格位文案：已登记的读状态文案表，其余走载体级说明 */
export function viewThumbCopy(view: string, cell: ViewCell): ThumbCopy {
  const registered = REGISTERED_KEYS[`${view}:${cell.carrier}:${cell.variant ?? ''}`]
  if (registered) {
    const copy = stateCopy(registered)
    return {
      title: copy.title,
      description: copy.description,
      action: copy.action,
    }
  }
  const key = `${cell.carrier}:${cell.variant ?? ''}`
  return CARRIER_COPY[key] ?? CARRIER_COPY[cell.carrier] ?? CARRIER_COPY.default
}
