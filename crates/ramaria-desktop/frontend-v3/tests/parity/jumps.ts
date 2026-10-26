/**
 * tests/parity/jumps.ts — 视图 A 相关的跨视图跳转矩阵条目
 *
 * 职责:
 * - 单一源声明 `frontend-spec-v2.md` §8 中与 memory / persona / import / chat 相关的跳转条目
 * - 每条绑定覆盖它的测试文件与用例名，供 jumps.test.ts 机器校验引用不漂移
 *
 * 设计特点:
 * - 条目编号、触发、目标与参数与规格 §8 的表格逐字一致，两份由测试对齐
 * - 不适用条目写 `na` 并给依据
 * - 一条跳转的两侧（触发端与落点端）可分别由不同用例覆盖，按文件逐条列出
 * - 只声明可观察的跳转行为，不写实现细节
 */

/** 跳转条目的覆盖情况 */
export type JumpStatus = 'covered' | 'na'

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
  status: JumpStatus
  /** 已覆盖条目的用例落点（触发端与落点端可各占一条） */
  coverage?: JumpCoverage[]
  /** 不适用条目的依据 */
  naReason?: string
}

/** 视图 A 涉及的跳转条目 */
export const JUMP_MATRIX: JumpEntry[] = [
  {
    no: 7,
    trigger: '记忆 L1 卡「查看对话」',
    target: 'chat',
    params: "{sessionId, personaUid, fromView:'memory'}",
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
    // 两侧分别覆盖：触发端在对话头部，落点端按记忆页快照恢复
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
    status: 'covered',
    coverage: [
      {
        file: 'importView.test.ts',
        cases: ['完成页「前往记忆页面」预选导出者人格'],
      },
    ],
  },
  {
    no: 10,
    trigger: '导入完成「查看导入消息」',
    target: 'chat',
    params: '一次性导入会话模式',
    status: 'covered',
    coverage: [
      {
        file: 'importView.test.ts',
        cases: ['完成页「查看导入消息」打开最后一个导入会话'],
      },
    ],
  },
  {
    no: 13,
    trigger: '人格详情操作',
    target: '页内二级',
    params: '不跨视图',
    status: 'covered',
    coverage: [
      {
        file: 'personaView.test.ts',
        cases: ['进入详情与返回都在页内完成，不切视图'],
      },
    ],
  },
]
