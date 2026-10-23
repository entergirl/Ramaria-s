<script setup lang="ts">
/**
 * src/views/setup/StepConfirm.vue — 步骤 3：确认并进入
 *
 * 职责:
 * - 回显本次将保存的配置、降级说明与索引后台构建说明
 * - 线上后端在此发起隐私确认（数据范围与去向由外层模态承载）
 *
 * 设计特点:
 * - 摘要行由纯函数生成；地址与路径用等宽芯片，行尾「修改」直接回到对应步骤
 * - 隐私确认块只占一行：说明在左，状态与「去确认」在右，不挤压摘要
 * - 提交错误留在卡内，不替换整页内容
 */

import BaseButton from '../../components/base/BaseButton.vue'
import type { SetupMode, SummaryRow } from '../../core/setupWizard'

const props = defineProps<{
  rows: SummaryRow[]
  mode: SetupMode
  privacyAccepted: boolean
  privacyLoading: boolean
  submitError: string
  locked: boolean
}>()

const emit = defineEmits<{
  goto: [step: number]
  privacy: []
}>()
</script>

<template>
  <section class="panel">
    <div class="panel-head">
      <h2 class="panel-title">确认配置</h2>
      <p class="panel-sub">检查以下信息；需要修改时点行尾「修改」。确认无误后保存并进入。</p>
    </div>

    <div class="summary">
      <div v-for="row in rows" :key="row.label" class="summary-row">
        <span class="summary-label">{{ row.label }}</span>
        <code v-if="row.mono" class="summary-code" :class="{ 'is-warning': row.warning }">
          {{ row.value }}
        </code>
        <span v-else class="summary-value" :class="{ 'is-warning': row.warning }">
          {{ row.value }}
        </span>
        <span class="spacer" />
        <button
          v-if="row.target"
          type="button"
          class="mini"
          :disabled="props.locked"
          @click="emit('goto', row.target)"
        >
          修改
        </button>
      </div>
    </div>

    <div v-if="props.mode === 'online'" class="privacy">
      <div class="privacy-copy">
        <span class="privacy-title">线上后端隐私确认</span>
      </div>
      <span class="spacer" />
      <span class="privacy-state" :class="{ 'is-ok': privacyAccepted }">
        {{ privacyLoading ? '读取中…' : privacyAccepted ? '已确认' : '未确认' }}
      </span>
      <BaseButton
        v-if="!privacyAccepted"
        size="sm"
        variant="ghost"
        :disabled="props.locked || privacyLoading"
        @click="emit('privacy')"
      >
        去确认
      </BaseButton>
    </div>

    <p v-if="submitError" class="submit-error" role="alert">{{ submitError }}</p>
  </section>
</template>

<style scoped>
.panel {
  display: flex;
  flex-direction: column;
  gap: var(--space-16);
}

.panel-head {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.panel-title {
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h3);
}

.panel-sub {
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.summary {
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-session);
  background: var(--bg-subtle);
}

.summary-row {
  display: flex;
  align-items: center;
  gap: var(--space-12);
  padding: var(--space-8) var(--space-14);
}

.summary-row + .summary-row {
  border-top: 1px dashed var(--border);
}

.summary-label {
  flex: none;
  width: 84px;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.summary-value {
  min-width: 0;
  overflow-wrap: anywhere;
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-medium);
}

.summary-value.is-warning {
  color: var(--warn);
}

.summary-code {
  min-width: 0;
  max-width: 100%;
  padding: 0 var(--space-8);
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-tag);
  background: var(--bg-card);
  color: var(--text-2);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  line-height: var(--lh-code);
  text-overflow: ellipsis;
  white-space: nowrap;
}

.summary-code.is-warning {
  border-color: var(--warn-line);
  color: var(--warn);
}

.spacer {
  flex: 1;
}

.mini {
  flex: none;
  padding: 0 var(--space-4);
  border: none;
  border-radius: var(--radius-check);
  background: transparent;
  color: var(--brand-text);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-small);
  cursor: pointer;
}

.mini:hover:not(:disabled) {
  color: var(--brand);
}

.mini:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.mini:disabled {
  opacity: var(--opacity-disabled);
  cursor: default;
}

.privacy {
  display: flex;
  align-items: center;
  gap: var(--space-12);
  padding: var(--space-8) var(--space-14);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.privacy-copy {
  display: flex;
  flex-direction: column;
  gap: 2px;
  min-width: 0;
}

.privacy-title {
  color: var(--text-1);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.privacy-state {
  flex: none;
  color: var(--warn);
  font-size: var(--fs-small);
  font-weight: var(--fw-semibold);
}

.privacy-state.is-ok {
  color: var(--ok);
}

.submit-error {
  margin: 0;
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-tag);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}
</style>
