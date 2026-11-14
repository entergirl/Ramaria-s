/**
 * src/core/debug/eval.ts — 评测报告解析（纯函数）
 *
 * 职责:
 * - 把 `ramaria eval report` 产出的 `report.json` 提炼为调试视图可渲染的摘要
 *
 * 设计特点:
 * - 只读解析，不触发后端运行、不修改产物
 * - 只按顶层 `artifact_kind` + 数字 `schema_version` 识别；其它 JSON 一律降级为空态
 * - 只取聚合分数与元信息：题面、参考答案与模型回复不进入视图（隐私红线）
 * - 报告按条件行展开，结论只保留编号、状态与一句话
 */

/** 产物类型 */
export type ArtifactKind = 'eval_report' | 'unknown'

/** 评测报告的产物标识（顶层 artifact_kind） */
const EVAL_REPORT_KIND = 'ramaria_eval_report'

/** 一条标量得分 */
export interface ArtifactScore {
  label: string
  value: number
  /** 逐轮聚合的样本说明（n 与 95% CI） */
  detail?: string
}

/** 一个条件 */
export interface ArtifactVariant {
  name: string
  /** 条件闸门说明：管线与预算 */
  gateText: string | null
  scores: ArtifactScore[]
}

/** 产物元信息 */
export interface ArtifactMeta {
  kind: ArtifactKind
  /** 判分模型是否已配置（报告只记状态，不取模型名与模板） */
  judgeUsed: boolean | null
}

/** 辅助指标 */
export interface ArtifactAuxMetric {
  label: string
  value: number
  /** 计量方式：比例（0..1）或计数；缺省按比例展示 */
  unit?: 'ratio' | 'count'
}

/** 评测报告的一条结论 */
export interface ArtifactHypothesis {
  id: string
  /** supported / partial / not_evidenced */
  status: string
  /** 一句话结论（不含题目正文与回复） */
  summary: string
}

/** 产物摘要 */
export interface ArtifactSummary {
  kind: ArtifactKind
  meta: ArtifactMeta
  variants: ArtifactVariant[]
  auxiliary: ArtifactAuxMetric[] | null
  /** 评测报告的 H 结论；未提供时为 null */
  hypotheses: ArtifactHypothesis[] | null
  /** 是否识别出可展示的结构 */
  hasContent: boolean
}

/** 产物类型的中文标签 */
export const ARTIFACT_KIND_LABELS: Record<ArtifactKind, string> = {
  eval_report: '评测报告',
  unknown: '未知形态',
}

/** 结论状态的中文标签 */
export const HYPOTHESIS_STATUS_LABELS: Record<string, string> = {
  supported: '支持',
  partial: '部分',
  not_evidenced: '未取证',
}

/** 条件行的得分标签 */
const EVAL_SCORE_LABELS = {
  meanRatio: '得分率',
  coveragePost: '覆盖(后)',
  fullHit: '全要点命中',
} as const

/**
 * 汇总单个产物 JSON。
 *
 * 参数:
 * - `value`: `read_eval_result` 返回的原始 JSON
 *
 * 返回:
 * - 评测报告摘要；非评测报告返回空摘要（不猜测旧结构）
 */
export function summarizeArtifact(value: unknown): ArtifactSummary {
  if (detectKind(value) !== 'eval_report') {
    return emptySummary('unknown')
  }
  return summarizeEvalReport(value)
}

/** 识别产物类型：只认带数字 schema_version 的评测报告 */
export function detectKind(value: unknown): ArtifactKind {
  const data = asRecord(value)
  if (!data) {
    return 'unknown'
  }
  if (data.artifact_kind === EVAL_REPORT_KIND && typeof data.schema_version === 'number') {
    return 'eval_report'
  }
  return 'unknown'
}

// =========================================================
// 私有辅助
// =========================================================

function emptySummary(kind: ArtifactKind): ArtifactSummary {
  return {
    kind,
    meta: { kind, judgeUsed: null },
    variants: [],
    auxiliary: null,
    hypotheses: null,
    hasContent: false,
  }
}

function asRecord(value: unknown): Record<string, unknown> | null {
  if (!value || typeof value !== 'object' || Array.isArray(value)) {
    return null
  }
  return value as Record<string, unknown>
}

function recordAt(value: unknown, key: string): Record<string, unknown> | null {
  const data = asRecord(value)
  return data ? asRecord(data[key]) : null
}

/** 按 key 取对象数组，非对象元素跳过 */
function recordsAt(data: Record<string, unknown>, key: string): Record<string, unknown>[] {
  const raw = data[key]
  if (!Array.isArray(raw)) {
    return []
  }
  const list: Record<string, unknown>[] = []
  for (const item of raw) {
    const record = asRecord(item)
    if (record) {
      list.push(record)
    }
  }
  return list
}

/**
 * 汇总评测报告（report.json）。
 *
 * 参数:
 * - `value`: 报告原始 JSON
 *
 * 返回:
 * - 每个条件一档，结论另置；判分模型只看是否已配置，不取模型名与模板
 */
function summarizeEvalReport(value: unknown): ArtifactSummary {
  const data = asRecord(value) ?? {}
  const judge = asRecord(recordAt(data, 'source')?.judge)
  const conditions = recordsAt(data, 'conditions')
  const comparisons = recordsAt(data, 'comparisons')
  const missing = Array.isArray(data.missing) ? data.missing : []
  const variants = conditions.map((item, index) => buildEvalVariant(item, index))
  const hypotheses = extractHypotheses(data.hypotheses)
  const significant = comparisons.filter((row) => row.significant === true).length
  const auxiliary: ArtifactAuxMetric[] = [
    { label: '条件数', value: conditions.length, unit: 'count' },
    { label: '显著比较数', value: significant, unit: 'count' },
    { label: '缺项数', value: missing.length, unit: 'count' },
  ]
  const judgeStatus = judge?.status
  return {
    kind: 'eval_report',
    meta: {
      kind: 'eval_report',
      judgeUsed: typeof judgeStatus === 'string' ? judgeStatus === 'configured' : null,
    },
    variants,
    auxiliary,
    hypotheses,
    hasContent: variants.length > 0 || (hypotheses?.length ?? 0) > 0,
  }
}

/** 条件行：名称取 condition_id，闸门写管线与预算字符数 */
function buildEvalVariant(item: Record<string, unknown>, index: number): ArtifactVariant {
  const rubric = asRecord(item.rubric)
  const retrieval = asRecord(item.retrieval)
  const artifact = retrieval ? asRecord(retrieval.artifact) : null
  const scores: ArtifactScore[] = []
  pushFiniteScore(scores, EVAL_SCORE_LABELS.meanRatio, rubric?.micro_mean_ratio)
  pushFiniteScore(scores, EVAL_SCORE_LABELS.coveragePost, artifact?.coverage_post)
  pushFiniteScore(scores, EVAL_SCORE_LABELS.fullHit, rubric?.micro_full_hit_rate)
  const name =
    typeof item.condition_id === 'string' && item.condition_id !== ''
      ? item.condition_id
      : `条件 ${index + 1}`
  return { name, gateText: evalGateText(item), scores }
}

/** 缺值不塞 0，直接跳过该项 */
function pushFiniteScore(scores: ArtifactScore[], label: string, value: unknown): void {
  if (typeof value === 'number' && Number.isFinite(value)) {
    scores.push({ label, value })
  }
}

/** 条件闸门说明：管线 / 预算字符数 */
function evalGateText(item: Record<string, unknown>): string | null {
  const pipeline = typeof item.pipeline === 'string' ? item.pipeline : ''
  const budget =
    typeof item.budget_chars === 'number' && Number.isFinite(item.budget_chars)
      ? `${item.budget_chars} 字符`
      : ''
  const parts = [pipeline, budget].filter((part) => part !== '')
  return parts.length > 0 ? parts.join(' / ') : null
}

/** 结论列表：只收编号、状态与一句话；空列表返回 null */
function extractHypotheses(raw: unknown): ArtifactHypothesis[] | null {
  if (!Array.isArray(raw)) {
    return null
  }
  const list: ArtifactHypothesis[] = []
  for (const item of raw) {
    const record = asRecord(item)
    if (!record || typeof record.id !== 'string' || record.id === '') {
      continue
    }
    list.push({
      id: record.id,
      status: typeof record.status === 'string' ? record.status : '',
      summary: typeof record.summary === 'string' ? record.summary : '',
    })
  }
  return list.length > 0 ? list : null
}
