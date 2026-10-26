/**
 * src/core/debug/probe.ts — probe 产物解析（纯函数）
 *
 * 职责:
 * - 把 `ramaria probe run / evaluate / report` 产出的 JSON 提炼为调试视图可渲染的摘要
 *
 * 设计特点:
 * - 只读解析，不触发后端运行、不修改产物
 * - 通过特征键探测产物类型，多代结构差异一律宽松降级为空态
 * - 只取聚合分数与元信息：产物内的题面、参考答案与模型回复不进入视图（隐私红线）
 * - 档位得分顺序固定，标签与 2.9 口径一致，便于对照历史产物
 */

/** 产物类型 */
export type ArtifactKind = 'report' | 'evaluate' | 'run' | 'unknown'

/** 一条标量得分 */
export interface ArtifactScore {
  label: string
  value: number
  /** 逐轮聚合的样本说明（n 与 95% CI） */
  detail?: string
}

/** 一个档位 */
export interface ArtifactVariant {
  name: string
  /** 消融档位标识（B1 / F0 / I_behavior 等） */
  ablation: string | null
  /** 档位的注入闸门语义说明 */
  gateText: string | null
  scores: ArtifactScore[]
}

/** 产物元信息 */
export interface ArtifactMeta {
  kind: ArtifactKind
  personaUid: string | null
  datasetSeed: string | null
  generatedAt: string | null
  judgeUsed: boolean | null
  embeddingUsed: boolean | null
  resultsFile: string | null
}

/** 辅助指标 */
export interface ArtifactAuxMetric {
  label: string
  value: number
}

/** 产物摘要 */
export interface ArtifactSummary {
  kind: ArtifactKind
  meta: ArtifactMeta
  variants: ArtifactVariant[]
  auxiliary: ArtifactAuxMetric[] | null
  /** 是否识别出可展示的结构 */
  hasContent: boolean
}

/** 产物类型的中文标签 */
export const ARTIFACT_KIND_LABELS: Record<ArtifactKind, string> = {
  report: '消融报告',
  evaluate: '评估结果',
  run: '运行结果',
  unknown: '未知形态',
}

/** 档位标签：常见评分键 → 中文 */
const SCORE_LABELS: Record<string, string> = {
  fact_score: '事实',
  fact_score_norm: '事实(归一)',
  fact_score_point: '事实(事实点)',
  fact_mean: '事实均值',
  tone_score: '语气',
  tone_mean: '语气均值',
  emotion_score: '情绪',
  emotion_mean: '情绪均值',
  overall: '综合',
  score: '得分',
  mean: '均值',
}

/** 档位得分的展示顺序 */
const SCORE_ORDER = [
  'fact_score',
  'fact_score_norm',
  'fact_score_point',
  'fact_mean',
  'tone_score',
  'tone_mean',
  'emotion_score',
  'emotion_mean',
  'overall',
  'score',
  'mean',
]

/** 维度聚合的标签 */
const DIMENSION_LABELS: Record<string, string> = {
  fact: '事实',
  fact_norm: '事实(归一)',
  fact_point: '事实(事实点)',
  tone: '语气',
  emotion: '情绪',
}

/** 辅助指标四件套 */
const AUX_LABELS: Array<[string, string]> = [
  ['evidence_traceability_rate', '证据链可追溯率'],
  ['behavior_rule_hit_rate', '行为规则命中率'],
  ['situation_route_misuse_rate', '情境路由误用率'],
  ['profile_regression_output_stability', '画像回归稳定性'],
]

/**
 * 汇总单个产物 JSON。
 *
 * 参数:
 * - `value`: `read_eval_result` 返回的原始 JSON
 *
 * 返回:
 * - 类型、元信息、档位得分、辅助指标与可展示性标记；畸形输入返回空摘要
 */
export function summarizeArtifact(value: unknown): ArtifactSummary {
  const meta = extractMeta(value)
  const variants = extractVariants(value)
  const auxiliary = normalizeAuxiliary(recordAt(value, 'auxiliary'))
  return {
    kind: meta.kind,
    meta,
    variants,
    auxiliary,
    hasContent: variants.length > 0 || meta.personaUid !== null || auxiliary !== null,
  }
}

/** 识别产物类型：report / evaluate / run / unknown */
export function detectKind(value: unknown): ArtifactKind {
  const data = asRecord(value)
  if (!data) {
    return 'unknown'
  }
  if (hasAny(data, ['recommendation', 'knowledge_quality', 'auxiliary', 'ablation'])) {
    return 'report'
  }
  if ('judge_used' in data) {
    return 'evaluate'
  }
  if (hasAny(data, ['dataset_seed', 'generated_at']) && data.variants !== undefined) {
    return 'run'
  }
  return 'unknown'
}

/**
 * 档位标识 → 注入闸门语义。
 *
 * 参数:
 * - `ablation`: 档位消融标识
 *
 * 返回:
 * - 已知档位的中文说明；未知返回 null
 */
export function gateDescription(ablation: string | null): string | null {
  if (!ablation) {
    return null
  }
  if (ablation === 'B0') {
    return '无注入基线（全部关闭）'
  }
  if (ablation === 'B1') {
    return '完整基线（RAG + 四层）'
  }
  if (ablation === 'F0') {
    return '移除专属层（仅 RAG 基座）'
  }
  if (/^F[1-4]$/.test(ablation)) {
    return '移除对照（F 组单层移除）'
  }
  if (ablation.startsWith('I_')) {
    return `净增量：B1 基座 + ${ablation.slice(2)} 专属层`
  }
  if (ablation.startsWith('S_')) {
    return `替代对照：去 RAG 仅保留 ${ablation.slice(2)} 层`
  }
  return null
}

// =========================================================
// 私有辅助
// =========================================================

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

function hasAny(data: Record<string, unknown>, keys: string[]): boolean {
  return keys.some((key) => data[key] !== undefined && data[key] !== null)
}

/** 取首个存在且非空的字符串（数字按字符串处理） */
function firstText(data: Record<string, unknown>, keys: string[]): string | null {
  for (const key of keys) {
    const value = data[key]
    if (typeof value === 'string' && value !== '') {
      return value
    }
    if (typeof value === 'number' && Number.isFinite(value)) {
      return String(value)
    }
  }
  return null
}

function extractMeta(value: unknown): ArtifactMeta {
  const data = asRecord(value) ?? {}
  const judgeUsed = data.judge_used
  const embeddingUsed = data.embedding_used
  return {
    kind: detectKind(value),
    personaUid: firstText(data, ['persona_uid', 'personaUid']),
    datasetSeed: firstText(data, ['dataset_seed', 'datasetSeed']),
    generatedAt: firstText(data, ['generated_at', 'generatedAt']),
    judgeUsed: typeof judgeUsed === 'boolean' ? judgeUsed : null,
    embeddingUsed: typeof embeddingUsed === 'boolean' ? embeddingUsed : null,
    resultsFile: firstText(data, [
      'results_file',
      'resultsFile',
      'evaluation_file',
      'evaluationFile',
    ]),
  }
}

/** 档位列表：数组形态与「按档位名索引」的对象形态都接受 */
function extractVariants(value: unknown): ArtifactVariant[] {
  const data = asRecord(value)
  if (!data || data.variants === undefined || data.variants === null) {
    return []
  }
  const raw = data.variants
  const list: Array<{ item: Record<string, unknown>; fallbackName: string }> = []
  if (Array.isArray(raw)) {
    raw.forEach((item, index) => {
      const record = asRecord(item)
      if (record) {
        list.push({ item: record, fallbackName: `档位 ${index + 1}` })
      }
    })
  } else {
    const record = asRecord(raw)
    if (record) {
      for (const [key, item] of Object.entries(record)) {
        const child = asRecord(item)
        if (child) {
          list.push({ item: child, fallbackName: key })
        }
      }
    }
  }
  return list.map(({ item, fallbackName }) => buildVariant(item, fallbackName))
}

function buildVariant(item: Record<string, unknown>, fallbackName: string): ArtifactVariant {
  const params = asRecord(item.params)
  const ablation = params && typeof params.ablation === 'string' ? params.ablation : null
  const name =
    firstText(item, ['variant_id', 'variant', 'variantId', 'id', 'name']) ??
    ablation ??
    fallbackName
  return {
    name,
    ablation,
    gateText: gateDescription(ablation),
    scores: extractScores(item),
  }
}

/** 档位内的标量得分：固定键序 + 逐轮维度聚合 */
function extractScores(item: Record<string, unknown>): ArtifactScore[] {
  const scores: ArtifactScore[] = []
  for (const key of SCORE_ORDER) {
    const value = item[key]
    if (typeof value === 'number' && Number.isFinite(value)) {
      scores.push({ label: SCORE_LABELS[key] ?? key, value })
    }
  }
  scores.push(...dimensionScores(item.dimension_scores))
  return scores
}

/**
 * 逐轮维度聚合：`dimension_scores` 现为记录数组，旧产物可能是按维度索引的对象。
 *
 * 参数:
 * - `raw`: 产物中的 `dimension_scores` 字段
 *
 * 返回:
 * - 每个维度一条「（逐轮）」得分，附样本量与置信区间说明
 */
function dimensionScores(raw: unknown): ArtifactScore[] {
  const scores: ArtifactScore[] = []
  if (Array.isArray(raw)) {
    for (const item of raw) {
      const record = asRecord(item)
      if (!record) {
        continue
      }
      const dimension = typeof record.dimension === 'string' ? record.dimension : ''
      const mean = record.mean
      if (typeof mean === 'number' && Number.isFinite(mean)) {
        scores.push({
          label: `${DIMENSION_LABELS[dimension] ?? dimension}（逐轮）`,
          value: mean,
          detail: dimensionDetail(record),
        })
      }
    }
    return scores
  }
  const record = asRecord(raw)
  if (!record) {
    return scores
  }
  for (const [key, item] of Object.entries(record)) {
    const child = asRecord(item)
    if (!child) {
      continue
    }
    const mean = child.mean
    if (typeof mean === 'number' && Number.isFinite(mean)) {
      scores.push({
        label: `${DIMENSION_LABELS[key] ?? key}（逐轮）`,
        value: mean,
        detail: dimensionDetail(child),
      })
    }
  }
  return scores
}

/** 维度聚合的样本说明：n 与 95% CI */
function dimensionDetail(record: Record<string, unknown>): string | undefined {
  const n = typeof record.n === 'number' ? String(record.n) : '?'
  const low = record.ci95_low
  const high = record.ci95_high
  if (typeof low === 'number' && typeof high === 'number') {
    return `n=${n} CI[${round(low)}, ${round(high)}]`
  }
  return `n=${n}`
}

function round(value: number): number {
  return Math.round(value * 1000) / 1000
}

/** 辅助指标四件套：只收数值项 */
export function normalizeAuxiliary(
  auxiliary: Record<string, unknown> | null,
): ArtifactAuxMetric[] | null {
  if (!auxiliary) {
    return null
  }
  const metrics: ArtifactAuxMetric[] = []
  for (const [key, label] of AUX_LABELS) {
    const value = auxiliary[key]
    if (typeof value === 'number' && Number.isFinite(value)) {
      metrics.push({ label, value })
    }
  }
  return metrics.length > 0 ? metrics : null
}
