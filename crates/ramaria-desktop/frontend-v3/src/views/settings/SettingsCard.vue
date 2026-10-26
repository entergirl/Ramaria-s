<script setup lang="ts">
/**
 * src/views/settings/SettingsCard.vue — 设置卡片壳
 *
 * 职责:
 * - 统一设置卡的头部（标题 / 说明 / 标记）、正文与页脚（状态 / 动作）
 *
 * 设计特点:
 * - 页脚状态按「未保存 / 保存中 / 已保存」单点表达，动作只有恢复默认与保存
 * - 错误留在卡内（错误条 + 重试由调用方放在正文），不阻断其它卡
 * - 保存中锁定正文与动作，避免脏值竞态
 */

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCard from '../../components/base/BaseCard.vue'

const props = withDefaults(
  defineProps<{
    title: string
    description?: string
    dirty?: boolean
    saving?: boolean
    error?: string
    /** 显示页脚（含保存动作） */
    editable?: boolean
    /** 显示「恢复默认」动作 */
    showReset?: boolean
    /** 默认值不可用：禁用「恢复默认」 */
    resetDisabled?: boolean
    /** 禁用原因（悬停提示） */
    resetHint?: string
  }>(),
  {
    description: '',
    dirty: false,
    saving: false,
    error: '',
    editable: true,
    showReset: true,
    resetDisabled: false,
    resetHint: '',
  },
)

const emit = defineEmits<{
  save: []
  reset: []
}>()

function statusText(): string {
  if (props.saving) {
    return '保存中…'
  }
  if (props.error) {
    return '未保存'
  }
  return props.dirty ? '有未保存的修改' : '已保存'
}
</script>

<template>
  <BaseCard class="settings-card" :state="error ? 'error' : 'default'">
    <template #error>{{ error }}</template>

    <header class="s-head">
      <div class="s-head-text">
        <h3 class="s-title">{{ title }}</h3>
        <p v-if="description" class="s-desc">{{ description }}</p>
      </div>
      <div class="s-head-extra">
        <slot name="head" />
      </div>
    </header>

    <div class="s-body" :aria-busy="saving ? 'true' : undefined">
      <slot />
    </div>

    <footer v-if="editable || $slots.footExtra" class="s-foot">
      <span v-if="editable" class="s-state" :data-tone="error ? 'danger' : dirty ? 'warn' : 'quiet'">
        {{ statusText() }}
      </span>
      <span class="s-foot-spacer" />
      <slot name="footExtra" />
      <BaseButton
        v-if="editable && showReset"
        variant="ghost"
        size="sm"
        :disabled="saving || resetDisabled"
        :title="resetDisabled ? resetHint || '默认值不可用' : undefined"
        @click="emit('reset')"
      >
        恢复默认
      </BaseButton>
      <BaseButton v-if="editable" size="sm" :loading="saving" :disabled="!dirty" @click="emit('save')">
        保存
      </BaseButton>
    </footer>
  </BaseCard>
</template>

<style scoped>
.settings-card {
  min-width: 0;
  height: 100%;
}

.s-head {
  display: flex;
  align-items: flex-start;
  gap: var(--space-10);
}

.s-head-text {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-4);
  min-width: 0;
}

.s-title {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: 1.4;
}

.s-desc {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-small);
  line-height: 1.6;
}

.s-head-extra {
  display: flex;
  flex: none;
  align-items: center;
  gap: var(--space-6);
}

.s-body {
  display: flex;
  flex: 1;
  flex-direction: column;
  gap: var(--space-10);
  min-width: 0;
}

/* 等高行里短卡的余白落在正文底部，操作区始终贴齐行底 */
.s-foot {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  margin-top: auto;
  padding-top: var(--space-10);
  border-top: 1px solid var(--border);
}

.s-foot-spacer {
  flex: 1;
}

.s-state {
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.s-state[data-tone='warn'] {
  color: var(--warn);
}

.s-state[data-tone='danger'] {
  color: var(--danger);
}
</style>
