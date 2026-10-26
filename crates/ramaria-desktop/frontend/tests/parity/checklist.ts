/**
 * tests/parity/checklist.ts — 九视图的 parity 清单（内容 / 操作 / 状态）
 *
 * 职责:
 * - 单一源声明 chat / memory / persona / rules / import / settings / debug / setup / error 该有什么内容、能做哪些操作、覆盖哪些状态
 * - 每条声明绑定覆盖它的测试文件与用例名，供 checklist.test.ts 机器校验引用不漂移
 *
 * 设计特点:
 * - 状态条目的 `cell` 与 `design-qa.md` §9 的格名逐字一致，两份文档由测试对齐
 * - 不适用的格写 `na` 并给依据，不允许留空
 * - 内容 / 操作条目只写可观察行为，不写实现细节
 */

/** 参与 parity 的视图标识 */
export type ParityViewId =
  | 'chat'
  | 'memory'
  | 'persona'
  | 'rules'
  | 'import'
  | 'settings'
  | 'debug'
  | 'setup'
  | 'error'

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

const CHAT: ParityViewChecklist = {
  view: 'chat',
  qaId: 'QA-V-chat',
  entries: [
    {
      id: 'chat-content-1',
      dimension: 'content',
      claim: '助手回复按 || 拆多气泡，撤回消息折叠为居中系统条',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['自动打开进行中的本地会话并渲染人格与消息'],
      },
    },
    {
      id: 'chat-content-2',
      dimension: 'content',
      claim: '会话卡片按今天 / 更早分组，行内带来源标签与未读徽标',
      status: 'covered',
      coverage: {
        file: 'chatSessions.test.ts',
        cases: ['按今天与更早分组', '导入人格的会话只读并带导入标签'],
      },
    },
    {
      id: 'chat-content-3',
      dimension: 'content',
      claim: '会话卡片副行优先展示后端预览，缺省回退消息数',
      status: 'covered',
      coverage: {
        file: 'chatSessions.test.ts',
        cases: ['后端给出预览时优先展示预览', '产品人格标默认，副行回退消息数'],
      },
    },
    {
      id: 'chat-content-4',
      dimension: 'content',
      claim: '图片消息渲染为图片卡，占位符卡片即时插入',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['选文件后落库并渲染图片消息'],
      },
    },
    {
      id: 'chat-content-5',
      dimension: 'content',
      claim: '主动消息带「主动消息」标签，同标识不重复插入',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['当前会话追加带主动标识的消息，同标识不重复'],
      },
    },
    {
      id: 'chat-content-6',
      dimension: 'content',
      claim: '只读会话显示只读横幅与底部只读条，不渲染输入区',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['从会话卡片切换到社交通道会话后隐藏输入区'],
      },
    },
    {
      id: 'chat-operation-1',
      dimension: 'operation',
      claim: 'Enter 发送、Shift+Enter 换行，输入法合成期回车不发送',
      status: 'covered',
      coverage: {
        file: 'chatComposer.test.ts',
        cases: [
          'Enter 发送并清空输入',
          'Shift+Enter 换行不发送',
          '输入法合成期的回车不发送，合成结束后可发送',
        ],
      },
    },
    {
      id: 'chat-operation-2',
      dimension: 'operation',
      claim: '发送失败给失败气泡，重试复用原文',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['发送失败给出失败气泡，重试复用原文'],
      },
    },
    {
      id: 'chat-operation-3',
      dimension: 'operation',
      claim: '本人消息可撤回，二次确认后折叠为系统条',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['本人消息可撤回，确认后折叠为系统条'],
      },
    },
    {
      id: 'chat-operation-4',
      dimension: 'operation',
      claim: '发图走文件对话框与本地发图命令',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['选文件后落库并渲染图片消息'],
      },
    },
    {
      id: 'chat-operation-5',
      dimension: 'operation',
      claim: '索引横幅动作触发重建，构建中跳设置',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['索引未建立时给出降级条与重建动作'],
      },
    },
    {
      id: 'chat-operation-6',
      dimension: 'operation',
      claim: '会话卡片可从头像展开、切会话、按外部点击收起',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['从会话卡片切换到社交通道会话后隐藏输入区'],
      },
    },
    {
      id: 'chat-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '有会话与消息：内容、输入与操作可用',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['自动打开进行中的本地会话并渲染人格与消息'],
      },
    },
    {
      id: 'chat-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '首屏在途：骨架 + 忙碌标记',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['首屏在途显示加载态与忙碌标记'],
      },
    },
    {
      id: 'chat-state-empty',
      dimension: 'state',
      cell: '空',
      claim: '无会话：无按钮的空态，输入区可用',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['无会话时为空态，输入区保持可用'],
      },
    },
    {
      id: 'chat-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '主请求失败：错误块 + 重试，输入禁用',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['读取会话失败显示错误态并禁用输入'],
      },
    },
    {
      id: 'chat-state-degraded-index',
      dimension: 'state',
      cell: '降级·index',
      claim: '索引未就绪：横幅 + 重建动作，不阻塞对话',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['索引未建立时给出降级条与重建动作'],
      },
    },
    {
      id: 'chat-state-degraded-llm',
      dimension: 'state',
      cell: '降级·llm',
      claim: '对话模型不可用：横幅 + 降级标记，转设置',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['模型不可用时给出降级条与降级标记'],
      },
    },
    {
      id: 'chat-state-degraded-privacy',
      dimension: 'state',
      cell: '降级·privacy',
      claim: '隐私未确认：横幅「去确认」+ 降级标记',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['隐私未确认时给出隐私降级条与降级标记'],
      },
    },
    {
      id: 'chat-state-restricted-readonly',
      dimension: 'state',
      cell: '受限·readonly',
      claim: '社交会话：只读横幅 + 隐藏输入区 + 底部只读条',
      status: 'covered',
      coverage: {
        file: 'chatView.test.ts',
        cases: ['从会话卡片切换到社交通道会话后隐藏输入区'],
      },
    },
    {
      id: 'chat-state-restricted-locked',
      dimension: 'state',
      cell: '受限·locked',
      claim: '流式生成中：输入与按键锁定，完成后恢复',
      status: 'covered',
      coverage: {
        file: 'chatFlow.test.ts',
        cases: ['发送后锁定输入，增量上屏，完成后恢复'],
      },
    },
  ],
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
      claim: '索引未就绪：页签上方横幅（构建中带百分比）+ 去设置，浏览不阻塞',
      status: 'covered',
      coverage: {
        file: 'memoryView.test.ts',
        cases: ['索引横幅位于页签之上：构建中带百分比，其余态为未就绪，浏览不受影响且可跳设置'],
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

const SETTINGS: ParityViewChecklist = {
  view: 'settings',
  qaId: 'QA-V-settings',
  entries: [
    {
      id: 'settings-content-1',
      dimension: 'content',
      claim: '基础子页按目录渲染卡片并回显生效配置',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['加载后渲染当前子页的卡片并回显配置'],
      },
    },
    {
      id: 'settings-content-2',
      dimension: 'content',
      claim: '界面与导航卡给出 5 个方位预设并可切换',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['界面与导航卡按方位缩略图切换预设，即写入本地偏好'],
      },
    },
    {
      id: 'settings-content-3',
      dimension: 'content',
      claim: '主动名单与投递概览无数据时给卡内说明，同页其它卡照常渲染',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['局部空：主动消息名单与投递概览给卡内说明，同页其它卡照常渲染'],
      },
    },
    {
      id: 'settings-content-4',
      dimension: 'content',
      claim: '深链定位子页；进阶未解锁时收敛回基础页',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['进阶未解锁时深链收敛回基础页，解锁后可直达'],
      },
    },
    {
      id: 'settings-operation-1',
      dimension: 'operation',
      claim: '脏值在分类键上挂未保存徽标，保存后清除',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['字段改动后分类键出现未保存徽标，保存后清除'],
      },
    },
    {
      id: 'settings-operation-2',
      dimension: 'operation',
      claim: '只改密钥时随卡保存写入，不触发配置回写',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['只改密钥时由卡片保存直接写入，不触发配置回写'],
      },
    },
    {
      id: 'settings-operation-3',
      dimension: 'operation',
      claim: '模型路径随卡保存热加载，页脚保留校验动作',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['模型路径随卡片保存热加载，页脚保留校验动作'],
      },
    },
    {
      id: 'settings-operation-4',
      dimension: 'operation',
      claim: '保存只覆盖本卡字段，并以最新配置为基底',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['保存只覆盖本卡字段，并以最新配置为基底'],
      },
    },
    {
      id: 'settings-operation-5',
      dimension: 'operation',
      claim: '恢复默认只改表单，再保存才落库',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['恢复默认只改表单，再保存才写库'],
      },
    },
    {
      id: 'settings-operation-6',
      dimension: 'operation',
      claim: '主请求失败可重试恢复',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['主请求失败显示视图错误，重试后恢复'],
      },
    },
    {
      id: 'settings-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '配置读取成功：卡片回显生效值',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['加载后渲染当前子页的卡片并回显配置'],
      },
    },
    {
      id: 'settings-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '主请求在途：卡片骨架 + 忙碌标记',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['配置读取在途时给加载态、忙碌标记与骨架'],
      },
    },
    {
      id: 'settings-state-empty-local',
      dimension: 'state',
      cell: '空·局部',
      claim: '局部空（名单 / 概览）留在卡内说明，不替换整页',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['局部空：主动消息名单与投递概览给卡内说明，同页其它卡照常渲染'],
      },
    },
    {
      id: 'settings-state-error-main',
      dimension: 'state',
      cell: '错误·主请求',
      claim: '主请求失败：视图错误 + 重试',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['主请求失败显示视图错误，重试后恢复'],
      },
    },
    {
      id: 'settings-state-error-card',
      dimension: 'state',
      cell: '错误·单卡',
      claim: '单卡写入失败：卡内错误条 + 保留草稿',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['单卡双侧写失败：错误留在卡内并保留草稿'],
      },
    },
    {
      id: 'settings-state-degraded-embedding',
      dimension: 'state',
      cell: '降级·embedding',
      claim: '嵌入缺失：根节点输出 embedding 标记',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['嵌入缺失与模型不可用各自输出降级标记'],
      },
    },
    {
      id: 'settings-state-degraded-llm',
      dimension: 'state',
      cell: '降级·llm',
      claim: '对话模型不可用：根节点输出 llm 标记',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['嵌入缺失与模型不可用各自输出降级标记'],
      },
    },
    {
      id: 'settings-state-degraded-index',
      dimension: 'state',
      cell: '降级·index',
      claim: '索引未就绪：根节点输出 index 标记',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['索引构建中为 index，部分向量缺失为 partial'],
      },
    },
    {
      id: 'settings-state-degraded-partial',
      dimension: 'state',
      cell: '降级·partial',
      claim: '索引就绪但存在补算失败：根节点输出 partial 标记',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['索引构建中为 index，部分向量缺失为 partial'],
      },
    },
    {
      id: 'settings-state-restricted-readonly',
      dimension: 'state',
      cell: '受限·readonly',
      claim: '凭据只读',
      status: 'na',
      naReason: '凭据只写不回显（掩码展示），不构成视图级只读态；见 frontend-spec-v3 §11',
    },
    {
      id: 'settings-state-restricted-locked',
      dimension: 'state',
      cell: '受限·locked',
      claim: '保存中锁字段并输出 locked 标记，完成后解除',
      status: 'covered',
      coverage: {
        file: 'settingsView.test.ts',
        cases: ['保存中锁定卡片并输出受限标记，完成后解除'],
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

const SETUP: ParityViewChecklist = {
  view: 'setup',
  qaId: 'QA-V-setup',
  entries: [
    {
      id: 'setup-content-1',
      dimension: 'content',
      claim: '三步向导带步骤条与步骤计数，默认落第 1 步',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['默认第 1 步并渲染本地字段'],
      },
    },
    {
      id: 'setup-content-2',
      dimension: 'content',
      claim: '本地 / 线上两组字段互斥显示',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['切到线上 API 后显示密钥与 API 地址字段'],
      },
    },
    {
      id: 'setup-content-3',
      dimension: 'content',
      claim: '确认步给出摘要，跳过嵌入时标出降级',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['测试连接、跳过嵌入、提交并进入应用'],
      },
    },
    {
      id: 'setup-operation-1',
      dimension: 'operation',
      claim: '字段校验不通过时不前进并给原因',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['模型名为空时下一步被字段校验拦下'],
      },
    },
    {
      id: 'setup-operation-2',
      dimension: 'operation',
      claim: '测试连接先保存再探测，失败留在步骤内',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['连接失败在步骤内给出原因，不切换步骤'],
      },
    },
    {
      id: 'setup-operation-3',
      dimension: 'operation',
      claim: '保存并进入应用：写入单键命令、刷新状态，不整份回写配置',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['测试连接、跳过嵌入、提交并进入应用'],
      },
    },
    {
      id: 'setup-state-default',
      dimension: 'state',
      cell: '默认',
      claim: '步骤 1：本地字段与主态标记',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['默认第 1 步并渲染本地字段'],
      },
    },
    {
      id: 'setup-state-loading',
      dimension: 'state',
      cell: '加载',
      claim: '测试连接 / 校验中：步骤内状态条与按钮加载',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['测试连接在途显示加载文案与按钮加载标记'],
      },
    },
    {
      id: 'setup-state-error',
      dimension: 'state',
      cell: '错误',
      claim: '步骤内错误条给出原因，不切换步骤',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['连接失败在步骤内给出原因，不切换步骤'],
      },
    },
    {
      id: 'setup-state-restricted',
      dimension: 'state',
      cell: '受限',
      claim: '提交中整页锁定并输出受限标记',
      status: 'covered',
      coverage: {
        file: 'setupView.test.ts',
        cases: ['提交中整页锁定并输出受限标记'],
      },
    },
  ],
}

const ERROR: ParityViewChecklist = {
  view: 'error',
  qaId: 'QA-V-error',
  entries: [
    {
      id: 'error-content-1',
      dimension: 'content',
      claim: '标题、说明、后端短句与唯一重试动作，不渲染业务内容',
      status: 'covered',
      coverage: {
        file: 'errorView.test.ts',
        cases: ['渲染标题、说明与后端短句，只留一个重试动作'],
      },
    },
    {
      id: 'error-operation-1',
      dimension: 'operation',
      claim: '重试重新读取应用状态',
      status: 'covered',
      coverage: {
        file: 'errorView.test.ts',
        cases: ['重试期间切到加载态，结束后回到错误态'],
      },
    },
    {
      id: 'error-state-detail',
      dimension: 'state',
      cell: '错误详情',
      claim: '错误态本体：标题 + 原因 + 重试',
      status: 'covered',
      coverage: {
        file: 'errorView.test.ts',
        cases: ['渲染标题、说明与后端短句，只留一个重试动作'],
      },
    },
    {
      id: 'error-state-retrying',
      dimension: 'state',
      cell: '重试中',
      claim: '重试在途：根节点加载态 + 按钮加载标记',
      status: 'covered',
      coverage: {
        file: 'errorView.test.ts',
        cases: ['重试期间切到加载态，结束后回到错误态'],
      },
    },
  ],
}

/** 九视图 parity 清单（顺序与 `design-qa.md` §4 的 QA-V 行一致） */
export const PARITY_CHECKLIST: ParityViewChecklist[] = [
  CHAT,
  MEMORY,
  PERSONA,
  RULES,
  IMPORT,
  SETTINGS,
  DEBUG,
  SETUP,
  ERROR,
]
