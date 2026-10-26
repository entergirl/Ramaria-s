<script setup lang="ts">
/**
 * src/views/rules/RuleDetailToolbar.vue — 规则详情工具栏
 *
 * 职责:
 * - 承载返回列表、当前规则标识、保存动作与启停开关
 *
 * 设计特点:
 * - 返回键与卡片圆角切点对齐，行内高度固定不随内容跳变
 * - 保存键只在草稿有改动且校验通过时可用，未保存时以独立标记提示
 * - 保存与开关各自独立加载：保存中锁保存键，启停中锁开关，互不牵连
 * - 来源、状态与候选标记同排展示，状态不只靠颜色表达
 */
import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseSwitch from '../../components/base/BaseSwitch.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { BehaviorRuleView } from '../../core/api/dto'
import {
  isCandidateRule,
  ruleSourceLabel,
  ruleSourceTone,
  ruleStatusLabel,
} from '../../core/rules/format'

const props = defineProps<{
  rule: BehaviorRuleView
  canSave: boolean
  dirty: boolean
  draftSaving: boolean
  toggleSaving: boolean
}>()

defineEmits<{
  back: []
  save: []
  toggle: []
}>()

const sourceLabel = computed(() => ruleSourceLabel(props.rule.source))
const sourceTone = computed(() => ruleSourceTone(props.rule.source))
const statusLabel = computed(() => ruleStatusLabel(props.rule.enabled))
const candidate = computed(() => isCandidateRule(props.rule))
</script>

<template>
  <section class="toolbar">
    <BaseButton variant="secondary" size="sm" class="back" @click="$emit('back')">
      <svg class="icon" viewBox="0 0 24 24" aria-hidden="true">
        <path d="m15 18-6-6 6-6" />
      </svg>
      规则
    </BaseButton>

    <span class="divider" aria-hidden="true" />

    <span class="title">规则 #{{ rule.id }}</span>
    <BaseTag :tone="sourceTone">{{ sourceLabel }}</BaseTag>
    <BaseTag tone="neutral">{{ statusLabel }}</BaseTag>
    <BaseTag v-if="candidate" tone="neutral">仅参数注入</BaseTag>

    <span class="spacer" />

    <span v-if="dirty" class="dirty" role="status">未保存</span>
    <BaseButton
      variant="primary"
      size="sm"
      :disabled="!canSave"
      :loading="draftSaving"
      @click="$emit('save')"
    >
      保存
    </BaseButton>
    <BaseSwitch
      :model-value="rule.enabled"
      :saving="toggleSaving"
      label="启用"
      @update:model-value="$emit('toggle')"
    />
  </section>
</template>

<style scoped>
.toolbar {
  position: sticky;
  top: 0;
  z-index: var(--z-sticky);
  display: flex;
  flex: none;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-10);
  min-height: 60px;
  /* 左内距取卡片圆角半径，返回键与圆角切点对齐 */
  padding: var(--space-10) var(--space-16) var(--space-10) var(--space-20);
  background: var(--bg-card);
  border: 1px solid var(--border);
  border-radius: var(--radius-card);
  box-shadow: var(--shadow-sm);
}

.back {
  gap: var(--space-4);
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

.divider {
  width: 1px;
  height: 22px;
  background: var(--border-strong);
}

.title {
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
  line-height: 1.25;
}

.spacer {
  flex: 1;
}

.dirty {
  padding: 2px var(--space-8);
  border: 1px solid var(--warn-line);
  border-radius: var(--radius-tag);
  background: var(--warn-surface);
  color: var(--warn);
  font-size: var(--fs-caption);
  font-weight: var(--fw-semibold);
}
</style>
