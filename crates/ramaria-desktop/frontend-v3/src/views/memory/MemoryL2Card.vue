<script setup lang="ts">
/**
 * src/views/memory/MemoryL2Card.vue — L2 离散事件卡
 *
 * 职责:
 * - 展示置信度、共享度、摘要、效价、表现与关键词
 * - 卡内展开事件时间、态度、显著性等可得字段
 *
 * 设计特点:
 * - 事件级证据链不在本卡伪造：可溯源信息按性格维度在 L3 页签展开
 * - 时间以事件开始时间为准，悬停补入库时间
 * - 收起态只保留判断事件所需的头部与两行正文
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { MemoryEvent } from '../../core/api/dto'
import { formatDateTime, smartTime } from '../../core/chat/time'
import {
  confidenceTone,
  dayLabel,
  decimal,
  limitedKeywords,
  percent,
  presentationLabel,
  shareLabel,
  signedDecimal,
  valenceTone,
} from '../../core/memory/format'

const props = defineProps<{
  item: MemoryEvent
  open: boolean
  personaName: string
}>()

defineEmits<{
  toggle: []
}>()

const startedAt = computed(() => props.item.start || props.item.created_at)
const keywords = computed(() => limitedKeywords(props.item.keywords))
const tone = computed(() => valenceTone(props.item.valence))
const startText = computed(() => formatDateTime(startedAt.value))
const endText = computed(() => {
  const end = props.item.end
  return end && end > props.item.start ? formatDateTime(end) : '—'
})
</script>

<template>
  <article class="mem-card" :class="{ 'is-open': open }">
    <div class="l2-head">
      <BaseTag :tone="confidenceTone(item.confidence)">置信度 {{ percent(item.confidence) }}</BaseTag>
      <BaseTag tone="neutral">共享度 · {{ shareLabel(item.share) }}</BaseTag>
      <span class="spacer" />
      <span class="head-date">{{ dayLabel(startedAt) }}</span>
    </div>

    <p class="mem-summary">{{ item.title || item.summary }}</p>
    <p v-if="item.title && item.summary && item.summary !== item.title" class="mem-sub">
      {{ item.summary }}
    </p>

    <div v-if="keywords.length" class="chip-row">
      <span v-for="keyword in keywords" :key="keyword" class="chip-sm">{{ keyword }}</span>
    </div>

    <div class="meta-row">
      <span class="kv">效价 <b :class="`valence-${tone}`">{{ signedDecimal(item.valence) }}</b></span>
      <span class="kv">表现 <b>{{ presentationLabel(item.presentation) }}</b></span>
      <span class="spacer" />
      <span class="time-note" :title="`入库：${formatDateTime(item.created_at)}`">
        {{ smartTime(startedAt) }}
      </span>
      <BaseButton
        variant="ghost"
        size="sm"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-controls="`l2-detail-${item.id}`"
        @click="$emit('toggle')"
      >
        {{ open ? '收起' : '详情' }}
        <svg class="chev" :class="{ 'is-up': open }" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </BaseButton>
    </div>

    <div v-if="open" :id="`l2-detail-${item.id}`" class="mem-detail">
      <div class="detail-cols">
        <section class="detail-block">
          <div class="detail-title">事件</div>
          <div class="detail-kv"><span class="k">标题</span><b>{{ item.title || '—' }}</b></div>
          <div class="detail-kv"><span class="k">摘要</span><span>{{ item.summary || '—' }}</span></div>
          <div class="detail-kv"><span class="k">态度</span><span>{{ item.attitude || '—' }}</span></div>
        </section>

        <section class="detail-block">
          <div class="detail-title">采集</div>
          <div class="detail-kv"><span class="k">开始</span><b>{{ startText }}</b></div>
          <div class="detail-kv"><span class="k">结束</span><b>{{ endText }}</b></div>
          <div class="detail-kv">
            <span class="k">置信度</span><b>{{ percent(item.confidence) }}</b>
          </div>
          <div class="detail-kv">
            <span class="k">共享度</span><b>{{ shareLabel(item.share) }} · {{ percent(item.share) }}</b>
          </div>
          <div class="detail-kv">
            <span class="k">显著性</span><b>{{ decimal(item.salience) }}</b>
          </div>
          <div class="detail-kv"><span class="k">归属人格</span><b>{{ personaName }}</b></div>
          <div class="detail-kv">
            <span class="k">入库</span><span>{{ formatDateTime(item.created_at) }}</span>
          </div>
        </section>
      </div>

      <div class="detail-actions">
        <span class="spacer" />
        <span class="detail-note">证据链在 L3 页签按性格维度展开。</span>
      </div>
    </div>
  </article>
</template>

<style scoped>
.mem-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding: var(--pad-card-lg);
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

.l2-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.head-date {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.mem-summary {
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
  line-height: 1.7;
}

.mem-sub {
  margin-top: calc(var(--space-4) * -1);
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.65;
}

.meta-row {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.meta-row .kv b {
  color: var(--text-1);
  font-weight: var(--fw-semibold);
}

.meta-row .kv b.valence-pos {
  color: var(--ok);
}

.meta-row .kv b.valence-neg {
  color: var(--danger);
}

.meta-row .kv b.valence-flat {
  color: var(--text-2);
}

.time-note {
  cursor: default;
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

.mem-detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
  padding-top: var(--space-12);
  border-top: 1px dashed var(--border);
}

.detail-cols {
  display: grid;
  grid-template-columns: 1.15fr 0.85fr;
  gap: var(--space-12);
  align-items: start;
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

.detail-kv {
  display: flex;
  gap: var(--space-8);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.detail-kv .k {
  flex: none;
  width: 64px;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.detail-kv b {
  min-width: 0;
  color: var(--text-1);
  font-weight: var(--fw-semibold);
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

@media (max-width: 760px) {
  .detail-cols {
    grid-template-columns: 1fr;
  }
}
</style>
