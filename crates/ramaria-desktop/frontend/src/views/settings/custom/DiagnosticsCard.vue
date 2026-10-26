<script setup lang="ts">
/**
 * src/views/settings/custom/DiagnosticsCard.vue — 诊断与更新卡
 *
 * 职责:
 * - 展示索引状态（状态 / 向量覆盖 / 模型），导出脱敏诊断包
 *
 * 设计特点:
 * - 索引信息只读，以「关于」浮层之外的唯一落点承接
 * - 诊断包路径由原生保存对话框决定，前端不传路径
 * - 失败留在卡内，不影响其它设置
 */

import { computed, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { diagnosticsApi } from '../../../core/api'
import type { DiagnosticsExport } from '../../../core/api/dto'
import { errorMessage } from '../../../core/errors'
import {
  indexCoverageText,
  indexModelText,
  indexStatusText,
  indexStatusTone,
} from '../../../core/indexStatus'
import { pushToast } from '../../../core/toast'
import { useAppStore } from '../../../stores/app'
import SettingsCard from '../SettingsCard.vue'

const app = useAppStore()

const exporting = ref(false)
const exported = ref<DiagnosticsExport | null>(null)
const error = ref('')

const indexText = computed(() => indexStatusText(app.indexStatus))
const indexTone = computed(() => indexStatusTone(app.indexStatus))
const indexCoverage = computed(() => indexCoverageText(app.indexStatus))
const indexModel = computed(() => indexModelText(app.indexStatus))

async function exportDiagnostics(): Promise<void> {
  if (exporting.value) {
    return
  }
  exporting.value = true
  error.value = ''
  try {
    exported.value = await diagnosticsApi.exportDiagnostics()
    pushToast({ tone: 'success', message: `诊断包已导出（${exported.value.fileSizeDisplay}）` })
  } catch (cause) {
    error.value = errorMessage(cause, '诊断导出失败')
  } finally {
    exporting.value = false
  }
}
</script>

<template>
  <SettingsCard
    title="诊断与更新"
    description="导出日志、配置与索引信息（不含原文与密钥）。"
    :editable="false"
    :error="error"
  >
    <div class="row">
      <span class="row-label">索引</span>
      <span class="row-value">
        <BaseTag :tone="indexTone">{{ indexText }}</BaseTag>
      </span>
    </div>
    <div class="row">
      <span class="row-label">向量</span>
      <span class="row-value">{{ indexCoverage }}</span>
    </div>
    <div class="row">
      <span class="row-label">模型</span>
      <span class="row-value path">{{ indexModel }}</span>
    </div>
    <p v-if="exported" class="note">
      已导出 {{ exported.fileSizeDisplay }}（{{ Object.keys(exported.collectionStatus).length }} 项已收集）
    </p>
    <p v-if="exported && exported.warnings.length > 0" class="note">
      警告：{{ exported.warnings.join('；') }}
    </p>

    <template #footExtra>
      <BaseButton variant="secondary" size="sm" :loading="exporting" @click="exportDiagnostics">
        导出诊断信息
      </BaseButton>
    </template>
  </SettingsCard>
</template>

<style scoped>
.row {
  display: flex;
  align-items: baseline;
  gap: var(--space-10);
  font-size: var(--fs-small);
}

.row-label {
  flex: none;
  width: 48px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  word-break: break-all;
}

.path {
  font-family: var(--font-mono);
  font-size: var(--fs-code);
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}
</style>
