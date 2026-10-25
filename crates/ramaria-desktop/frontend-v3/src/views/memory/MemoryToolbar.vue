<script setup lang="ts">
/**
 * src/views/memory/MemoryToolbar.vue — 记忆页工具栏
 *
 * 职责:
 * - 展示当前人格头像、名称与分类，作为人格卡的展开入口
 * - 承载「深度处理导入的消息」入口（仅导入人格可用）
 *
 * 设计特点:
 * - 人格筛选块与规则页共用同一形态，展开态经 aria-expanded 透出
 * - 工具栏始终全宽，不随人格卡收缩
 */
import BaseButton from '../../components/base/BaseButton.vue'
import PersonaFilterButton from '../../components/shell/PersonaFilterButton.vue'
import type { PersonaSummary } from '../../core/api/dto'

defineProps<{
  persona: PersonaSummary | null
  open: boolean
  canDeepProcess: boolean
  processing: boolean
}>()

defineEmits<{
  toggle: []
  'deep-process': []
}>()
</script>

<template>
  <section class="toolbar">
    <PersonaFilterButton
      id="memory-persona-trigger"
      controls="memory-personas"
      :persona="persona"
      :open="open"
      @toggle="$emit('toggle')"
    />

    <span class="spacer" />

    <BaseButton
      v-if="canDeepProcess"
      variant="ghost"
      size="sm"
      :loading="processing"
      @click="$emit('deep-process')"
    >
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="M12 3v3M12 18v3M4.2 7.5l2.6 1.5M17.2 15l2.6 1.5M4.2 16.5l2.6-1.5M17.2 9l2.6-1.5" />
        <circle cx="12" cy="12" r="3.4" />
      </svg>
      深度处理导入的消息
    </BaseButton>
  </section>
</template>

<style scoped>
.toolbar {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-14);
  padding: var(--space-14) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.spacer {
  flex: 1;
}

.icon {
  width: 15px;
  height: 15px;
  fill: none;
  stroke: currentColor;
  stroke-width: 1.8;
  stroke-linecap: round;
  stroke-linejoin: round;
}

@media (max-width: 720px) {
  .toolbar {
    flex-wrap: wrap;
  }
}
</style>
