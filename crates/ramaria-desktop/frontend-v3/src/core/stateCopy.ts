/**
 * src/core/stateCopy.ts — 状态文案单一源
 *
 * 职责:
 * - 集中视图级状态文案：键为 `{view}.{state}.{reason}`
 * - 每条含标题（≤10 字）、说明（≤2 行）、动作（0 或 1 个）
 *
 * 设计特点:
 * - 视图只引用键，不写死文案；同一原因全应用只在一处定义
 * - 错误文案必答「发生了什么 + 不阻塞什么」，不出现「加载失败」这类空话
 * - 降级文案必须写明仍可用的能力，否则按错误处理
 */

/** 状态文案模板 */
export interface StateCopy {
  title: string
  description: string
  action?: string
}

const COPY: Record<string, StateCopy> = {
  // ---- chat ----
  'chat.empty.no-data': {
    title: '还没有消息',
    description: '从一句问候开始，或者点击头像打开会话列表继续之前的对话。',
  },
  'chat.error.load': {
    title: '会话加载失败',
    description: '读取本地数据库时出错，其余功能不受影响。',
    action: '重试',
  },
  'chat.degraded.index.idle': {
    title: '索引未建立，召回降级',
    description: '可继续对话，召回暂用关键词与图谱。',
    action: '开始构建',
  },
  'chat.degraded.index.building': {
    title: '索引构建中',
    description: '可继续对话，召回暂用关键词与图谱。',
    action: '查看进度',
  },
  'chat.degraded.index.failed': {
    title: '索引构建失败',
    description: '记忆与对话仍可用，召回范围较小。',
    action: '重试构建',
  },
  'chat.degraded.index.partial': {
    title: '部分向量缺失',
    description: '少数记忆暂不参与语义召回，可重建补齐。',
    action: '重建索引',
  },
  'chat.degraded.llm.unavailable': {
    title: '对话模型不可用',
    description: '记忆浏览不受影响。',
    action: '去设置',
  },
  'chat.degraded.privacy': {
    title: '需要确认隐私范围',
    description: '使用线上后端前需确认数据范围与去向。',
    action: '去确认',
  },
  'chat.restricted.readonly': {
    title: '此会话来自社交平台，只读',
    description: '此处只可查看，回复请前往对应平台。',
    action: '开始新对话',
  },
  'chat.restricted.locked': {
    title: '正在生成',
    description: '输入区暂不可用，生成完成后恢复。',
  },

  // ---- memory ----
  'memory.empty.no-data': {
    title: '这里还没有记忆',
    description: '对话结束后会自动生成摘要；也可以导入历史聊天记录。',
    action: '去导入',
  },
  'memory.error.load': {
    title: '记忆加载失败',
    description: '读取本地数据库时出错，其余功能不受影响。',
    action: '重试',
  },
  'memory.degraded.index': {
    title: '索引尚未就绪',
    description: '已形成的记忆可正常浏览；对话召回要等索引完成后才包含最新记忆。',
    action: '去设置',
  },

  // ---- persona ----
  'persona.empty.no-data': {
    title: '还没有人格',
    description: '导入一个预设，或先创建默认人格。',
    action: '导入预设',
  },
  'persona.error.load': {
    title: '人格列表加载失败',
    description: '读取本地数据库时出错，其余功能不受影响。',
    action: '重试',
  },
  'persona.degraded.profile': {
    title: '画像尚未生成',
    description: '该人格还没有足够的记忆事件，重载画像可尝试重算。',
    action: '重载画像',
  },
  'persona.degraded.llm': {
    title: '对话模型不可用',
    description: '重载画像与全量提示词预览暂不可用，编辑仍可保存。',
    action: '去设置',
  },

  // ---- import ----
  'import.empty.no-data': {
    title: '还没有选择文件',
    description: '支持 QQ 聊天记录导出文件。',
    action: '选择文件',
  },
  'import.error.parse': {
    title: '文件解析失败',
    description: '没有写入任何数据；可换文件或排除问题后重试。',
    action: '重试',
  },
  'import.error.execute': {
    title: '导入未完成',
    description: '已写入的数据会保留；可重试或更换文件。',
    action: '重试',
  },
  'import.degraded.skipped': {
    title: '部分处理未完成',
    description: '附件或摘要没有全部生成，可继续处理补齐。',
    action: '继续处理',
  },
  'import.restricted.locked': {
    title: '正在处理',
    description: '导入进行中，参数与步骤暂不可改。',
  },

  // ---- settings ----
  'settings.error.load': {
    title: '设置加载失败',
    description: '读取本地配置时出错，其余功能不受影响；可重试或稍后再进。',
    action: '重试',
  },
  'settings.empty.roster': {
    title: '还没有可配置的人格',
    description: '创建或导入人格后，可在这里设置主动消息开关。',
    action: '去人格',
  },

  // ---- rules ----
  'rules.empty.no-data': {
    title: '还没有规则',
    description: '规则随对话与画像推断自动生成。',
    action: '去对话',
  },
  'rules.error.load': {
    title: '规则加载失败',
    description: '读取本地数据库时出错，其余功能不受影响。',
    action: '重试',
  },
  'rules.degraded.llm': {
    title: '对话模型不可用',
    description: '规则自动抽取暂停，编辑与启停仍可用。',
  },

  // ---- 全局降级 ----
  'global.degraded.embedding_missing': {
    title: '嵌入模型不可用',
    description: '语义检索关闭，浏览与对话不受影响。',
    action: '去设置',
  },
  'global.degraded.llm_unavailable': {
    title: '对话模型不可用',
    description: '记忆浏览不受影响。',
    action: '去设置',
  },

  // ---- 全屏页 ----
  'setup.error.connection': {
    title: '连接测试未通过',
    description: '检查本地服务是否已启动，或稍后重试。',
    action: '重试',
  },
  'setup.error.embedding': {
    title: '嵌入模型校验未通过',
    description: '路径无效或文件不完整，可跳过并稍后在设置中补配。',
    action: '重试',
  },
  'setup.error.submit': {
    title: '配置未保存',
    description: '写入本地配置时出错，已填内容不会丢失，可直接重试。',
    action: '重试',
  },
  'error.detail': {
    title: '无法继续',
    description: '启动检查未通过，可重试；失败详情可在诊断包中查看。',
    action: '重试',
  },
}

/** 按键取文案；键缺失时返回兜底文案，避免界面空白 */
export function stateCopy(key: string, fallback?: StateCopy): StateCopy {
  const hit = COPY[key]
  if (hit) {
    return hit
  }
  return fallback ?? { title: '暂不可用', description: '稍后重试即可，其余功能不受影响。' }
}

/** 全部已登记键（供状态画廊与校验使用） */
export function stateCopyKeys(): string[] {
  return Object.keys(COPY)
}
