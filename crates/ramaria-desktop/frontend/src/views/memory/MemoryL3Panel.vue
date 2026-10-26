<script setup lang="ts">
/**
 * src/views/memory/MemoryL3Panel.vue — L3 性格画像页签
 *
 * 职责:
 * - 顶部展示画像状态与样本量，并提供重载画像入口
 * - 按底色层 / 主色调层 / 点缀层分组渲染性格标签卡
 *
 * 设计特点:
 * - 三层固定顺序出现，空层给一行说明而不隐藏，避免层概念丢失
 * - 重载画像按人格下发一次，不在每条标签卡内重复
 * - 详情展开状态与证据链由外层持有，本组件只做分组与透传
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonalityProfile, ProfileStatus, TraitDetail } from '../../core/api/dto'
import { amount, profileStatusLabel, profileStatusTone } from '../../core/memory/format'
import MemoryTraitCard from './MemoryTraitCard.vue'
import type { EvidenceState } from './useMemoryView'

const props = defineProps<{
  profile: PersonalityProfile | null
  status: ProfileStatus | null
  detailId: string | null
  evidence: Record<number, EvidenceState>
  reloading: boolean
}>()

defineEmits<{
  'toggle-trait': [traitId: number]
  'retry-evidence': [traitId: number]
  reload: []
}>()

/** 三层配置：顺序、标题与一句说明 */
const LAYERS: { key: 'base' | 'primary' | 'accent'; label: string; hint: string }[] = [
  { key: 'base', label: '底色层', hint: '跨情境稳定的深层基调' },
  { key: 'primary', label: '主色调层', hint: '日常最突出的性格特征' },
  { key: 'accent', label: '点缀层', hint: '特定条件下浮现的一面' },
]

const layers = computed(() =>
  LAYERS.map((layer) => ({
    ...layer,
    traits: (props.profile?.[layer.key] ?? []) as TraitDetail[],
  })),
)

const statusLabel = computed(() => profileStatusLabel(props.status?.status))
const statusTone = computed(() => profileStatusTone(props.status?.status))
const statusText = computed(() => props.status?.status_text ?? '画像状态暂未读取')
const sampleText = computed(() => {
  if (!props.status) {
    return ''
  }
  return `样本量 ${amount(props.status.n_total_eff)} · 活跃特质 ${props.status.active_trait_count}`
})
</script>

<template>
  <div class="l3">
    <section class="profile-status">
      <BaseTag :tone="statusTone">画像状态 · {{ statusLabel }}</BaseTag>
      <div class="status-text">
        <span>{{ statusText }}</span>
        <span v-if="sampleText" class="status-sample">{{ sampleText }}</span>
      </div>
      <span class="spacer" />
      <BaseButton variant="ghost" size="sm" :loading="reloading" @click="$emit('reload')">
        重载画像
      </BaseButton>
    </section>

    <section v-for="layer in layers" :key="layer.key" class="layer">
      <header class="layer-head">
        <span class="layer-title">{{ layer.label }}</span>
        <span class="layer-hint">{{ layer.hint }}</span>
        <span class="spacer" />
        <span class="layer-count">{{ layer.traits.length }}</span>
      </header>

      <MemoryTraitCard
        v-for="trait in layer.traits"
        :key="trait.id"
        :trait="trait"
        :open="detailId === `l3:${trait.id}`"
        :evidence="evidence[trait.id]"
        @toggle="$emit('toggle-trait', trait.id)"
        @retry-evidence="$emit('retry-evidence', trait.id)"
      />

      <p v-if="layer.traits.length === 0" class="layer-empty">该层暂无性格标签。</p>
    </section>
  </div>
</template>

<style scoped>
.l3 {
  display: flex;
  flex-direction: column;
  gap: var(--gap-section);
}

.profile-status {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-12);
  padding: var(--pad-card);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.status-text {
  display: flex;
  flex-wrap: wrap;
  gap: var(--space-8);
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.status-sample {
  color: var(--text-3);
}

.layer {
  display: flex;
  flex-direction: column;
  gap: var(--space-10);
}

.layer-head {
  display: flex;
  flex-wrap: wrap;
  align-items: baseline;
  gap: var(--space-8);
  padding: 0 var(--space-4);
}

.layer-title {
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
}

.layer-hint {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.layer-count {
  min-width: 18px;
  padding: 0 var(--space-6);
  border-radius: var(--radius-tag);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
  line-height: 18px;
  text-align: center;
}

.layer-empty {
  padding: var(--space-12) var(--space-16);
  border: 1px dashed var(--border);
  border-radius: var(--radius-inner);
  color: var(--text-3);
  font-size: var(--fs-small);
}

.spacer {
  flex: 1;
}
</style>
