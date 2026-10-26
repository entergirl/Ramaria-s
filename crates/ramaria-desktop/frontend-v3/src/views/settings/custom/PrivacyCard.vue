<script setup lang="ts">
/**
 * src/views/settings/custom/PrivacyCard.vue — 隐私确认卡
 *
 * 职责:
 * - 展示当前后端是否需要隐私确认，并提供确认入口
 *
 * 设计特点:
 * - 状态一次性读取；确认后立即重读，不缓存结论
 * - 只回显确认时间与范围，不展示密钥或请求内容
 * - 读取失败留在卡内，不阻塞其它设置
 */

import { computed, onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'
import { chatApi } from '../../../core/api'
import type { PrivacyStatus } from '../../../core/api/dto'
import { errorMessage } from '../../../core/errors'
import { pushToast } from '../../../core/toast'
import SettingsCard from '../SettingsCard.vue'

const status = ref<PrivacyStatus | null>(null)
const loading = ref(false)
const confirming = ref(false)
const error = ref('')

const needsConfirm = computed(() => status.value?.status === 'NeedsConfirmation')

const statusText = computed(() => {
  if (!status.value) {
    return loading.value ? '读取中…' : '未知'
  }
  switch (status.value.status) {
    case 'NotNeeded':
      return '本地模式，无需确认'
    case 'Confirmed':
      return status.value.persistent ? '已确认（跨重启保留）' : '已确认（本次有效）'
    case 'NeedsConfirmation':
      return '需要确认'
    default:
      return '无法判断'
  }
})

const scopeText = computed(() => {
  if (status.value?.status !== 'NeedsConfirmation') {
    return ''
  }
  return `${status.value.provider_name} · ${status.value.base_url}`
})

const timeText = computed(() => {
  if (status.value?.status !== 'Confirmed') {
    return ''
  }
  return new Date(status.value.confirmed_at).toLocaleString('zh-CN', { hour12: false })
})

async function load(): Promise<void> {
  if (loading.value) {
    return
  }
  loading.value = true
  error.value = ''
  try {
    status.value = await chatApi.checkPrivacy()
  } catch (cause) {
    error.value = errorMessage(cause, '隐私状态读取失败')
  } finally {
    loading.value = false
  }
}

async function confirm(): Promise<void> {
  if (confirming.value) {
    return
  }
  confirming.value = true
  try {
    await chatApi.confirmPrivacy(false)
    pushToast({ tone: 'success', message: '已记录本次隐私确认' })
    await load()
  } catch (cause) {
    pushToast({ tone: 'danger', message: errorMessage(cause, '隐私确认失败') })
  } finally {
    confirming.value = false
  }
}

onMounted(() => {
  void load()
})
</script>

<template>
  <SettingsCard
    title="隐私确认"
    description="线上后端注入记忆前需确认数据范围；换 provider 或地址需重新确认。"
    :editable="false"
    :error="error"
  >
    <template #head>
      <BaseTag v-if="status?.status === 'NotNeeded'" tone="neutral">本地</BaseTag>
      <BaseTag v-else-if="status?.status === 'Confirmed'" tone="ok">已确认</BaseTag>
      <BaseTag v-else-if="needsConfirm" tone="warn">待确认</BaseTag>
    </template>

    <div class="row">
      <span class="row-label">状态</span>
      <span class="row-value">{{ statusText }}</span>
    </div>
    <div v-if="scopeText" class="row">
      <span class="row-label">确认范围</span>
      <span class="row-value">{{ scopeText }}</span>
    </div>
    <div v-if="timeText" class="row">
      <span class="row-label">确认时间</span>
      <span class="row-value">{{ timeText }}</span>
    </div>

    <p class="note">未确认前不发起注入记忆的对话；本地模式不出现该确认。</p>

    <template #footExtra>
      <BaseButton variant="ghost" size="sm" :loading="loading" @click="load">重新检查</BaseButton>
      <BaseButton
        v-if="needsConfirm"
        size="sm"
        :loading="confirming"
        @click="confirm"
      >
        确认本次使用
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
  width: 64px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  word-break: break-all;
}

.note {
  margin: 0;
  color: var(--text-3);
  font-size: var(--fs-caption);
  line-height: 1.6;
}
</style>
