/**
 * tests/parity/jumps.ts — 跨视图跳转矩阵条目
 *
 * 职责:
 * - 单一源声明 `frontend-spec-v3.md` §8 的跳转条目（13 条全局 + 页内二级）
 * - 每条绑定覆盖它的测试文件与用例名，供 jumps.test.ts 机器校验引用不漂移
 *
 * 设计特点:
 * - 条目编号、触发、目标与参数与规格 §8 的表格逐字一致，两份由测试对齐
 * - `scope` 区分全局形态变化与页内二级导航：全局 13 条（#1 ~ #12、#14）
 * - 一条跳转的两侧（触发端与落点端）可分别由不同用例覆盖，按文件逐条列出
 * - 只声明可观察的跳转行为，不写实现细节
 */

/** 跳转条目的覆盖情况 */
export type JumpStatus = 'covered' | 'na'

/** 跳转的作用范围：全局形态变化 / 页内二级导航 */
export type JumpScope = 'global' | 'page'

/** 覆盖用例所在文件与用例名（文件名相对 `tests/`） */
export interface JumpCoverage {
  file: string
  cases: string[]
}

export interface JumpEntry {
  /** 规格 §8 矩阵行号 */
  no: number
  /** 触发动作 */
  trigger: string
  /** 目标视图 */
  target: string
  /** 参数与前置条件 */
  params: string
  /** 全局形态变化或页内二级导航 */
  scope: JumpScope
  status: JumpStatus
  /** 已覆盖条目的用例落点（触发端与落点端可各占一条） */
  coverage?: JumpCoverage[]
  /** 不适用条目的依据 */
  naReason?: string
}

/** 规格 §8 的跳转条目（全局 13 条 + 页内 2 条） */
export const JUMP_MATRIX: JumpEntry[] = [
  {
    no: 1,
    trigger: 'needs_setup',
    target: 'setup',
    params: '欢迎文案',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'appRouting.test.ts', cases: ['needs_setup 时主视图被拦回配置页'] },
    ],
  },
  {
    no: 2,
    trigger: 'downloading_model',
    target: '不切视图',
    params: '全局下载横幅（不含 indexing）',
    scope: 'global',
    status: 'covered',
    coverage: [{ file: 'appRouting.test.ts', cases: ['downloading_model 不切视图'] }],
  },
  {
    no: 3,
    trigger: 'ready',
    target: '上次导航视图（默认 chat）',
    params: '恢复所在页',
    scope: 'global',
    status: 'covered',
    coverage: [{ file: 'appRouting.test.ts', cases: ['ready 停留在上次导航视图'] }],
  },
  {
    no: 4,
    trigger: 'degraded',
    target: '上次导航视图',
    params: '视图内降级条按 degradedReason 分流',
    scope: 'global',
    status: 'covered',
    coverage: [
      {
        file: 'appRouting.test.ts',
        cases: ['degraded 停留在上次导航视图并带上降级原因'],
      },
      {
        file: 'chatFlow.test.ts',
        cases: ['模型不可用时给出降级条与降级标记'],
      },
    ],
  },
  {
    no: 5,
    trigger: 'fatal_error / 后端连接失败',
    target: 'error',
    params: '错误标题 + 详情',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'appRouting.test.ts', cases: ['fatal_error 时主视图被拦到错误页'] },
      {
        file: 'errorView.test.ts',
        cases: ['渲染标题、说明与后端短句，只留一个重试动作'],
      },
    ],
  },
  {
    no: 6,
    trigger: '导航项点击',
    target: '对应视图',
    params: '无参数；全屏中忽略',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'navDock.test.ts', cases: ['点击视图键切到对应视图'] },
      {
        file: 'navDock.test.ts',
        cases: ['全屏配置页展开后只有主题与品牌两键'],
      },
    ],
  },
  {
    no: 7,
    trigger: '记忆 L1 卡「查看对话」',
    target: 'chat',
    params: "{sessionId, personaUid, fromView:'memory'}",
    scope: 'global',
    status: 'covered',
    coverage: [
      {
        file: 'memoryView.test.ts',
        cases: ['查看对话带会话与来源参数跳转，回记忆后按快照恢复人格与页签'],
      },
    ],
  },
  {
    no: 8,
    trigger: '对话「返回记忆」',
    target: 'memory',
    params: '恢复人格 / 页签 / 滚动位置',
    scope: 'global',
    status: 'covered',
    coverage: [
      {
        file: 'chatView.test.ts',
        cases: ['从记忆进入时给出返回入口，查询被一次性消费'],
      },
      {
        file: 'memoryView.test.ts',
        cases: ['查看对话带会话与来源参数跳转，回记忆后按快照恢复人格与页签'],
      },
    ],
  },
  {
    no: 9,
    trigger: '导入完成「前往记忆页面」',
    target: 'memory',
    params: '一次性 preselectPersonaUid',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'importView.test.ts', cases: ['完成页「前往记忆页面」预选导出者人格'] },
    ],
  },
  {
    no: 10,
    trigger: '导入完成「查看导入消息」',
    target: 'chat',
    params: '一次性导入会话模式',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'importView.test.ts', cases: ['完成页「查看导入消息」打开最后一个导入会话'] },
    ],
  },
  {
    no: 11,
    trigger: '主动消息通知点击',
    target: 'chat',
    params: "{sessionId, fromView:'proactive'}；已在 chat 则直接打开会话",
    scope: 'global',
    status: 'covered',
    coverage: [
      {
        file: 'hostWiring.test.ts',
        cases: ['非对话视图收到点击重播时切到对话并携带会话参数'],
      },
      { file: 'hostWiring.test.ts', cases: ['已在对话视图时不重复跳转'] },
    ],
  },
  {
    no: 12,
    trigger: '首次配置完成',
    target: '状态机路由',
    params: '写 appState=ready',
    scope: 'global',
    status: 'covered',
    coverage: [
      { file: 'appRouting.test.ts', cases: ['首次配置完成写 ready 后离开全屏页'] },
      {
        file: 'setupView.test.ts',
        cases: ['测试连接、跳过嵌入、提交并进入应用'],
      },
    ],
  },
  {
    no: 13,
    trigger: '人格详情操作',
    target: '页内二级',
    params: '不跨视图',
    scope: 'page',
    status: 'covered',
    coverage: [
      { file: 'personaView.test.ts', cases: ['进入详情与返回都在页内完成，不切视图'] },
    ],
  },
  {
    no: 14,
    trigger: '品牌键',
    target: '关于浮层',
    params: '不切视图；版本 / 检查更新 / 调试开关',
    scope: 'global',
    status: 'covered',
    coverage: [
      {
        file: 'navDock.test.ts',
        cases: ['关于浮层只留版本 / 更新 / 调试，导航位置不在其中'],
      },
    ],
  },
  {
    no: 15,
    trigger: '设置一 / 二级菜单',
    target: '页内子页面',
    params: '默认「基础设置 → 模型与检索」',
    scope: 'page',
    status: 'covered',
    coverage: [
      { file: 'settingsView.test.ts', cases: ['加载后渲染当前子页的卡片并回显配置'] },
      {
        file: 'settingsView.test.ts',
        cases: ['进阶未解锁时深链收敛回基础页，解锁后可直达'],
      },
    ],
  },
]
