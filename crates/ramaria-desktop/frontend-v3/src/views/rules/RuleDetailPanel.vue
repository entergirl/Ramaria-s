<script setup lang="ts">
/**
 * src/views/rules/RuleDetailPanel.vue — 规则详情内容
 *
 * 职责:
 * - 展示并就地编辑规则文本与禁忌词，保存动作由工具栏发起
 * - 展示结构化参数、情境统计与元数据
 * - 展示证据链（事件 id / 权重 / 标题 / 摘要 / 态度）
 *
 * 设计特点:
 * - 规则文本用文本框承载，高度吃掉左列剩余空间，随两列高度变化
 * - 左列两张卡（规则 / 证据链）与右列参数卡等高；证据与参数各自内部滚动并渐隐
 * - 校验与保存失败共用一行提示，避免固定高度卡片被撑开
 * - 只呈现已有字段，不推测缺失数据
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'
import BaseInput from '../../components/base/BaseInput.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import FadeScroll from '../../components/shell/FadeScroll.vue'
import type { BehaviorRuleView } from '../../core/api/dto'
import { formatDateTime, smartTime } from '../../core/chat/time'
import { amount, decimal, presentationLabel, signedDecimal } from '../../core/memory/format'
import {
  evidenceKeywords,
  isCandidateRule,
  ruleKeywords,
  ruleSourceLabel,
} from '../../core/rules/format'
import type { EvidenceState } from './useRulesView'

const props = defineProps<{
  rule: BehaviorRuleView
  evidence: EvidenceState
  reaction: string
  avoid: string
  draftSaving: boolean
  validation: string
  error: string
}>()

const emit = defineEmits<{
  'update:reaction': [value: string]
  'update:avoid': [value: string]
  'retry-evidence': []
}>()

const candidate = computed(() => isCandidateRule(props.rule))
const keywords = computed(() => ruleKeywords(props.rule))
const sourceLabel = computed(() => ruleSourceLabel(props.rule.source))
const createdText = computed(() => formatDateTime(props.rule.created_at))
const updatedText = computed(() => smartTime(props.rule.updated_at))
const updatedFull = computed(() => formatDateTime(props.rule.updated_at))
const evidenceCount = computed(() => (props.evidence.loaded ? props.evidence.items.length : null))

/** 一行提示：保存失败 > 字段校验 > 常规说明 */
const hint = computed(() => {
  if (props.error !== '') {
    return props.error
  }
  if (props.validation !== '') {
    return props.validation
  }
  return candidate.value
    ? '这是候选规则：文本可留空，只保存禁忌词；填写文本后会转为手工规则。'
    : '保存后转为手工规则（Manual 强锚点），优先级高于自动学习。'
})

const hintTone = computed(() =>
  props.error !== '' || props.validation !== '' ? 'is-error' : '',
)

/** 簇内占比最高的陈述方式（无分布时不显示） */
const topPresentation = computed(() => {
  const dist = props.rule.situation.presentation_dist
  if (dist.length === 0) {
    return null
  }
  const top = dist.reduce((best, item) => (item.freq > best.freq ? item : best), dist[0])
  return { label: presentationLabel(top.presentation), percent: Math.round(top.freq * 100) }
})

function onReactionInput(event: Event): void {
  emit('update:reaction', (event.target as HTMLTextAreaElement).value)
}
</script>

<template>
  <div class="detail-grid">
    <div class="col-main">
      <BaseCard class="detail-card rule-edit-card">
        <header class="section-head">
          <h3 class="section-title">规则内容</h3>
          <span class="section-note">可直接编辑后保存</span>
        </header>

        <div class="rule-body">
          <textarea
            id="rule-detail-input"
            class="rule-input"
            :value="reaction"
            :disabled="draftSaving"
            placeholder="该人格在此情境下应如何回应"
            aria-label="规则文本"
            @input="onReactionInput"
          />
          <p class="field-hint" :class="hintTone">{{ hint }}</p>
          <BaseInput
            label="禁忌词"
            :model-value="avoid"
            hint="用逗号分隔；该人格在此情境下应避免的话题。"
            :disabled="draftSaving"
            @update:model-value="$emit('update:avoid', $event)"
          />
        </div>
      </BaseCard>

      <BaseCard class="detail-card evidence-card">
        <header class="section-head">
          <h3 class="section-title">证据链</h3>
          <span class="section-note">
            {{ evidenceCount === null ? '规则 → 事件' : `共 ${evidenceCount} 条` }}
          </span>
        </header>

        <FadeScroll class="evidence-scroll">
          <p v-if="evidence.loading" class="hint">读取证据链…</p>
          <div v-else-if="evidence.error" class="error-strip" role="alert">
            <span class="error-text">{{ evidence.error }}</span>
            <BaseButton variant="ghost" size="sm" @click="$emit('retry-evidence')">重试</BaseButton>
          </div>
          <p v-else-if="evidence.items.length === 0" class="hint">
            这条规则还没有关联事件证据；手工编辑或导入的规则通常没有证据链。
          </p>
          <div v-else class="evidence">
            <article v-for="item in evidence.items" :key="item.event_id" class="ev-item">
              <header class="ev-head">
                <span class="ev-id">事件 #{{ item.event_id }}</span>
                <span class="ev-weight">权重 {{ decimal(item.weight) }}</span>
              </header>
              <p class="ev-title">{{ item.title }}</p>
              <p class="ev-summary">{{ item.summary }}</p>
              <p v-if="item.paraphrase" class="ev-para">态度（脱敏）：{{ item.paraphrase }}</p>
              <div v-if="evidenceKeywords(item).length" class="chips">
                <BaseTag v-for="word in evidenceKeywords(item)" :key="word" tone="neutral">
                  {{ word }}
                </BaseTag>
              </div>
            </article>
          </div>
        </FadeScroll>
      </BaseCard>
    </div>

    <BaseCard class="detail-card params-card">
      <header class="section-head">
        <h3 class="section-title">参数与统计</h3>
      </header>

      <FadeScroll class="params-scroll">
        <div class="kv"><span class="k">置信度</span><b>{{ decimal(rule.confidence) }}</b></div>
        <div class="kv"><span class="k">稳定性</span><b>{{ decimal(rule.stability) }}</b></div>
        <div class="kv">
          <span class="k">情感强度</span><b>{{ decimal(rule.params.emotional_intensity) }}</b>
        </div>
        <div class="kv">
          <span class="k">主动程度</span><b>{{ decimal(rule.params.proactiveness) }}</b>
        </div>
        <div class="kv">
          <span class="k">详细度</span><b>{{ decimal(rule.params.detail_level) }}</b>
        </div>
        <div class="kv"><span class="k">正式度</span><b>{{ decimal(rule.params.formality) }}</b></div>

        <span class="rule-line" aria-hidden="true" />

        <div class="kv"><span class="k">样本数</span><b>{{ rule.situation.sample_count }}</b></div>
        <div class="kv">
          <span class="k">时间跨度</span><b>{{ amount(rule.situation.time_span_days) }} 天</b>
        </div>
        <div class="kv">
          <span class="k">情境强度</span><b>{{ decimal(rule.situation.situation_strength_mean) }}</b>
        </div>
        <div class="kv">
          <span class="k">效价</span>
          <b>{{ signedDecimal(rule.situation.valence_mean) }} ± {{ decimal(rule.situation.valence_std) }}</b>
        </div>
        <div v-if="topPresentation" class="kv">
          <span class="k">陈述方式</span>
          <b>{{ topPresentation.label }} {{ topPresentation.percent }}%</b>
        </div>

        <div v-if="keywords.length" class="params-note">
          <span class="k">情境关键词</span>
          <div class="chips">
            <BaseTag v-for="word in keywords" :key="word" tone="neutral">{{ word }}</BaseTag>
          </div>
        </div>

        <span class="rule-line" aria-hidden="true" />

        <div class="kv"><span class="k">来源</span><b>{{ sourceLabel }}</b></div>
        <div class="kv"><span class="k">创建</span><b>{{ createdText }}</b></div>
        <div class="kv">
          <span class="k">更新</span><b :title="updatedFull">{{ updatedText }}</b>
        </div>
      </FadeScroll>
    </BaseCard>
  </div>
</template>

<style scoped>
.detail-grid {
  display: grid;
  flex: 1;
  grid-template-columns: minmax(0, 7fr) minmax(0, 5fr);
  gap: var(--space-10);
  align-items: stretch;
  min-height: 0;
}

.col-main {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  min-height: 0;
}

.detail-card {
  min-height: 0;
  gap: var(--space-8);
  padding: var(--space-14) var(--space-16);
}

/* 左列两张卡按比例分掉整列高度，文本框随列高伸缩 */
.rule-edit-card {
  flex: 1.25 1 0;
}

.evidence-card {
  flex: 1 1 0;
}

.params-card {
  height: 100%;
}

.section-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
}

.section-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: 1.35;
}

.section-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.rule-body {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-6);
  min-height: 0;
}

.rule-input {
  flex: 1;
  width: 100%;
  min-height: 64px;
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border-strong);
  border-radius: var(--radius-input);
  background: var(--bg-subtle);
  color: var(--text-1);
  font-family: inherit;
  font-size: var(--fs-body);
  line-height: 1.7;
  resize: none;
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.rule-input:hover:not(:disabled) {
  border-color: var(--brand-line);
}

.rule-input:focus-visible {
  outline: none;
  border-color: var(--brand);
  box-shadow: var(--focus-ring);
}

.rule-input:disabled {
  opacity: var(--opacity-disabled);
}

.field-hint {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: var(--lh-caption);
}

.field-hint.is-error {
  color: var(--danger);
}

.evidence-scroll,
.params-scroll {
  flex: 1;
  min-height: 0;
}

.params-scroll {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
}

.evidence {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.ev-item {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.ev-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
}

.ev-id {
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.ev-weight {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.ev-title {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.ev-summary,
.ev-para {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.ev-para {
  color: var(--text-3);
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-4);
}

.hint {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.error-strip {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-inner);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.error-text {
  flex: 1;
  min-width: 0;
}

.kv {
  display: flex;
  gap: var(--space-8);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.kv .k {
  flex: none;
  width: 64px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.kv b {
  min-width: 0;
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.rule-line {
  height: 1px;
  background: var(--border);
}

.params-note {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.params-note .k {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

@media (max-width: 860px) {
  .detail-grid {
    grid-template-columns: minmax(0, 1fr);
    overflow-y: auto;
  }

  .rule-edit-card,
  .evidence-card {
    flex: none;
  }

  .rule-input {
    min-height: 96px;
  }

  .evidence-card {
    min-height: 220px;
  }
}
</style>
