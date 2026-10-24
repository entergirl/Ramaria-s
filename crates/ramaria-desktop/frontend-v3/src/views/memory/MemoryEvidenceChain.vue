<script setup lang="ts">
/**
 * src/views/memory/MemoryEvidenceChain.vue — 性格标签的证据链
 *
 * 职责:
 * - 展示某条性格标签的支撑 / 矛盾 / 中性统计与逐事件溯源
 * - 事件展开后给出推断信号与来源 L1 摘要、证据片段
 *
 * 设计特点:
 * - 默认只列前五条，长链按需展开，避免一次撑高卡片
 * - 事件方向不在单条上判定（后端只给汇总计数），色块仅用于统计行
 * - 空链与读失败都给明确文案与重试入口，不留白
 */
import { computed, ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { TraitEvidenceChain } from '../../core/api/dto'
import { decimal, percent, signedDecimal } from '../../core/memory/format'

const props = defineProps<{
  loading: boolean
  error: string
  chain: TraitEvidenceChain | null
}>()

defineEmits<{
  retry: []
}>()

/** 首屏最多展示的证据事件数 */
const PAGE_SIZE = 5

const showAll = ref(false)
const events = computed(() => props.chain?.evidence_events ?? [])
const visible = computed(() => (showAll.value ? events.value : events.value.slice(0, PAGE_SIZE)))
const hidden = computed(() => events.value.length - visible.value.length)

watch(
  () => props.chain?.trait_id,
  () => {
    showAll.value = false
  },
)
</script>

<template>
  <div class="ev">
    <p v-if="loading" class="ev-hint">读取证据链…</p>

    <div v-else-if="error" class="ev-error">
      <span>{{ error }}</span>
      <BaseButton variant="ghost" size="sm" @click="$emit('retry')">重试</BaseButton>
    </div>

    <p v-else-if="!chain || events.length === 0" class="ev-hint">
      暂无证据记录；积累更多对话后，事件提取会自动关联证据。
    </p>

    <template v-else>
      <div class="ev-stats">
        <BaseTag tone="ok">支持 {{ chain.support_count }}</BaseTag>
        <BaseTag tone="brand">矛盾 {{ chain.contradict_count }}</BaseTag>
        <BaseTag tone="neutral">中性 {{ chain.neutral_count }}</BaseTag>
        <span class="ev-total">共 {{ chain.total_evidence }} 条</span>
      </div>

      <div class="ev-chain">
        <div v-for="event in visible" :key="event.event_id" class="ev-item">
          <div class="ev-head">
            <span class="ev-title">{{ event.title || '未命名事件' }}</span>
            <BaseTag tone="neutral">置信 {{ percent(event.confidence) }}</BaseTag>
            <BaseTag v-if="event.attitude" tone="accent">{{ event.attitude }}</BaseTag>
          </div>
          <p class="ev-summary">{{ event.summary }}</p>
          <div class="ev-meta">
            <span>效价 {{ signedDecimal(event.valence) }}</span>
            <span>显著性 {{ decimal(event.salience) }}</span>
            <span v-if="event.motives">动机 · {{ event.motives }}</span>
          </div>
          <p v-if="event.paraphrase" class="ev-note">重述 · {{ event.paraphrase }}</p>

          <div class="ev-sources">
            <div v-for="source in event.l1_sources" :key="source.l1_id" class="ev-source">
              <div class="ev-source-head">
                <span class="ev-source-tag">L1</span>
                <span class="ev-source-text">{{ source.summary }}</span>
                <span class="ev-source-weight">权重 {{ decimal(source.weight) }}</span>
              </div>
              <div v-if="source.evidence_notes.length" class="ev-notes">
                <span v-for="(note, index) in source.evidence_notes" :key="index" class="ev-note-item">
                  {{ note }}
                </span>
              </div>
              <div class="ev-source-meta">
                <span v-if="source.atmosphere">氛围 · {{ source.atmosphere }}</span>
                <span>效价 {{ signedDecimal(source.valence) }}</span>
              </div>
            </div>
            <p v-if="event.l1_sources.length === 0" class="ev-hint">该事件没有可用的 L1 溯源。</p>
          </div>
        </div>
      </div>

      <div v-if="hidden > 0" class="ev-more">
        <BaseButton variant="ghost" size="sm" @click="showAll = true">
          查看其余 {{ hidden }} 条
        </BaseButton>
      </div>
    </template>
  </div>
</template>

<style scoped>
.ev {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
}

.ev-hint {
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: 1.65;
}

.ev-error {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  color: var(--danger);
  font-size: var(--fs-small);
}

.ev-stats {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.ev-total {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.ev-chain {
  display: flex;
  flex-direction: column;
}

.ev-item {
  position: relative;
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  margin-left: 3px;
  padding: var(--space-8) 0 var(--space-8) var(--space-16);
  border-left: 1px dashed var(--border-strong);
}

.ev-item::before {
  position: absolute;
  top: 13px;
  left: -4px;
  width: 7px;
  height: 7px;
  border: 1.5px solid var(--text-3);
  border-radius: var(--radius-full);
  background: var(--bg-card);
  content: '';
}

.ev-item:last-child {
  border-left-color: transparent;
}

.ev-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-6);
}

.ev-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.ev-summary {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.ev-meta {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.ev-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.5;
}

.ev-sources {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
  margin-top: var(--space-4);
  padding: var(--space-8) var(--space-10);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.ev-source {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.ev-source-head {
  display: flex;
  align-items: baseline;
  gap: var(--space-6);
}

.ev-source-tag {
  flex: none;
  color: var(--brand-text);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}

.ev-source-text {
  flex: 1;
  min-width: 0;
  color: var(--text-2);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.ev-source-weight {
  flex: none;
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.ev-notes {
  display: flex;
  flex-direction: column;
  gap: 2px;
  padding-left: var(--space-20);
}

.ev-note-item {
  position: relative;
  color: var(--text-2);
  font-size: var(--fs-caption);
  line-height: 1.55;
}

.ev-note-item::before {
  position: absolute;
  left: calc(var(--space-10) * -1);
  content: '·';
  color: var(--text-3);
}

.ev-source-meta {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-8);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.ev-more {
  display: flex;
  justify-content: flex-start;
}
</style>
