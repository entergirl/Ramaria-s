/**
 * src/views/dev/gallery/catalog.ts — 状态画廊的格位目录
 *
 * 职责:
 * - 声明基础组件与契约视图的全部状态格，供画廊渲染与覆盖度断言共用
 *
 * 设计特点:
 * - 目录是画廊的唯一事实源：格位编号、中文状态名与触发方式都在此定义，渲染器只按 `slug` 取参数
 * - 每个格位带组件矩阵行号与验收编号，便于逐格勾选
 * - 视图格位区分里程碑归属，未实现视图的格位以载体示意呈现
 */

import type { ChatBannerKind } from '../../chat/useChatBanner'

/** 格位所属渲染族：控制件 / 内容件 / 反馈件 */
export type ComponentGroup = 'control' | 'content' | 'feedback'

/** 视图格的载体类型 */
export type ViewCarrier = 'default' | 'loading' | 'empty' | 'error' | 'degraded' | 'restricted'

/** 单个格位 */
export interface GalleryCell {
  /** 全局唯一标识，形如 `button-hover` */
  id: string
  /** 行内状态短名，渲染器据此取参数 */
  slug: string
  /** 中文状态名，用词与组件矩阵一致 */
  label: string
  /** 交互态触发方式；静态可复现的格位留空 */
  trigger?: string
  /** 补充说明（同族另有形态、不适用原因等） */
  note?: string
}

/** 视图格位：在通用格位之上带载体信息 */
export interface ViewCell extends GalleryCell {
  carrier: ViewCarrier
  /** 载体细分：空态归因 / 降级来源 / 受限型 / 错误层级 */
  variant?: string
  /** 降级条的横幅种类；无横幅的视图走卡内标记 */
  banner?: ChatBannerKind
  /** 横幅附带进度 */
  percent?: number
}

/** 组件行 */
export interface ComponentRow {
  /** 组件矩阵行号 */
  no: number
  key: string
  name: string
  /** 验收编号 */
  qa: string
  group: ComponentGroup
  /** 该行的硬规则 */
  rule: string
  cells: GalleryCell[]
}

/** 视图行 */
export interface ViewRow {
  key: string
  name: string
  qa: string
  milestone: 'M5' | 'M6' | 'M7'
  /** 该视图状态的关键判据 */
  rule: string
  cells: ViewCell[]
}

/** 构造格位，统一拼接全局标识 */
function cell(row: string, slug: string, label: string, extra: Partial<GalleryCell> = {}): GalleryCell {
  return { id: `${row}-${slug}`, slug, label, ...extra }
}

/** 构造视图格位 */
function viewCell(
  view: string,
  slug: string,
  label: string,
  carrier: ViewCarrier,
  extra: Partial<ViewCell> = {},
): ViewCell {
  return { id: `${view}-${slug}`, slug, label, carrier, ...extra }
}

/** 交互态触发说明 */
const HOVER = '指针悬停'
const ACTIVE = '按住不放'
const FOCUS = 'Tab 键聚焦'

/** 组件状态矩阵（15 行 / 86 格） */
export const COMPONENT_ROWS: ComponentRow[] = [
  {
    no: 1,
    key: 'button',
    name: 'Button',
    qa: 'QA-C-01',
    group: 'control',
    rule: '加载不降透明度、宽度不跳；一屏一主按钮；图标按钮宽 = 高',
    cells: [
      cell('button', 'default', '默认'),
      cell('button', 'hover', '悬停', { trigger: HOVER }),
      cell('button', 'active', '按压', { trigger: ACTIVE }),
      cell('button', 'focus', '聚焦', { trigger: FOCUS }),
      cell('button', 'disabled', '禁用'),
      cell('button', 'loading', '加载'),
    ],
  },
  {
    no: 2,
    key: 'input',
    name: 'Input',
    qa: 'QA-C-02',
    group: 'control',
    rule: '标签外置；错误文案紧贴字段；只读数据用静态文本',
    cells: [
      cell('input', 'default', '默认'),
      cell('input', 'hover', '悬停', { trigger: HOVER }),
      cell('input', 'focus', '聚焦', { trigger: FOCUS }),
      cell('input', 'disabled', '禁用'),
      cell('input', 'invalid', '无效'),
      cell('input', 'validating', '校验中'),
    ],
  },
  {
    no: 3,
    key: 'select',
    name: 'Select',
    qa: 'QA-C-03',
    group: 'control',
    rule: '空选项写「暂无可选项」；已选值不得截断到不可读',
    cells: [
      cell('select', 'default', '默认'),
      cell('select', 'hover', '悬停', { trigger: HOVER }),
      cell('select', 'focus', '聚焦', { trigger: FOCUS }),
      cell('select', 'open', '展开', { trigger: '自动展开', note: 'Esc 或点外部收起' }),
      cell('select', 'disabled', '禁用'),
      cell('select', 'invalid', '无效'),
      cell('select', 'selected', '选项选中'),
    ],
  },
  {
    no: 4,
    key: 'switch',
    name: 'Switch',
    qa: 'QA-C-04',
    group: 'control',
    rule: '写入中锁定并给行内指示；失败回滚加提示',
    cells: [
      cell('switch', 'off', '关'),
      cell('switch', 'on', '开'),
      cell('switch', 'hover', '悬停', { trigger: HOVER }),
      cell('switch', 'focus', '聚焦', { trigger: FOCUS }),
      cell('switch', 'disabled', '禁用'),
      cell('switch', 'saving', '写入中'),
    ],
  },
  {
    no: 5,
    key: 'checkbox',
    name: 'Checkbox',
    qa: 'QA-C-05',
    group: 'control',
    rule: '标签整体可点；整组禁用时标题同步降级',
    cells: [
      cell('checkbox', 'unchecked', '未选'),
      cell('checkbox', 'checked', '已选'),
      cell('checkbox', 'indeterminate', '半选'),
      cell('checkbox', 'hover', '悬停', { trigger: HOVER }),
      cell('checkbox', 'focus', '聚焦', { trigger: FOCUS }),
      cell('checkbox', 'disabled', '禁用'),
      cell('checkbox', 'invalid', '无效'),
    ],
  },
  {
    no: 6,
    key: 'radio',
    name: 'Radio',
    qa: 'QA-C-06',
    group: 'control',
    rule: '组内方向键切换；单选用途不用下拉替代',
    cells: [
      cell('radio', 'unchecked', '未选'),
      cell('radio', 'checked', '已选'),
      cell('radio', 'hover', '悬停', { trigger: HOVER }),
      cell('radio', 'focus', '聚焦', { trigger: FOCUS }),
      cell('radio', 'disabled', '禁用'),
      cell('radio', 'invalid', '无效'),
    ],
  },
  {
    no: 7,
    key: 'segmented',
    name: 'Segmented',
    qa: 'QA-C-07',
    group: 'control',
    rule: '2~5 段不换行；选中白底加品牌描边',
    cells: [
      cell('segmented', 'default', '默认'),
      cell('segmented', 'selected', '选中'),
      cell('segmented', 'hover', '悬停', { trigger: HOVER }),
      cell('segmented', 'focus', '聚焦', { trigger: FOCUS }),
      cell('segmented', 'disabled', '禁用'),
    ],
  },
  {
    no: 8,
    key: 'tag',
    name: 'Tag',
    qa: 'QA-C-08',
    group: 'control',
    rule: '除筛选与移除不承载操作；文字不超过六字；圆角 8',
    cells: [
      cell('tag', 'static', '静态'),
      cell('tag', 'selected', '选中'),
      cell('tag', 'remove-hover', '可移除悬停', { trigger: '指针悬停移除键' }),
      cell('tag', 'remove-active', '可移除按压', { trigger: '按住移除键' }),
    ],
  },
  {
    no: 9,
    key: 'card',
    name: 'Card',
    qa: 'QA-C-09',
    group: 'content',
    rule: '不可点击卡不得有悬停；错误留在卡内；未保存仅表单类卡',
    cells: [
      cell('card', 'default', '默认'),
      cell('card', 'hover', '可点击悬停', { trigger: HOVER }),
      cell('card', 'selected', '选中'),
      cell('card', 'loading', '加载'),
      cell('card', 'error', '错误'),
      cell('card', 'dirty', '未保存'),
      cell('card', 'dimmed', '变暗'),
    ],
  },
  {
    no: 10,
    key: 'bubble',
    name: 'Bubble',
    qa: 'QA-C-10',
    group: 'content',
    rule: '撤回替换为系统条；失败给重试；不改变对齐',
    cells: [
      cell('bubble', 'user', '用户'),
      cell('bubble', 'assistant', '助手'),
      cell('bubble', 'system', '系统'),
      cell('bubble', 'streaming', '流式'),
      cell('bubble', 'recalled', '撤回'),
      cell('bubble', 'proactive', '主动'),
      cell('bubble', 'failed', '失败'),
      cell('bubble', 'group', '群聊', { note: '发言人区独立于正文' }),
    ],
  },
  {
    no: 11,
    key: 'modal',
    name: 'Modal',
    qa: 'QA-C-11',
    group: 'feedback',
    rule: '永不禁止 Esc 与关闭；焦点陷阱；危险操作二次确认',
    cells: [
      cell('modal', 'open', '打开', { trigger: '点击打开预览' }),
      cell('modal', 'closing', '关闭中', { trigger: '点击打开后关闭', note: '观察过渡与焦点归位' }),
      cell('modal', 'loading', '加载', { trigger: '点击打开预览' }),
      cell('modal', 'danger', '危险', { trigger: '点击打开预览' }),
      cell('modal', 'scroll', '滚动', { trigger: '点击打开预览', note: '正文在框内滚动' }),
    ],
  },
  {
    no: 12,
    key: 'toast',
    name: 'Toast',
    qa: 'QA-C-12',
    group: 'feedback',
    rule: '不超过两行三条；错误不自动消失；视图级错误不进提示',
    cells: [
      cell('toast', 'info', '信息'),
      cell('toast', 'success', '成功'),
      cell('toast', 'warn', '警告'),
      cell('toast', 'danger', '错误'),
      cell('toast', 'action', '带操作'),
    ],
  },
  {
    no: 13,
    key: 'progress',
    name: 'Progress',
    qa: 'QA-C-13',
    group: 'feedback',
    rule: '必须带数值或阶段；终态两秒后收起或转内联',
    cells: [
      cell('progress', 'determinate', '确定'),
      cell('progress', 'indeterminate', '不定'),
      cell('progress', 'success', '成功'),
      cell('progress', 'failed', '失败'),
      cell('progress', 'timeout', '超时'),
    ],
  },
  {
    no: 14,
    key: 'skeleton',
    name: 'Skeleton',
    qa: 'QA-C-14',
    group: 'feedback',
    rule: '形状匹配真实尺寸；列表最多三张卡骨架；减弱动效转静态',
    cells: [
      cell('skeleton', 'text', '文本'),
      cell('skeleton', 'avatar', '头像'),
      cell('skeleton', 'card', '卡片'),
      cell('skeleton', 'bubble', '气泡'),
      cell('skeleton', 'table', '表格'),
      cell('skeleton', 'static', '静态', { note: '减弱动效下的静止形态' }),
    ],
  },
  {
    no: 15,
    key: 'empty',
    name: 'EmptyState',
    qa: 'QA-C-15',
    group: 'feedback',
    rule: '最多一个主操作；说明不超过两行；图形用内联图形或单字标记',
    cells: [
      cell('empty', 'guide', '引导'),
      cell('empty', 'action', '操作'),
      cell('empty', 'blocked', '受阻'),
    ],
  },
]

/** 视图六态（9 行 / 60 格） */
export const VIEW_ROWS: ViewRow[] = [
  {
    key: 'chat',
    name: '对话',
    qa: 'QA-V-chat',
    milestone: 'M5',
    rule: '空态无按钮；只读隐藏输入并只留一个动作；流式无横幅',
    cells: [
      viewCell('chat', 'default', '默认', 'default'),
      viewCell('chat', 'loading', '加载', 'loading'),
      viewCell('chat', 'empty', '空', 'empty', { variant: 'no-data' }),
      viewCell('chat', 'error', '错误', 'error', { variant: 'main' }),
      viewCell('chat', 'degraded-index', '降级·index', 'degraded', {
        variant: 'index',
        banner: 'index-building',
        percent: 42,
        note: '另含未建立 / 失败 / 部分缺失三态',
      }),
      viewCell('chat', 'degraded-llm', '降级·llm', 'degraded', {
        variant: 'llm',
        banner: 'llm',
      }),
      viewCell('chat', 'degraded-privacy', '降级·privacy', 'degraded', {
        variant: 'privacy',
        banner: 'privacy',
      }),
      viewCell('chat', 'restricted-readonly', '受限·readonly', 'restricted', {
        variant: 'readonly',
        banner: 'readonly',
      }),
      viewCell('chat', 'restricted-locked', '受限·locked', 'restricted', { variant: 'locked' }),
    ],
  },
  {
    key: 'memory',
    name: '记忆',
    qa: 'QA-V-memory',
    milestone: 'M6',
    rule: '切页签保留旧内容；切人格清空；工具栏在错误态仍可用',
    cells: [
      viewCell('memory', 'default', '默认', 'default'),
      viewCell('memory', 'loading', '加载', 'loading'),
      viewCell('memory', 'empty-no-data', '空·no-data', 'empty', { variant: 'no-data' }),
      viewCell('memory', 'empty-filter', '空·filter', 'empty', { variant: 'filter' }),
      viewCell('memory', 'empty-capability', '空·capability', 'empty', { variant: 'capability' }),
      viewCell('memory', 'error', '错误', 'error', { variant: 'main' }),
      viewCell('memory', 'degraded-index', '降级·index', 'degraded', {
        variant: 'index',
        note: '卡内标记，不占横幅',
      }),
      viewCell('memory', 'degraded-embedding', '降级·embedding', 'degraded', {
        variant: 'embedding',
        note: '搜索不可用说明',
      }),
    ],
  },
  {
    key: 'persona',
    name: '人格',
    qa: 'QA-V-persona',
    milestone: 'M6',
    rule: '空态给导入入口；预览只读；导入执行中锁定',
    cells: [
      viewCell('persona', 'default', '默认', 'default'),
      viewCell('persona', 'loading', '加载', 'loading'),
      viewCell('persona', 'empty', '空', 'empty', { variant: 'no-data' }),
      viewCell('persona', 'error', '错误', 'error', { variant: 'main' }),
      viewCell('persona', 'degraded-profile', '降级·profile', 'degraded', {
        variant: 'profile',
        note: '画像未生成',
      }),
      viewCell('persona', 'degraded-llm', '降级·llm', 'degraded', {
        variant: 'llm',
        note: '重载画像禁用',
      }),
      viewCell('persona', 'restricted-import', '受限·import-locked', 'restricted', {
        variant: 'locked',
      }),
      viewCell('persona', 'restricted-preview', '受限·preview-readonly', 'restricted', {
        variant: 'readonly',
        note: '预览层天然只读',
      }),
    ],
  },
  {
    key: 'rules',
    name: '规则',
    qa: 'QA-V-rules',
    milestone: 'M6',
    rule: '无删除动作；候选规则标「仅参数注入」；人工编辑不依赖模型',
    cells: [
      viewCell('rules', 'default', '默认', 'default'),
      viewCell('rules', 'loading', '加载', 'loading'),
      viewCell('rules', 'empty', '空', 'empty', { variant: 'no-data' }),
      viewCell('rules', 'error', '错误', 'error', { variant: 'main' }),
      viewCell('rules', 'degraded-llm', '降级·llm', 'degraded', {
        variant: 'llm',
        note: '重新推断禁用，编辑仍可用',
      }),
    ],
  },
  {
    key: 'import',
    name: '导入',
    qa: 'QA-V-import',
    milestone: 'M6',
    rule: '阶段加百分比与预计时间；离开不中断；执行中锁步骤',
    cells: [
      viewCell('import', 'default', '默认', 'default'),
      viewCell('import', 'loading-analyze', '加载·分析', 'loading', { variant: 'analyze' }),
      viewCell('import', 'loading-execute', '加载·执行', 'loading', { variant: 'execute' }),
      viewCell('import', 'empty-unselected', '空·未选', 'empty', { variant: 'no-data' }),
      viewCell('import', 'error-parse', '错误·解析', 'error', { variant: 'parse' }),
      viewCell('import', 'error-execute', '错误·执行', 'error', { variant: 'execute' }),
      viewCell('import', 'degraded-skipped', '降级·skipped/L1', 'degraded', {
        variant: 'skipped',
        banner: 'l1',
      }),
      viewCell('import', 'restricted-locked', '受限·locked', 'restricted', { variant: 'locked' }),
    ],
  },
  {
    key: 'settings',
    name: '设置',
    qa: 'QA-V-settings',
    milestone: 'M7',
    rule: '单卡失败不阻塞其余；降级走卡内标记；凭据只读',
    cells: [
      viewCell('settings', 'default', '默认', 'default'),
      viewCell('settings', 'loading', '加载', 'loading'),
      viewCell('settings', 'empty-partial', '空·局部', 'empty', { variant: 'no-data' }),
      viewCell('settings', 'error-main', '错误·主请求', 'error', { variant: 'main' }),
      viewCell('settings', 'error-card', '错误·单卡', 'error', { variant: 'card' }),
      viewCell('settings', 'degraded-embedding', '降级·embedding', 'degraded', {
        variant: 'embedding',
      }),
      viewCell('settings', 'degraded-llm', '降级·llm', 'degraded', { variant: 'llm' }),
      viewCell('settings', 'degraded-index', '降级·index', 'degraded', { variant: 'index' }),
      viewCell('settings', 'degraded-partial', '降级·partial', 'degraded', { variant: 'partial' }),
      viewCell('settings', 'restricted-readonly', '受限·readonly', 'restricted', {
        variant: 'readonly',
        note: '凭据只读展示',
      }),
      viewCell('settings', 'restricted-locked', '受限·locked', 'restricted', { variant: 'locked' }),
    ],
  },
  {
    key: 'debug',
    name: '调试',
    qa: 'QA-V-debug',
    milestone: 'M7',
    rule: '天然只读不显示横幅；未解锁不进视图',
    cells: [
      viewCell('debug', 'default', '默认', 'default'),
      viewCell('debug', 'loading', '加载', 'loading'),
      viewCell('debug', 'empty', '空', 'empty', { variant: 'no-data' }),
      viewCell('debug', 'error', '错误', 'error', { variant: 'main' }),
      viewCell('debug', 'degraded-dir', '降级·dir', 'degraded', {
        variant: 'dir',
        note: '评估目录不可读',
      }),
    ],
  },
  {
    key: 'setup',
    name: '首次配置',
    qa: 'QA-V-setup',
    milestone: 'M5',
    rule: '三步可回退；提交中锁定；卡片全程不滚动',
    cells: [
      viewCell('setup', 'default', '默认', 'default'),
      viewCell('setup', 'loading', '加载', 'loading', { variant: 'button' }),
      viewCell('setup', 'error', '错误', 'error', { variant: 'inline' }),
      viewCell('setup', 'restricted-locked', '受限', 'restricted', { variant: 'locked' }),
    ],
  },
  {
    key: 'error',
    name: '错误页',
    qa: 'QA-V-error',
    milestone: 'M5',
    rule: '错误码加一句原因，动作不超过两个；重试有加载态',
    cells: [
      viewCell('error', 'detail', '错误详情', 'error', { variant: 'detail' }),
      viewCell('error', 'retrying', '重试中', 'error', { variant: 'retrying' }),
    ],
  },
]

/** 组件格总数 */
export const COMPONENT_CELL_COUNT = COMPONENT_ROWS.reduce((sum, row) => sum + row.cells.length, 0)

/** 视图格总数 */
export const VIEW_CELL_COUNT = VIEW_ROWS.reduce((sum, row) => sum + row.cells.length, 0)

/** 全部格位标识（含视图格），用于唯一性断言 */
export function allCellIds(): string[] {
  return [
    ...COMPONENT_ROWS.flatMap((row) => row.cells.map((item) => item.id)),
    ...VIEW_ROWS.flatMap((row) => row.cells.map((item) => item.id)),
  ]
}
