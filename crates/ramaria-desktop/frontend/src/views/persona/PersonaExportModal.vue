<script setup lang="ts">
/**
 * src/views/persona/PersonaExportModal.vue — 导出人格预设
 *
 * 职责:
 * - 确认导出对象与「含对话示例」开关，回传选择结果
 *
 * 设计特点:
 * - 示例默认关闭，开启时给出含真实对话片段的警示
 * - 文件路径由调用方经保存对话框取得，本组件不接触路径
 * - 导出执行中锁定关闭与重复提交
 */
import { ref, watch } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseCheckbox from '../../components/base/BaseCheckbox.vue'
import BaseModal from '../../components/base/BaseModal.vue'
import BaseTag from '../../components/base/BaseTag.vue'
import type { PersonaFull } from '../../core/api/dto'
import { personaInitial } from '../../core/memory/format'
import { personaKindLabel, personaKindTone } from '../../core/persona/format'

const props = defineProps<{
  open: boolean
  persona: PersonaFull | null
  busy: boolean
  error: string
}>()

const emit = defineEmits<{
  close: []
  confirm: [withExamples: boolean]
}>()

const withExamples = ref(false)

watch(
  () => props.open,
  (open) => {
    if (open) {
      withExamples.value = false
    }
  },
)

function requestClose(): void {
  if (props.busy) {
    return
  }
  emit('close')
}
</script>

<template>
  <BaseModal :open="open" title="导出人格" :loading="busy" @close="requestClose">
    <div v-if="persona" class="export-body">
      <div class="who">
        <span class="avatar" aria-hidden="true">{{ personaInitial(persona.name) }}</span>
        <div class="who-text">
          <div class="who-name">{{ persona.name }}</div>
          <div class="who-sub">{{ persona.uid }}</div>
        </div>
        <BaseTag :tone="personaKindTone(persona.kind)">
          {{ personaKindLabel(persona.kind) }}
        </BaseTag>
      </div>

      <div class="choice">
        <BaseCheckbox v-model="withExamples" label="包含对话示例" />
        <p class="hint">示例来自真实对话片段；默认关闭，仅用于预览与迁移。</p>
      </div>

      <p class="note">
        导出为预设文件（超集 TOML，可原样再导入）：包含名称、描述、L3 画像、知识事实与行为规则，
        不含证据链与会话原文。
      </p>

      <p v-if="error" class="error" role="alert">{{ error }}</p>
    </div>

    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" :disabled="busy" @click="requestClose">取消</BaseButton>
      <BaseButton :disabled="!persona" :loading="busy" @click="emit('confirm', withExamples)">
        选择位置并导出
      </BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
.export-body {
  display: flex;
  flex-direction: column;
  gap: var(--space-12);
}

.who {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--space-12);
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.avatar {
  display: grid;
  flex: none;
  place-items: center;
  width: 38px;
  height: 38px;
  border-radius: var(--radius-full);
  background: var(--brand-surface);
  box-shadow: inset 0 0 0 1px var(--brand-line);
  color: var(--brand-text);
  font-size: var(--fs-body-lg);
  font-weight: var(--fw-bold);
  line-height: 1;
}

.who-text {
  flex: 1;
  min-width: 0;
}

.who-name {
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-bold);
}

.who-sub {
  margin-top: 2px;
  color: var(--text-3);
  font-family: var(--font-mono);
  font-size: var(--fs-caption);
}

.choice {
  display: flex;
  flex-direction: column;
  gap: var(--space-4);
}

.hint {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}

.note {
  margin: 0;
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: 1.7;
}

.error {
  margin: 0;
  padding: var(--space-8) var(--space-12);
  border: 1px solid var(--danger-line);
  border-radius: var(--radius-tag);
  background: var(--danger-surface);
  color: var(--danger);
  font-size: var(--fs-small);
}

.spacer {
  flex: 1;
}
</style>
