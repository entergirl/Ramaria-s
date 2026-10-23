<script setup lang="ts">
/**
 * src/views/setup/StepEmbedding.vue — 步骤 2：嵌入模型
 *
 * 职责:
 * - 收集嵌入模型文件夹路径，提供校验与跳过两条路径
 *
 * 设计特点:
 * - 校验只做路径与模型文件可用性判断，失败原因直接来自后端
 * - 跳过说明常驻，提前告知降级影响，不靠弹窗事后告知
 */

import { computed } from 'vue'

import BaseButton from '../../components/base/BaseButton.vue'
import BaseInput from '../../components/base/BaseInput.vue'
import type { EmbeddingValidation } from '../../core/api/dto'
import type { SetupFieldErrors } from '../../core/setupWizard'
import SetupStatusLine from './SetupStatusLine.vue'

const props = defineProps<{
  errors: SetupFieldErrors
  validating: boolean
  validation: EmbeddingValidation | null
  skipped: boolean
}>()

const emit = defineEmits<{
  validate: []
  skip: []
}>()

const path = defineModel<string>('path', { required: true })

const statusTone = computed<'idle' | 'busy' | 'ok' | 'fail'>(() => {
  if (props.validating) {
    return 'busy'
  }
  if (props.validation) {
    return props.validation.valid ? 'ok' : 'fail'
  }
  return 'idle'
})

const statusText = computed(() => {
  if (props.validating) {
    return '正在校验模型文件…'
  }
  if (props.validation?.valid) {
    return `校验通过 · 维度 ${props.validation.dimension ?? '未知'}`
  }
  if (props.validation) {
    return props.validation.reason ?? '模型路径无效或文件不完整'
  }
  if (props.skipped) {
    return '已跳过：进入应用后向量检索关闭'
  }
  return '尚未校验模型'
})
</script>

<template>
  <section class="panel">
    <div class="panel-head">
      <h2 class="panel-title">配置嵌入模型</h2>
      <p class="panel-sub">用于向量检索。推荐 bge-small-zh-v1.5；也可以先跳过，稍后在设置中补充。</p>
    </div>

    <BaseInput
      v-model="path"
      label="模型文件夹路径"
      placeholder="例如 D:/models/bge-small-zh-v1.5"
      hint="路径应包含 config.json 与模型权重文件。"
      :error="errors.embeddingPath"
    />

    <div class="inline-status">
      <SetupStatusLine class="status-slot" :tone="statusTone" :text="statusText" />
      <BaseButton size="sm" variant="ghost" :loading="validating" @click="emit('validate')">
        校验模型路径
      </BaseButton>
    </div>

    <div class="note">
      未配置或跳过时进入降级模式：向量检索不可用，仅保留关键词与图谱通道；对话与浏览不受影响，可随时在设置中补配。
    </div>

    <div class="skip-row">
      <BaseButton size="sm" variant="ghost" @click="emit('skip')">跳过，稍后配置</BaseButton>
    </div>
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

.inline-status {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  padding: var(--space-10) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
}

.status-slot {
  flex: 1;
}

.note {
  padding: var(--space-12) var(--space-14);
  border: 1px solid var(--accent-line);
  border-radius: var(--radius-inner);
  background: var(--accent-surface);
  color: var(--accent-text);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}

.skip-row {
  display: flex;
  justify-content: flex-start;
}
</style>
