<script setup lang="ts">
/**
 * src/views/memory/MemoryKnowledgeCard.vue — 知识事实卡（只读）
 *
 * 职责:
 * - 展示事实内容、来源、分层、置信度与关键词
 * - 卡内展开该事实的更新历史（覆盖链）
 *
 * 设计特点:
 * - 只读：不提供编辑、删除与校验动作，事实由管线写入
 * - 更新历史以日期为主键，按最早到最新排列并标出当前版本
 * - 无覆盖链时不渲染展开入口，避免空详情
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaFact } from '../../core/api/dto'
import {
  dayLabel,
  factSourceLabel,
  factTierLabel,
  limitedKeywords,
  percent,
} from '../../core/memory/format'

const props = defineProps<{
  fact: PersonaFact
  versions: PersonaFact[]
  open: boolean
}>()

defineEmits<{
  toggle: []
}>()

const keywords = computed(() => limitedKeywords(props.fact.keyword_hint))
const hasHistory = computed(() => props.versions.length > 1)
</script>

<template>
  <article class="mem-card fact-card" :class="{ 'is-open': open }">
    <p class="mem-summary">{{ fact.content }}</p>

    <div class="meta-row">
      <BaseTag tone="neutral">来源 · {{ factSourceLabel(fact.source) }}</BaseTag>
      <BaseTag tone="neutral">分层 · {{ factTierLabel(fact.tier) }}</BaseTag>
      <BaseTag tone="accent">置信度 {{ percent(fact.confidence) }}</BaseTag>
    </div>

    <div v-if="keywords.length" class="chip-row">
      <span v-for="keyword in keywords" :key="keyword" class="chip-sm">{{ keyword }}</span>
    </div>

    <div class="meta-row">
      <span class="kv">更新日期 <b>{{ dayLabel(fact.created_at) }}</b></span>
      <span class="spacer" />
      <BaseButton
        v-if="hasHistory"
        variant="ghost"
        size="sm"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-controls="`k-detail-${fact.id}`"
        @click="$emit('toggle')"
      >
        {{ open ? '收起' : '更新历史' }}
        <svg class="chev" :class="{ 'is-up': open }" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </BaseButton>
    </div>

    <div v-if="open && hasHistory" :id="`k-detail-${fact.id}`" class="mem-detail">
      <section class="detail-block">
        <div class="detail-title">更新历史</div>
        <div class="ver-chain">
          <div
            v-for="version in versions"
            :key="version.id"
            class="ver-item"
            :class="{ 'is-current': version.id === fact.id }"
          >
            <b>{{ dayLabel(version.created_at) }}</b>
            <span class="ver-text">{{ version.content }}</span>
            <span v-if="version.id === fact.id" class="ver-current">当前</span>
          </div>
        </div>
      </section>
      <div class="detail-actions">
        <span class="spacer" />
        <span class="detail-note">历史版本随覆盖链保留，按最早到最新排列。</span>
      </div>
    </div>
  </article>
</template>

<style scoped>
.mem-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  padding: var(--pad-card) var(--pad-card-lg);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
  transition:
    border-color var(--dur-fast) var(--ease-out),
    box-shadow var(--dur-fast) var(--ease-out);
}

.mem-card.is-open {
  border-color: var(--brand-line);
  box-shadow: var(--shadow-md);
}

.mem-summary {
  font-size: var(--fs-body);
  line-height: 1.7;
}

.meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.meta-row .kv b {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.chip-row {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
}

.chip-sm {
  display: inline-flex;
  align-items: center;
  height: 20px;
  padding: 0 var(--space-8);
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-2);
  font-size: var(--fs-caption);
  line-height: 1;
}

.chev {
  width: 11px;
  height: 11px;
  fill: none;
  stroke: currentColor;
  stroke-width: 2.2;
  stroke-linecap: round;
  stroke-linejoin: round;
  transition: transform var(--dur-fast) var(--ease-out);
}

.chev.is-up {
  transform: rotate(180deg);
}

.mem-detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding-top: var(--space-12);
  border-top: 1px dashed var(--border);
}

.detail-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-6);
  min-width: 0;
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.detail-title {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  letter-spacing: 0.06em;
}

.ver-chain {
  display: flex;
  flex-direction: column;
}

.ver-item {
  position: relative;
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-6);
  margin-left: 3px;
  padding: var(--space-6) 0 var(--space-6) var(--space-16);
  border-left: 1px dashed var(--border-strong);
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.ver-item::before {
  position: absolute;
  top: 12px;
  left: -4px;
  width: 7px;
  height: 7px;
  border: 1.5px solid var(--border-strong);
  border-radius: var(--radius-full);
  background: var(--bg-card);
  content: '';
}

.ver-item:last-child {
  border-left-color: transparent;
}

.ver-item.is-current {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.ver-item.is-current::before {
  border-color: var(--brand);
  background: var(--brand);
}

.ver-item b {
  flex: none;
  font-weight: var(--fw-semibold);
}

.ver-text {
  flex: 1;
  min-width: 0;
}

.ver-current {
  flex: none;
  color: var(--brand-text);
  font-size: var(--fs-caption);
}

.detail-actions {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.detail-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.spacer {
  flex: 1;
}
</style>
