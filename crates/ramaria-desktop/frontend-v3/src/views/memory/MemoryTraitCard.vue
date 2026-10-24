<script setup lang="ts">
/**
 * src/views/memory/MemoryTraitCard.vue — 单条性格标签卡
 *
 * 职责:
 * - 展示标签名、置信度、含义、证据量、一致性与触发 / 抑制条件
 * - 卡内展开该标签的证据链
 *
 * 设计特点:
 * - 置信度用标签色阶表达，不只靠颜色：数字与色彩同时出现
 * - 反向界定与关联性格有则显示、无则不留空行
 * - 详情只承载证据链与生成时间，重载画像放在页签顶部统一下发
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { TraitDetail } from '../../core/api/dto'
import { formatDateTime, smartTime } from '../../core/chat/time'
import { amount, confidenceTone, percent } from '../../core/memory/format'
import MemoryEvidenceChain from './MemoryEvidenceChain.vue'
import type { EvidenceState } from './useMemoryView'

const props = defineProps<{
  trait: TraitDetail
  open: boolean
  evidence: EvidenceState | undefined
}>()

defineEmits<{
  toggle: []
  'retry-evidence': []
}>()

const createdText = computed(() => formatDateTime(props.trait.created_at))
const evidenceState = computed<EvidenceState>(
  () => props.evidence ?? { loading: false, error: '', chain: null },
)
</script>

<template>
  <article class="mem-card trait-card" :class="{ 'is-open': open }">
    <div class="trait-head">
      <span class="trait-name">{{ trait.label }}</span>
      <BaseTag :tone="confidenceTone(trait.confidence)">置信度 {{ percent(trait.confidence) }}</BaseTag>
    </div>

    <p class="trait-desc">{{ trait.meaning }}</p>

    <div class="meta-row">
      <span class="kv">证据量 <b>{{ amount(trait.evidence) }}</b></span>
      <span class="kv">一致性 <b>{{ percent(trait.consistency) }}</b></span>
      <span v-if="trait.trigger" class="kv">触发 · {{ trait.trigger }}</span>
      <span v-if="trait.suppress" class="kv">抑制 · {{ trait.suppress }}</span>
    </div>

    <p v-if="trait.not_meaning" class="trait-note">≠ {{ trait.not_meaning }}</p>
    <p v-if="trait.related" class="trait-note">关联 · {{ trait.related }}</p>

    <div class="meta-row">
      <span class="time-note" :title="`生成：${createdText}`">{{ smartTime(trait.created_at) }}</span>
      <span class="spacer" />
      <BaseButton
        variant="ghost"
        size="sm"
        :aria-expanded="open ? 'true' : 'false'"
        :aria-controls="`l3-detail-${trait.id}`"
        @click="$emit('toggle')"
      >
        {{ open ? '收起' : '展开证据' }}
        <svg class="chev" :class="{ 'is-up': open }" viewBox="0 0 24 24" aria-hidden="true">
          <path d="m6 9 6 6 6-6" />
        </svg>
      </BaseButton>
    </div>

    <div v-if="open" :id="`l3-detail-${trait.id}`" class="mem-detail">
      <div class="detail-title">证据链</div>
      <MemoryEvidenceChain
        :loading="evidenceState.loading"
        :error="evidenceState.error"
        :chain="evidenceState.chain"
        @retry="$emit('retry-evidence')"
      />
      <div class="detail-actions">
        <span class="spacer" />
        <span class="detail-note">证据链按事件倒序列出；画像重载由页签顶部统一下发。</span>
      </div>
    </div>
  </article>
</template>

<style scoped>
.mem-card {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
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

.trait-head {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
}

.trait-name {
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
}

.trait-desc {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.72;
}

.trait-note {
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
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

.mem-detail {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
  padding-top: var(--space-12);
  border-top: 1px dashed var(--border);
}

.detail-title {
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
  letter-spacing: 0.06em;
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
