<script setup lang="ts">
/**
 * src/views/settings/custom/DataCard.vue — 数据管理卡
 *
 * 职责:
 * - 导出会话数据、清理图片缓存、查看与重建索引、设置发图目录
 *
 * 设计特点:
 * - 导出路径与目录一律由原生对话框选择，前端不拼路径
 * - 索引状态来自全局 store（事件推进），本卡只触发重建与刷新
 * - 缓存只显示本轮清理结果；目录切换立即落库，不参与区块保存
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseModal from '../../../components/base/BaseModal.vue'
import BaseProgress from '../../../components/base/BaseProgress.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { attachmentApi, exportApi, systemApi } from '../../../core/api'
import type { ClearCacheResult } from '../../../core/api/dto'
import { errorMessage } from '../../../core/errors'
import { indexStatusText, indexStatusTone } from '../../../core/indexStatus'
import { pushToast } from '../../../core/toast'
import { useAppStore } from '../../../stores/app'
import SettingsCard from '../SettingsCard.vue'

const app = useAppStore()

const exporting = ref('')
const clearing = ref(false)
const rebuilding = ref(false)
const confirmRebuild = ref(false)
const cleared = ref<ClearCacheResult | null>(null)
const imageDir = ref('')
const savingDir = ref(false)
const error = ref('')

const indexStatus = computed(() => app.indexStatus)

const indexText = computed(() => indexStatusText(app.indexStatus))

const indexTagTone = computed(() => indexStatusTone(app.indexStatus))

function formatBytes(bytes: number): string {
  if (bytes < 1024) {
    return `${bytes} B`
  }
  if (bytes < 1024 * 1024) {
    return `${(bytes / 1024).toFixed(1)} KB`
  }
  return `${(bytes / (1024 * 1024)).toFixed(1)} MB`
}

async function exportData(kind: 'json' | 'markdown'): Promise<void> {
  if (exporting.value) {
    return
  }
  const extension = kind === 'json' ? 'json' : 'md'
  exporting.value = kind
  error.value = ''
  try {
    const path = await systemApi.saveFileDialog(`ramaria-sessions.${extension}`, extension.toUpperCase())
    if (!path) {
      return
    }
    const written = kind === 'json' ? await exportApi.json(path) : await exportApi.markdown(path)
    pushToast({ tone: 'success', message: `已导出到 ${written}` })
  } catch (cause) {
    error.value = errorMessage(cause, '导出失败')
  } finally {
    exporting.value = ''
  }
}

async function clearCache(): Promise<void> {
  if (clearing.value) {
    return
  }
  clearing.value = true
  error.value = ''
  try {
    cleared.value = await attachmentApi.clearCache()
    pushToast({ tone: 'success', message: `已清理 ${cleared.value.files} 个缓存文件` })
  } catch (cause) {
    error.value = errorMessage(cause, '清理图片缓存失败')
  } finally {
    clearing.value = false
  }
}

async function rebuild(): Promise<void> {
  if (rebuilding.value) {
    return
  }
  rebuilding.value = true
  confirmRebuild.value = false
  error.value = ''
  try {
    const started = await app.rebuildIndex()
    pushToast({
      tone: started ? 'success' : 'warn',
      message: started ? '索引重建已开始，进度显示在此卡' : '已有构建在途，不重复启动',
    })
  } catch (cause) {
    error.value = errorMessage(cause, '重建索引失败')
  } finally {
    rebuilding.value = false
  }
}

async function loadImageDir(): Promise<void> {
  try {
    imageDir.value = await systemApi.getImageSourceDir()
  } catch (cause) {
    error.value = errorMessage(cause, '发图目录读取失败')
  }
}

async function chooseImageDir(): Promise<void> {
  if (savingDir.value) {
    return
  }
  savingDir.value = true
  error.value = ''
  try {
    const picked = await systemApi.openDirectoryDialog(imageDir.value || null, '选择发图目录')
    if (picked) {
      imageDir.value = await systemApi.setImageSourceDir(picked)
      pushToast({ tone: 'success', message: '发图目录已更新' })
    }
  } catch (cause) {
    error.value = errorMessage(cause, '设置发图目录失败')
  } finally {
    savingDir.value = false
  }
}

async function resetImageDir(): Promise<void> {
  if (savingDir.value) {
    return
  }
  savingDir.value = true
  error.value = ''
  try {
    imageDir.value = await systemApi.setImageSourceDir(null)
    pushToast({ tone: 'success', message: '已恢复默认发图目录' })
  } catch (cause) {
    error.value = errorMessage(cause, '恢复默认发图目录失败')
  } finally {
    savingDir.value = false
  }
}

onMounted(() => {
  void loadImageDir()
  void app.refreshIndex()
})
</script>

<template>
  <SettingsCard
    title="数据管理"
    description="导出会话数据、清理图片缓存、重建索引与设置发图目录。"
    :editable="false"
    :error="error"
  >
    <template #head>
      <BaseTag :tone="indexTagTone">索引 {{ indexText }}</BaseTag>
    </template>

    <div class="row">
      <span class="row-label">导出</span>
      <div class="row-actions">
        <BaseButton variant="ghost" size="sm" :loading="exporting === 'json'" @click="exportData('json')">
          JSON
        </BaseButton>
        <BaseButton
          variant="ghost"
          size="sm"
          :loading="exporting === 'markdown'"
          @click="exportData('markdown')"
        >
          Markdown
        </BaseButton>
      </div>
    </div>

    <div class="row">
      <span class="row-label">图片缓存</span>
      <span class="row-value">
        {{ cleared ? `已清理 ${cleared.files} 个文件 · ${formatBytes(cleared.bytes)}` : '未清理' }}
      </span>
      <BaseButton variant="ghost" size="sm" :loading="clearing" @click="clearCache">清理</BaseButton>
    </div>

    <div class="row row--stack">
      <span class="row-label">索引</span>
      <span class="row-value">{{ indexText }}</span>
      <BaseButton variant="ghost" size="sm" :disabled="indexStatus?.status === 'building'" @click="confirmRebuild = true">
        重建索引
      </BaseButton>
    </div>
    <BaseProgress
      v-if="indexStatus?.status === 'building'"
      :value="indexStatus.percent"
      label="索引构建中（可继续对话）"
    />

    <div class="row row--stack">
      <span class="row-label">发图目录</span>
      <span class="row-value path">{{ imageDir || '读取中…' }}</span>
    </div>
    <p class="note">只接受该目录内的图片；恢复默认使用数据目录下的 outgoing。</p>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="savingDir" @click="chooseImageDir">选择目录</BaseButton>
      <BaseButton variant="ghost" size="sm" :disabled="savingDir" @click="resetImageDir">恢复默认目录</BaseButton>
    </template>
  </SettingsCard>

  <BaseModal :open="confirmRebuild" title="重建索引？" @close="confirmRebuild = false">
    <p class="modal-copy">
      重建会重新生成向量与 BM25 索引，已有对话与记忆不受影响；构建期间可继续对话，召回暂不含最新记忆。
    </p>
    <template #foot>
      <span class="spacer" />
      <BaseButton variant="ghost" @click="confirmRebuild = false">取消</BaseButton>
      <BaseButton :loading="rebuilding" @click="rebuild">开始重建</BaseButton>
    </template>
  </BaseModal>
</template>

<style scoped>
.row {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  font-size: var(--fs-small);
}

.row--stack {
  align-items: flex-start;
}

.row-label {
  flex: none;
  width: 64px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  line-height: 1.6;
  word-break: break-all;
}

.row-actions {
  display: flex;
  flex: 1;
  gap: var(--space-6);
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

.modal-copy {
  margin: 0;
  color: var(--text-1);
  font-size: var(--fs-body);
  line-height: 1.7;
}

.spacer {
  flex: 1;
}
</style>
