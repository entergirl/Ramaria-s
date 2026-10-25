<script setup lang="ts">
/**
 * src/views/rules/RuleCard.vue — 规则卡片
 *
 * 职责:
 * - 展示单条规则的来源、启停、规则文本、情境关键词与元信息
 * - 承载启停开关与进入详情的入口
 *
 * 设计特点:
 * - 卡片本体不可点，避免选中文本时误触发；动作一律走显式按钮
 * - 列表为双列栅格：留白收紧，行内卡片等高，正文过长的行由整行拉伸吸收
 * - 候选规则（无规则文本）显示固定占位，并单独标注「仅参数注入」
 * - 停用态用底色与状态标签表达，不整体降透明度，保证正文可读
 * - 编辑不进列表，统一在详情页的文本框里完成
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseSwitch from '../../components/base/BaseSwitch.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { BehaviorRuleView } from '../../core/api/dto'
import { decimal } from '../../core/memory/format'
import {
  isCandidateRule,
  ruleKeywords,
  ruleSourceLabel,
  ruleSourceTone,
  ruleStatusLabel,
  ruleText,
} from '../../core/rules/format'

const props = defineProps<{
  rule: BehaviorRuleView
  saving: boolean
}>()

defineEmits<{
  'open-detail': []
  toggle: []
}>()

const sourceLabel = computed(() => ruleSourceLabel(props.rule.source))
const sourceTone = computed(() => ruleSourceTone(props.rule.source))
const statusLabel = computed(() => ruleStatusLabel(props.rule.enabled))
const candidate = computed(() => isCandidateRule(props.rule))
const text = computed(() => ruleText(props.rule))
const keywords = computed(() => ruleKeywords(props.rule))
const evidenceCount = computed(() => props.rule.evidence.length)
</script>

<template>
  <article class="rule-card" :class="{ 'is-off': !rule.enabled }">
    <header class="head">
      <BaseTag :tone="sourceTone">{{ sourceLabel }}</BaseTag>
      <BaseTag tone="neutral">{{ statusLabel }}</BaseTag>
      <BaseTag v-if="candidate" tone="neutral">仅参数注入</BaseTag>
      <span class="spacer" />
      <BaseSwitch
        :model-value="rule.enabled"
        :saving="saving"
        :aria-label="`规则 #${rule.id} 启用`"
        @update:model-value="$emit('toggle')"
      />
    </header>

    <p class="text" :class="{ 'is-candidate': candidate }">{{ text }}</p>

    <div v-if="keywords.length" class="chips">
      <BaseTag v-for="keyword in keywords" :key="keyword" tone="neutral">
        {{ keyword }}
      </BaseTag>
    </div>

    <footer class="meta">
      <span>置信度 {{ decimal(rule.confidence) }}</span>
      <span>稳定性 {{ decimal(rule.stability) }}</span>
      <span>情感强度 {{ decimal(rule.params.emotional_intensity) }}</span>
      <span>证据 {{ evidenceCount }} 条</span>
      <span class="spacer" />
      <BaseButton variant="secondary" size="sm" @click="$emit('open-detail')">详情</BaseButton>
    </footer>
  </article>
</template>

<style scoped>
.rule-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  padding: var(--space-14) var(--space-16);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.rule-card.is-off {
  background: var(--bg-subtle);
}

.head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.spacer {
  flex: 1;
}

.text {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-body);
  line-height: 1.7;
}

.text.is-candidate {
  color: var(--text-3);
}

.chips {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-4);
}

.meta {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  margin-top: auto;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

@media (max-width: 720px) {
  .meta {
    gap: var(--space-6);
  }
}
</style>
