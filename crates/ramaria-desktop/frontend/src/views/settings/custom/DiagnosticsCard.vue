<script setup lang="ts">
/**
 * src/views/settings/custom/DiagnosticsCard.vue — 诊断与更新卡
 *
 * 职责:
 * - 展示当前版本，手动检查更新，导出脱敏诊断包
 *
 * 设计特点:
 * - 版本读取纯本地；检查更新与诊断导出各自独立加载态
 * - 诊断包路径由原生保存对话框决定，前端不传路径
 * - 失败留在卡内，不影响其它设置
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { diagnosticsApi, systemApi } from '../../../core/api'
import type { DiagnosticsExport, UpdateStatus } from '../../../core/api/dto'
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

const version = ref('')
const versionError = ref('')
const update = ref<UpdateStatus | null>(null)
const checking = ref(false)
const exporting = ref(false)
const exported = ref<DiagnosticsExport | null>(null)
const error = ref('')

const versionText = computed(() => (version.value ? `v${version.value}` : versionError.value || '读取中…'))

const indexText = computed(() => indexStatusText(app.indexStatus))
const indexTone = computed(() => indexStatusTone(app.indexStatus))
const indexCoverage = computed(() => indexCoverageText(app.indexStatus))
const indexModel = computed(() => indexModelText(app.indexStatus))

const updateText = computed(() => {
  if (!update.value) {
    return '尚未检查'
  }
  if (update.value.error) {
    return `检查失败：${update.value.error}`
  }
  if (update.value.updateAvailable) {
    return `有新版本 ${update.value.latestVersion ?? ''}`
  }
  return '已是最新版本'
})

async function loadVersion(): Promise<void> {
  try {
    version.value = await systemApi.getVersion()
    versionError.value = ''
  } catch (cause) {
    versionError.value = errorMessage(cause, '版本读取失败')
  }
}

async function checkUpdate(): Promise<void> {
  if (checking.value) {
    return
  }
  checking.value = true
  error.value = ''
  try {
    update.value = await diagnosticsApi.checkUpdate()
  } catch (cause) {
    error.value = errorMessage(cause, '检查更新失败')
  } finally {
    checking.value = false
  }
}

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

onMounted(() => {
  void loadVersion()
})
</script>

<template>
  <SettingsCard
    title="诊断与更新"
    description="导出日志、配置与索引信息（不含原文与密钥）。"
    :editable="false"
    :error="error"
  >
    <template #head>
      <BaseTag tone="neutral">{{ versionText }}</BaseTag>
    </template>

    <div class="row">
      <span class="row-label">版本</span>
      <span class="row-value">{{ versionText }}</span>
    </div>
    <div class="row">
      <span class="row-label">更新</span>
      <span class="row-value">{{ updateText }}</span>
    </div>
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
    <p v-if="update?.releaseNotesPreview" class="note">{{ update.releaseNotesPreview }}</p>
    <p v-if="exported" class="note">
      已导出 {{ exported.fileSizeDisplay }}（{{ Object.keys(exported.collectionStatus).length }} 项已收集）
    </p>
    <p v-if="exported && exported.warnings.length > 0" class="note">
      警告：{{ exported.warnings.join('；') }}
    </p>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="checking" @click="checkUpdate">
        检查更新
      </BaseButton>
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
