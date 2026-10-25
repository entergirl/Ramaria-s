<script setup lang="ts">
/**
 * src/views/dev/RulesPreview.vue — 规则页的开发期预览
 *
 * 职责:
 * - 用内存夹具接管 Tauri 命令，让规则视图在浏览器里可交互预览
 * - 提供场景切换：默认 / 空 / 加载 / 错误 / 降级 / 证据空 / 证据失败
 *
 * 设计特点:
 * - 仅经 `import.meta.env.DEV` 路由挂载，生产构建不进入应用
 * - 夹具数据在内存中可写：启停与编辑会真实改写，便于验证就地更新
 * - 每次切换场景按 key 重挂规则视图，等价于重新进入页面
 */
import { onBeforeUnmount, ref } from 'vue'

import type {
  BehaviorRuleView,
  PersonaSummary,
  RuleEvidenceItem,
} from '../../core/api/dto'
import { setTauriRuntime, type TauriRuntime } from '../../core/transport'
import { useAppStore } from '../../stores/app'
import RulesView from '../rules/index.vue'

type Scenario =
  | 'default'
  | 'empty'
  | 'slow'
  | 'error'
  | 'degraded'
  | 'evidence-empty'
  | 'evidence-error'

const SCENARIOS: Array<{ value: Scenario; label: string }> = [
  { value: 'default', label: '默认' },
  { value: 'empty', label: '空' },
  { value: 'slow', label: '加载' },
  { value: 'error', label: '错误' },
  { value: 'degraded', label: '降级·llm' },
  { value: 'evidence-empty', label: '证据为空' },
  { value: 'evidence-error', label: '证据失败' },
]

const NOW = Date.now()

const personas: PersonaSummary[] = [
  {
    uid: 'rama-0001',
    name: '小珊',
    kind: 'rama',
    source: 'manual',
    is_active: true,
    created_at: NOW - 86_400_000 * 40,
  },
  {
    uid: 'char-1001',
    name: '林霁',
    kind: 'char',
    source: 'qq',
    is_active: true,
    created_at: NOW - 86_400_000 * 20,
  },
]

function makeRule(partial: Partial<BehaviorRuleView>): BehaviorRuleView {
  return {
    id: 0,
    persona_uid: 'rama-0001',
    situation: {
      keywords: [],
      centroid: null,
      response_centroid: null,
      valence_mean: 0,
      valence_std: 0,
      sample_count: 0,
      presentation_dist: [],
      situation_strength_mean: 3,
      time_span_days: 0,
      trait_refs: [],
    },
    reaction: null,
    params: {
      emotional_intensity: 0,
      proactiveness: 0.5,
      detail_level: 0.5,
      formality: 0.5,
    },
    avoid: [],
    evidence: [],
    confidence: 0.5,
    stability: 0.5,
    source: 'auto',
    enabled: true,
    created_at: NOW - 86_400_000 * 6,
    updated_at: NOW - 3_600_000,
    ...partial,
  }
}

/** 可写夹具：启停与编辑会就地改写，模拟真实落库 */
const data: Record<string, BehaviorRuleView[]> = {
  'rama-0001': [
    makeRule({
      id: 101,
      source: 'manual',
      reaction: '深夜 23:00 之后不要主动发起需要高强度思考的话题，除非用户先开口。',
      confidence: 0.91,
      stability: 0.8,
      situation: {
        ...makeRule({}).situation,
        keywords: ['夜间', '工作', '边界'],
        sample_count: 8,
        valence_mean: 0.12,
        valence_std: 0.08,
        situation_strength_mean: 3.4,
        time_span_days: 21,
        presentation_dist: [{ presentation: 'subjective', freq: 0.62 }],
      },
      params: {
        emotional_intensity: 0.42,
        proactiveness: 0.3,
        detail_level: 0.55,
        formality: 0.4,
      },
      avoid: ['说教'],
      evidence: [
        { event_id: 1042, weight: 0.9 },
        { event_id: 1180, weight: 0.35 },
      ],
    }),
    makeRule({
      id: 102,
      reaction: null,
      confidence: 0.55,
      stability: 0.62,
      situation: {
        ...makeRule({}).situation,
        keywords: ['倾诉', '情绪'],
        sample_count: 4,
        valence_mean: -0.28,
        valence_std: 0.11,
        situation_strength_mean: 4.1,
        time_span_days: 9,
        presentation_dist: [{ presentation: 'mixed', freq: 0.48 }],
      },
      params: {
        emotional_intensity: 0.18,
        proactiveness: 0.72,
        detail_level: 0.4,
        formality: 0.25,
      },
      evidence: [{ event_id: 992, weight: 0.6 }],
    }),
    makeRule({
      id: 103,
      enabled: false,
      confidence: 0.48,
      stability: 0.31,
      reaction: '涉及商业话题时优先给出结构化利弊清单。',
      situation: {
        ...makeRule({}).situation,
        keywords: ['商业', '决策'],
        sample_count: 3,
      },
      params: {
        emotional_intensity: 0.05,
        proactiveness: 0.45,
        detail_level: 0.85,
        formality: 0.7,
      },
      avoid: ['空话'],
    }),
    makeRule({
      id: 104,
      source: 'manual',
      reaction: '回复保持温和语气，句长偏短，多用「呢」「呀」等软化词。',
      confidence: 0.77,
      stability: 0.66,
      situation: { ...makeRule({}).situation, keywords: [], sample_count: 0 },
      params: {
        emotional_intensity: 0.3,
        proactiveness: 0.4,
        detail_level: 0.3,
        formality: 0.2,
      },
      evidence: [{ event_id: 875, weight: 0.5 }],
    }),
    makeRule({
      id: 105,
      confidence: 0.63,
      stability: 0.52,
      reaction:
        '遇到用户连续两次以上提到同一件烦心事时，先复述一遍他的原话再给建议，不要急着安慰，也不要把话题引向自己。',
      situation: {
        ...makeRule({}).situation,
        keywords: ['反复', '烦心事', '复述', '建议', '安慰', '倾听', '同伴', '耐心'],
        sample_count: 6,
        valence_mean: -0.15,
        valence_std: 0.2,
        situation_strength_mean: 3.8,
        time_span_days: 15,
        presentation_dist: [{ presentation: 'objective', freq: 0.55 }],
      },
      params: {
        emotional_intensity: 0.22,
        proactiveness: 0.35,
        detail_level: 0.6,
        formality: 0.45,
      },
      evidence: [{ event_id: 1103, weight: 0.7 }],
    }),
  ],
  'char-1001': [
    makeRule({
      id: 201,
      persona_uid: 'char-1001',
      source: 'manual',
      reaction: '聊到专业内容时先给结论，再补两句依据。',
      confidence: 0.82,
      stability: 0.74,
      situation: {
        ...makeRule({}).situation,
        keywords: ['专业', '结论'],
        sample_count: 5,
      },
      evidence: [{ event_id: 1310, weight: 0.8 }],
    }),
    makeRule({
      id: 202,
      persona_uid: 'char-1001',
      enabled: false,
      confidence: 0.4,
      stability: 0.28,
      reaction: null,
      situation: { ...makeRule({}).situation, keywords: ['玩笑'] },
    }),
  ],
}

const evidenceMap: Record<number, RuleEvidenceItem[]> = {
  101: [
    {
      event_id: 1042,
      weight: 0.9,
      title: '深夜加班后的对话',
      summary: '夜里聊到工作压力，用户明确表示不想再听大道理。',
      paraphrase: '先接住情绪，不给方案',
      keywords: '夜间,工作',
    },
    {
      event_id: 1180,
      weight: 0.35,
      title: '睡前闲聊',
      summary: '用户提到第二天要早起，希望对话收得干脆一些。',
      paraphrase: '收尾干脆',
      keywords: '睡前,早起',
    },
  ],
  102: [
    {
      event_id: 992,
      weight: 0.6,
      title: '情绪低落的一次倾诉',
      summary: '用户连续说了几件不顺心的事，情绪偏沉。',
      paraphrase: null,
      keywords: '倾诉,情绪',
    },
  ],
  104: [
    {
      event_id: 875,
      weight: 0.5,
      title: '关于语气的反馈',
      summary: '用户说更喜欢短句和软一点的语气。',
      paraphrase: '短句 + 软化词',
      keywords: '语气,句长',
    },
  ],
  105: [
    {
      event_id: 1103,
      weight: 0.7,
      title: '重复提起的烦心事',
      summary: '同一件烦心事在一周内被提到三次，用户希望被听见而不是被劝。',
      paraphrase: '先复述再回应',
      keywords: '反复,倾听',
    },
  ],
  201: [
    {
      event_id: 1310,
      weight: 0.8,
      title: '专业问题咨询',
      summary: '用户问了一个专业问题，并追问依据。',
      paraphrase: '结论先行',
      keywords: '专业,结论',
    },
  ],
}

const scenario = ref<Scenario>('default')
const generation = ref(0)
const app = useAppStore()
let evidenceFailed = false

function clone<T>(value: T): T {
  return JSON.parse(JSON.stringify(value)) as T
}

function sleep(ms: number): Promise<void> {
  return new Promise((resolve) => {
    window.setTimeout(resolve, ms)
  })
}

function rulesOf(uid: string | null): BehaviorRuleView[] {
  return data[uid ?? 'rama-0001'] ?? []
}

function findRule(id: number): BehaviorRuleView | null {
  for (const list of Object.values(data)) {
    const hit = list.find((rule) => rule.id === id)
    if (hit) {
      return hit
    }
  }
  return null
}

const runtime: TauriRuntime = {
  core: {
    invoke: async (command, args = {}) => {
      switch (command) {
        case 'get_personas':
          return clone(personas)
        case 'list_rules': {
          const uid = (args.personaUid as string | null) ?? null
          if (scenario.value === 'error') {
            throw new Error('本地数据库被占用，读取失败')
          }
          if (scenario.value === 'slow') {
            await sleep(1200)
          }
          const list = scenario.value === 'empty' ? [] : rulesOf(uid)
          return { persona_uid: uid ?? 'rama-0001', total: list.length, rules: clone(list) }
        }
        case 'set_rule_enabled': {
          const id = args.ruleId as number
          const enabled = args.enabled as boolean
          const rule = findRule(id)
          if (!rule) {
            throw new Error('规则不存在')
          }
          rule.enabled = enabled
          rule.updated_at = Date.now()
          return { id, enabled }
        }
        case 'edit_rule': {
          const id = args.ruleId as number
          const rule = findRule(id)
          if (!rule) {
            throw new Error('规则不存在')
          }
          if (args.reaction !== null) {
            rule.reaction = args.reaction as string
          }
          if (args.avoid !== null) {
            rule.avoid = String(args.avoid)
              .split(',')
              .filter((item) => item.length > 0)
          }
          rule.source = 'manual'
          rule.updated_at = Date.now()
          return clone(rule)
        }
        case 'rule_evidence': {
          const id = args.ruleId as number
          if (scenario.value === 'evidence-empty') {
            return { rule_id: id, evidence: [] }
          }
          if (scenario.value === 'evidence-error' && !evidenceFailed) {
            evidenceFailed = true
            throw new Error('证据链读取超时，请重试')
          }
          return { rule_id: id, evidence: clone(evidenceMap[id] ?? []) }
        }
        default:
          throw new Error(`预览未接入命令: ${command}`)
      }
    },
  },
  event: {
    listen: async () => () => {},
    emit: async () => {},
  },
}

setTauriRuntime(runtime)

function applyScenario(next: Scenario): void {
  scenario.value = next
  generation.value += 1
  evidenceFailed = false
  app.degradedReason = next === 'degraded' ? 'llm_unavailable' : null
}

onBeforeUnmount(() => {
  setTauriRuntime(null)
  app.degradedReason = null
})
</script>

<template>
  <div class="preview">
    <header class="dev-bar">
      <span class="dev-title">规则页预览 · 临时数据</span>
      <div class="dev-actions">
        <button
          v-for="item in SCENARIOS"
          :key="item.value"
          type="button"
          class="dev-chip"
          :class="{ 'is-active': scenario === item.value }"
          @click="applyScenario(item.value)"
        >
          {{ item.label }}
        </button>
      </div>
      <span class="dev-note">可试：切人格 / 开关启停 / 编辑 / 详情看证据</span>
    </header>

    <div class="stage">
      <RulesView :key="`${scenario}-${generation}`" />
    </div>
  </div>
</template>

<style scoped>
.preview {
  display: flex;
  flex-direction: column;
  height: 100vh;
}

.dev-bar {
  display: flex;
  flex: none;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-8) var(--space-16);
  border-bottom: 1px solid var(--border);
  background: var(--bg-card);
}

.dev-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.dev-actions {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
}

.dev-chip {
  height: 26px;
  padding: 0 var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-btn-sm);
  background: var(--bg-card);
  color: var(--text-2);
  font-family: inherit;
  font-size: var(--fs-caption);
  cursor: pointer;
}

.dev-chip:hover {
  border-color: var(--brand-line);
  color: var(--brand-text);
}

.dev-chip:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.dev-chip.is-active {
  border-color: var(--brand);
  background: var(--brand-surface);
  color: var(--brand-text);
  font-weight: var(--fw-semibold);
}

.dev-note {
  margin-left: auto;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.stage {
  flex: 1;
  min-height: 0;
}

.stage :deep(.workbench) {
  height: 100%;
}
</style>
