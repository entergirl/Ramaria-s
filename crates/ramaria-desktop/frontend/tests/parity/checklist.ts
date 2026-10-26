/**
 * tests/parity/checklist.ts — 视图 A / B 的 parity 清单（内容 / 操作 / 五态）
 *
 * 职责:
 * - 单一源声明 memory / persona / rules / import / debug 五视图该有什么内容、能做哪些操作、覆盖哪些状态
 * - 每条声明绑定覆盖它的测试文件与用例名，供 checklist.test.ts 机器校验引用不漂移
 *
 * 设计特点:
 * - 状态条目的 `cell` 与 `design-qa.md` §9 的格名逐字一致，两份文档由测试对齐
 * - 不适用的格写 `na` 并给依据，不允许留空
 * - 内容 / 操作条目只写可观察行为，不写实现细节
 */

/** 参与 parity 的视图标识 */
export type ParityViewId = 'memory' | 'persona' | 'rules' | 'import' | 'debug'

/** 清单维度 */
export type ParityDimension = 'content' | 'operation' | 'state'

/** 条目状态：已覆盖 / 不适用 */
export type ParityEntryStatus = 'covered' | 'na'

/** 覆盖用例所在文件与用例名（文件名相对 `tests/`） */
export interface ParityCoverage {
  file: string
  cases: string[]
}

export interface ParityEntry {
  /** 条目编号：`{view}-{dimension}-{序号}` */
  id: string
  dimension: ParityDimension
  /** 声明：该视图在此维度上具备什么 */
  claim: string
  status: ParityEntryStatus
  /** 仅状态条目：与 `design-qa.md` 的格名一致 */
  cell?: string
  /** 已覆盖条目的用例落点 */
  coverage?: ParityCoverage
  /** 不适用条目的依据 */
  naReason?: string
}

export interface ParityViewChecklist {
  view: ParityViewId
  /** `design-qa.md` §9 的验收编号 */
  qaId: string
  entries: ParityEntry[]
}

const MEMORY: ParityViewChecklist = {
  view: 'memory',
  qaId: 'QA-V-memory',
  entries: [
    {
      id: 'memory-content-1',
      dimension: 'content',
      claim: '四页签（L1 摘要 / L2 事件 / L3 性格 / 知识）各带计数徽标',
      status: 'covered',
      coverage: { file: 'memoryView.test.ts', cases: ['首页渲染 L1 卡片、页签计数与视图主态'] },
    },
    {
      id: 'memory-content-2',
      dimension: 'content',
      claim: '人格卡显示名称、类型标签与默认标记，抽屉可切换人格',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: [
          '首页渲染 L1 卡片、页签计数与视图主态',
          '切换人格重新取数，深度处理入口只对导入人格出现',
        ],
      },
    },
    {
      id: 'memory-content-3',
      dimension: 'content',
      claim: 'L1 卡显示摘要、关键词、氛围、效价、显著度、参与者与消息数',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['首页渲染 L1 卡片、页签计数与视图主态'],
      },
    },
    {
      id: 'memory-content-4',
      dimension: 'content',
      claim: 'L1 时间取发生时间，悬停补入库时间',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['L1 时间以发生时间为准，悬停补入库时间'],
      },
    },
    {
      id: 'memory-content-5',
      dimension: 'content',
      claim: 'L2 卡显示标题、摘要、置信度、共享度与态度',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['切换页签复用已装载数据，不再请求'],
      },
    },
    {
      id: 'memory-content-6',
      dimension: 'content',
      claim: 'L3 按底色层 / 主色调层 / 点缀层渲染三层画像并给出画像状态',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['L3 渲染三层画像，展开证据链并重载画像'],
      },
    },
    {
      id: 'memory-content-7',
      dimension: 'content',
      claim: 'L3 证据链给出支持 / 矛盾计数、L1 来源与权重',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['L3 渲染三层画像，展开证据链并重载画像'],
      },
    },
    {
      id: 'memory-content-8',
      dimension: 'content',
      claim: '知识按固定字段顺序分组，更新历史标出当前版本',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['知识页按固定字段顺序分组，更新历史标出当前版本'],
      },
    },
    {
      id: 'memory-content-9',
      dimension: 'content',
      claim: 'L1 详情展示来源摘录（最近三条）、压缩比，撤回消息只留占位',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['L1 详情按需取来源摘录，撤回消息只留占位，并可回对话'],
      },
    },
    {
      id: 'memory-content-10',
      dimension: 'content',
      claim: '深度处理入口只对导入人格出现',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['切换人格重新取数，深度处理入口只对导入人格出现'],
      },
    },
    {
      id: 'memory-operation-1',
      dimension: 'operation',
      claim: '切换页签复用已装载数据，不重复请求',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['切换页签复用已装载数据，不再请求'],
      },
    },
    {
      id: 'memory-operation-2',
      dimension: 'operation',
      claim: '切换人格清空详情与摘录缓存并重新取数',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['切换人格重新取数，深度处理入口只对导入人格出现'],
      },
    },
    {
      id: 'memory-operation-3',
      dimension: 'operation',
      claim: '展开 / 收起详情，同时只开一张并按需取摘录与证据链',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: [
          'L1 详情按需取来源摘录，撤回消息只留占位，并可回对话',
          'L3 渲染三层画像，展开证据链并重载画像',
        ],
      },
    },
    {
      id: 'memory-operation-4',
      dimension: 'operation',
      claim: '查看对话保存快照后带会话参数跳转',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['L1 详情按需取来源摘录，撤回消息只留占位，并可回对话'],
      },
    },
    {
      id: 'memory-operation-5',
      dimension: 'operation',
      claim: '深度处理与重载画像提交后台管线并给出回执提示',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: [
          '切换人格重新取数，深度处理入口只对导入人格出现',
          'L3 渲染三层画像，展开证据链并重载画像',
        ],
      },
    },
    {
      id: 'memory-operation-6',
      dimension: 'operation',
      claim: '主请求失败可重试并恢复',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['主请求全部失败进错误态，重试后恢复'],
      },
    },
    {
      id: 'memory-operation-7',
      dimension: 'operation',
      claim: '空态提供去导入的唯一主操作',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['空数据给引导空态并可跳转导入'],
      },
    },
    {
      id: 'memory-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '有数据且能力可用：渲染内容、操作可用',
      status: 'covered',
      coverage: { file: 'memoryView.test.ts', cases: ['首页渲染 L1 卡片、页签计数与视图主态'] },
    },
    {
      id: 'memory-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '首屏在途：骨架屏 + 忙碌标记',
      status: 'covered',
      coverage: { file: 'memoryView.test.ts', cases: ['首屏读取中给出骨架与忙碌标记'] },
    },
    {
      id: 'memory-state-empty',
      dimension: 'state',
      cell: '空·no-data',
      claim: '请求成功但无数据：说明 + 去导入',
      status: 'covered',
      coverage: { file: 'memoryView.test.ts', cases: ['空数据给引导空态并可跳转导入'] },
    },
    {
      id: 'memory-state-empty-filter',
      dimension: 'state',
      cell: '空·filter',
      claim: '筛选无结果',
      status: 'na',
      naReason: '无浏览期筛选面（备注 35）',
    },
    {
      id: 'memory-state-empty-capability',
      dimension: 'state',
      cell: '空·capability',
      claim: '能力缺失导致的空',
      status: 'na',
      naReason: '记忆浏览不依赖嵌入 / LLM，缺能力不产生空态（备注 35）',
    },
    {
      id: 'memory-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '主请求失败：原因 + 重试，工具栏仍可用',
      status: 'covered',
      coverage: { file: 'memoryView.test.ts', cases: ['主请求全部失败进错误态，重试后恢复'] },
    },
    {
      id: 'memory-state-degraded-index',
      dimension: 'state',
      cell: '降级·index',
      claim: '索引未就绪：降级条 + 去设置，浏览不阻塞',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['索引未就绪时给降级条，浏览不受影响且可跳设置'],
      },
    },
    {
      id: 'memory-state-degraded-embedding',
      dimension: 'state',
      cell: '降级·embedding',
      claim: '嵌入缺失导致搜索不可用',
      status: 'na',
      naReason: '无浏览期语义搜索面（备注 35）',
    },
  ],
}

const PERSONA: ParityViewChecklist = {
  view: 'persona',
  qaId: 'QA-V-persona',
  entries: [
    {
      id: 'persona-content-1',
      dimension: 'content',
      claim: '列表人格卡显示名称、类型、头像与默认标记，工具条给出数量与默认统计',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['渲染人格卡、类型标签与默认统计'],
      },
    },
    {
      id: 'persona-content-2',
      dimension: 'content',
      claim: '详情基础资料显示名称、头像与描述',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['进入详情展示基础资料、元数据与画像状态'],
      },
    },
    {
      id: 'persona-content-3',
      dimension: 'content',
      claim: '元数据卡显示 uid、类型、来源、默认开关与画像状态',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['进入详情展示基础资料、元数据与画像状态'],
      },
    },
    {
      id: 'persona-content-4',
      dimension: 'content',
      claim: '提示词查看器两档：静态段与全量（含召回）',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: [
          '提示词查看器：折叠态即读静态段，展开压上两卡并可切编译预览',
          '提示词读取失败时折叠态给出原因，展开自动重取',
        ],
      },
    },
    {
      id: 'persona-content-5',
      dimension: 'content',
      claim: '预设导入预览给出 UID、模式、拆解状态与隐私确认',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导入两段式：选文件 → 预览 → 确认写入'],
      },
    },
    {
      id: 'persona-content-6',
      dimension: 'content',
      claim: '导出弹窗含「含对话示例」开关与警示',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导出：示例开关传到命令，取消保存不写文件'],
      },
    },
    {
      id: 'persona-operation-1',
      dimension: 'operation',
      claim: '进入 / 返回详情，未保存修改返回先确认',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['未保存返回列表先确认，确认后丢弃草稿'],
      },
    },
    {
      id: 'persona-operation-2',
      dimension: 'operation',
      claim: '编辑名称 / 头像 / 描述并保存，空名禁保存、清头像下发空串',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: [
          '编辑名称后出现未保存标记，保存按请求形状提交',
          '清除头像时下发空串以清除旧值',
          '名称为空时禁止保存并提示原因',
        ],
      },
    },
    {
      id: 'persona-operation-3',
      dimension: 'operation',
      claim: '设为默认并更新标记',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['设为默认写入本地设置并更新标记'],
      },
    },
    {
      id: 'persona-operation-4',
      dimension: 'operation',
      claim: '重载画像经确认后提交后台管线',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['重载画像经确认后提交后台管线'],
      },
    },
    {
      id: 'persona-operation-5',
      dimension: 'operation',
      claim: '预设导入两段式：选文件 → 预览 → 确认写入',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导入两段式：选文件 → 预览 → 确认写入'],
      },
    },
    {
      id: 'persona-operation-6',
      dimension: 'operation',
      claim: '导出先取保存路径再写文件，示例开关传到命令',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导出：示例开关传到命令，取消保存不写文件'],
      },
    },
    {
      id: 'persona-operation-7',
      dimension: 'operation',
      claim: '提示词可复制，读取失败可重取',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['提示词读取失败时折叠态给出原因，展开自动重取'],
      },
    },
    {
      id: 'persona-operation-8',
      dimension: 'operation',
      claim: '列表读取失败可重试',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['列表读取失败给出错误态与重试'],
      },
    },
    {
      id: 'persona-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '列表有数据：卡片与操作可用',
      status: 'covered',
      coverage: { file: 'personaView.test.ts', cases: ['渲染人格卡、类型标签与默认统计'] },
    },
    {
      id: 'persona-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '首屏在途：卡片骨架 + 忙碌标记',
      status: 'covered',
      coverage: { file: 'personaView.test.ts', cases: ['列表读取中给出骨架与忙碌标记'] },
    },
    {
      id: 'persona-state-empty',
      dimension: 'state',
      cell: '空',
      claim: '无人格：说明 + 导入预设入口',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['空列表给出导入入口，点击打开导入弹窗'],
      },
    },
    {
      id: 'persona-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '列表失败：原因 + 重试',
      status: 'covered',
      coverage: { file: 'personaView.test.ts', cases: ['列表读取失败给出错误态与重试'] },
    },
    {
      id: 'persona-state-degraded-profile',
      dimension: 'state',
      cell: '降级·profile',
      claim: '画像未生成：提示 + 重载画像入口',
      status: 'covered',
      coverage: { file: 'personaView.test.ts', cases: ['画像未生成时给出降级提示'] },
    },
    {
      id: 'persona-state-degraded-llm',
      dimension: 'state',
      cell: '降级·llm',
      claim: '对话模型不可用：降级标记，浏览与编辑仍可用',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['对话模型不可用时标记降级，浏览与编辑不受影响'],
      },
    },
    {
      id: 'persona-state-restricted-import',
      dimension: 'state',
      cell: '受限·import-locked',
      claim: '预设写入中锁定流程',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导入预览为只读，写入中锁定流程并给出受限标记'],
      },
    },
    {
      id: 'persona-state-restricted-preview',
      dimension: 'state',
      cell: '受限·preview-readonly',
      claim: '预设预览只读',
      status: 'covered',
      coverage: {
        file: 'personaView.test.ts',
        cases: ['导入预览为只读，写入中锁定流程并给出受限标记'],
      },
    },
  ],
}

const RULES: ParityViewChecklist = {
  view: 'rules',
  qaId: 'QA-V-rules',
  entries: [
    {
      id: 'rules-content-1',
      dimension: 'content',
      claim: '人格筛选块与规则总数',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['渲染规则卡、来源与元信息，且没有删除入口'],
      },
    },
    {
      id: 'rules-content-2',
      dimension: 'content',
      claim: '规则卡显示来源、启停、情境关键词、置信度 / 稳定性 / 情感强度与证据条数',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['渲染规则卡、来源与元信息，且没有删除入口'],
      },
    },
    {
      id: 'rules-content-3',
      dimension: 'content',
      claim: '候选规则标注「仅参数注入」，规则文本可留空',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: [
          '渲染规则卡、来源与元信息，且没有删除入口',
          '候选规则只改禁忌词时保存不带规则文本',
        ],
      },
    },
    {
      id: 'rules-content-4',
      dimension: 'content',
      claim: '详情展示规则文本、禁忌词、四项参数、情境统计、元数据与证据链',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['详情页展示规则内容、参数与证据链，并可返回列表'],
      },
    },
    {
      id: 'rules-content-5',
      dimension: 'content',
      claim: '不提供删除与重新推断入口，编辑只在详情页',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['渲染规则卡、来源与元信息，且没有删除入口'],
      },
    },
    {
      id: 'rules-operation-1',
      dimension: 'operation',
      claim: '切换人格清空重取，切回命中缓存并静默刷新',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['切换人格清空重取，切回旧人格命中缓存并静默刷新'],
      },
    },
    {
      id: 'rules-operation-2',
      dimension: 'operation',
      claim: '启停写库后按回执更新卡片，失败保持原状态',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['启停写库后按回执更新卡片', '启停失败保持原状态'],
      },
    },
    {
      id: 'rules-operation-3',
      dimension: 'operation',
      claim: '进入详情与返回列表，未保存改动返回先确认',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['有未保存改动时返回先确认'],
      },
    },
    {
      id: 'rules-operation-4',
      dimension: 'operation',
      claim: '详情页编辑保存后转为手工并就地更新',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['保存后转为手工并就地更新', '已有文本的规则不允许清空，保存保持不可用'],
      },
    },
    {
      id: 'rules-operation-5',
      dimension: 'operation',
      claim: '证据链可重试，空证据说明原因',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['证据为空时说明原因', '证据读取失败可重试'],
      },
    },
    {
      id: 'rules-operation-6',
      dimension: 'operation',
      claim: '列表失败可重试',
      status: 'covered',
      coverage: { file: 'rulesView.test.ts', cases: ['列表失败给出错误态与重试'] },
    },
    {
      id: 'rules-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '有规则：卡片与启停可用',
      status: 'covered',
      coverage: {
        file: 'rulesView.test.ts',
        cases: ['渲染规则卡、来源与元信息，且没有删除入口'],
      },
    },
    {
      id: 'rules-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '首屏在途：卡片骨架 + 忙碌标记',
      status: 'covered',
      coverage: { file: 'rulesView.test.ts', cases: ['首屏读取中给出骨架与忙碌标记'] },
    },
    {
      id: 'rules-state-empty',
      dimension: 'state',
      cell: '空',
      claim: '无规则：说明 + 去对话',
      status: 'covered',
      coverage: { file: 'rulesView.test.ts', cases: ['无规则时给出去对话的引导'] },
    },
    {
      id: 'rules-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '列表失败：原因 + 重试',
      status: 'covered',
      coverage: { file: 'rulesView.test.ts', cases: ['列表失败给出错误态与重试'] },
    },
    {
      id: 'rules-state-degraded-llm',
      dimension: 'state',
      cell: '降级·llm',
      claim: '对话模型不可用：降级条，编辑与启停仍可用',
      status: 'covered',
      coverage: { file: 'rulesView.test.ts', cases: ['对话模型不可用时给降级条，编辑仍可用'] },
    },
  ],
}

const IMPORT: ParityViewChecklist = {
  view: 'import',
  qaId: 'QA-V-import',
  entries: [
    {
      id: 'import-content-1',
      dimension: 'content',
      claim: '选择步同屏常驻文件行与模式卡，含双方 Persona 与切割间隔，底部单个分析动作',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: [
          '未选文件时同屏展示文件行占位与模式卡，并查询续跑状态',
          '选择文件后展示文件名与路径、模式卡与双方字段',
        ],
      },
    },
    {
      id: 'import-content-2',
      dimension: 'content',
      claim: '预览步给出统计卡与信息列（导出者 / 对方、时间范围、切割、模式、对话类型）与群聊成员',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['分析后展示统计、群聊成员与确认按钮'],
      },
    },
    {
      id: 'import-content-3',
      dimension: 'content',
      claim: '执行步给出阶段条、百分比、已完成 / 总数、ETA 与离开不中断提示',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['确认导入后进入执行步，提交参数带报告回填的名称'],
      },
    },
    {
      id: 'import-content-4',
      dimension: 'content',
      claim: '完成页给出统计、L1 失败提示与四个动作',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['完成事件后展示统计、L1 失败提示与四个动作'],
      },
    },
    {
      id: 'import-content-5',
      dimension: 'content',
      claim: '存在待办时给出继续处理入口与规模',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['存在待办时展示继续处理入口与规模'],
      },
    },
    {
      id: 'import-operation-1',
      dimension: 'operation',
      claim: '选择 / 更换文件',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['选择文件后展示文件名与路径、模式卡与双方字段'],
      },
    },
    {
      id: 'import-operation-2',
      dimension: 'operation',
      claim: '分析文件（只调 analyze_qq_chat）',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['分析后展示统计、群聊成员与确认按钮'],
      },
    },
    {
      id: 'import-operation-3',
      dimension: 'operation',
      claim: '确认导入，提交参数带报告回填的名称',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['确认导入后进入执行步，提交参数带报告回填的名称'],
      },
    },
    {
      id: 'import-operation-4',
      dimension: 'operation',
      claim: '解析失败可重试并回到预览',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['解析失败进入错误态，重试成功回到预览'],
      },
    },
    {
      id: 'import-operation-5',
      dimension: 'operation',
      claim: '执行失败可重试或更换文件',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['导入执行失败进入错误态并给出重试与更换文件'],
      },
    },
    {
      id: 'import-operation-6',
      dimension: 'operation',
      claim: '继续处理：选目录后续跑，取消时可改选仅补摘要与画像',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: [
          '继续处理选择目录后调用续跑，完成页展示续跑统计',
          '取消目录选择后可选择仅补摘要与画像',
        ],
      },
    },
    {
      id: 'import-operation-7',
      dimension: 'operation',
      claim: '完成动作：前往记忆页面预选导出者人格、查看导入消息',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: [
          '完成页「前往记忆页面」预选导出者人格',
          '完成页「查看导入消息」打开最后一个导入会话',
        ],
      },
    },
    {
      id: 'import-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '预览步 / 完成页有内容，操作可用',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['完成事件后展示统计、L1 失败提示与四个动作'],
      },
    },
    {
      id: 'import-state-loading-analyze',
      dimension: 'state',
      cell: '加载·分析',
      claim: '解析在途：骨架 + 忙碌标记',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['解析期间进加载态，给出骨架与忙碌标记'],
      },
    },
    {
      id: 'import-state-loading-run',
      dimension: 'state',
      cell: '加载·执行',
      claim: '执行在途：阶段进度 + ETA',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['确认导入后进入执行步，提交参数带报告回填的名称'],
      },
    },
    {
      id: 'import-state-empty',
      dimension: 'state',
      cell: '空·未选',
      claim: '未选文件：文件行承担选择，不设独立空态页',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['未选文件时同屏展示文件行占位与模式卡，并查询续跑状态'],
      },
    },
    {
      id: 'import-state-error-parse',
      dimension: 'state',
      cell: '错误·解析',
      claim: '解析失败停在预览步，给重试',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['解析失败进入错误态，重试成功回到预览'],
      },
    },
    {
      id: 'import-state-error-run',
      dimension: 'state',
      cell: '错误·执行',
      claim: '导入失败停在执行步，给重试与更换文件',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['导入执行失败进入错误态并给出重试与更换文件'],
      },
    },
    {
      id: 'import-state-degraded',
      dimension: 'state',
      cell: '降级·skipped/L1',
      claim: '附件跳过或 L1 失败：完成页降级提示 + 继续处理',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['完成事件后展示统计、L1 失败提示与四个动作'],
      },
    },
    {
      id: 'import-state-restricted',
      dimension: 'state',
      cell: '受限·locked',
      claim: '执行中锁步骤与参数',
      status: 'covered',
      coverage: {
        file: 'importView.test.ts',
        cases: ['确认导入后进入执行步，提交参数带报告回填的名称'],
      },
    },
  ],
}

const DEBUG: ParityViewChecklist = {
  view: 'debug',
  qaId: 'QA-V-debug',
  entries: [
    {
      id: 'debug-content-1',
      dimension: 'content',
      claim: '关键词池给出规范词 / 已确认别名 / 待确认别名三个计数与按状态分组的全量词条',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['默认进入关键词池，展示三态计数与待确认别名'],
      },
    },
    {
      id: 'debug-content-2',
      dimension: 'content',
      claim: '待确认别名行展示「别名 → 建议规范词」，并给两个裁决动作',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['默认进入关键词池，展示三态计数与待确认别名', '确认合并后调用裁决命令并重新取数'],
      },
    },
    {
      id: 'debug-content-3',
      dimension: 'content',
      claim: '说话风格展示样本量、统计状态、规则来源、自动规则文本与五维统计参数',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['说话风格按人格取数并渲染五维与规则文本'],
      },
    },
    {
      id: 'debug-content-4',
      dimension: 'content',
      claim: '评估产物展示已选目录、文件大小与修改时间，以及单文件的档位得分与辅助指标',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['选择目录后列出产物并打开摘要'],
      },
    },
    {
      id: 'debug-operation-1',
      dimension: 'operation',
      claim: '裁决待确认别名：确认合并 / 驳回为规范词，处理后重新取数',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['确认合并后调用裁决命令并重新取数'],
      },
    },
    {
      id: 'debug-operation-2',
      dimension: 'operation',
      claim: '说话风格按人格下拉切换并重新取数',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['切换人格后按该人格重新取统计'],
      },
    },
    {
      id: 'debug-operation-3',
      dimension: 'operation',
      claim: '评估产物：选择目录、打开文件查看摘要、返回文件列表',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['选择目录后列出产物并打开摘要'],
      },
    },
    {
      id: 'debug-operation-4',
      dimension: 'operation',
      claim: '折叠组可展开 / 收起，展开后显示该状态的全部词条',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['折叠组展开后显示该状态的词条'],
      },
    },
    {
      id: 'debug-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '页签有内容：计数、词条与动作可用',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['默认进入关键词池，展示三态计数与待确认别名'],
      },
    },
    {
      id: 'debug-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '主请求在途：骨架占位 + 忙碌标记，切换页签不受阻',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['请求在途时进入加载态并给出骨架'],
      },
    },
    {
      id: 'debug-state-empty',
      dimension: 'state',
      cell: '空',
      claim: '无统计记录或无产物目录：按页签归因，给对应主操作',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['无统计记录时说话风格为空态', '深链直接进入评估产物页签'],
      },
    },
    {
      id: 'debug-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '主请求失败：错误块写清原因并可重试',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['关键词池读取失败显示错误块并可重试'],
      },
    },
    {
      id: 'debug-state-degraded',
      dimension: 'state',
      cell: '降级·dir',
      claim: '评估目录不可读：视图内降级条 + 重新选择，不替换内容也不影响其余页签',
      status: 'covered',
      coverage: {
        file: 'debugView.test.ts',
        cases: ['评估目录不可读时显示降级条并清空列表'],
      },
    },
  ],
}

/** 视图 A / B 的五份 parity 清单 */
export const PARITY_CHECKLIST: ParityViewChecklist[] = [MEMORY, PERSONA, RULES, IMPORT, DEBUG]
