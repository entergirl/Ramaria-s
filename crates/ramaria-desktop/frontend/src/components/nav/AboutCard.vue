<script setup lang="ts">
/**
 * src/components/nav/AboutCard.vue — 关于浮层
 *
 * 职责:
 * - 展示品牌、版本、仓库与许可，手动检查更新，提供调试开关
 *
 * 设计特点:
 * - 头部用 `head` 插槽承载品牌块（标识 / 名称 / 版本徽标 / 一句话说明），关闭键随行
 * - 版本与更新只读展示，检查更新是唯一动作，失败就地给原因；发行说明不进浮层
 * - 仓库地址点击即复制（不注册外部打开插件），复制结果用 Toast 反馈
 * - 调试开关即时生效：关闭时若停留在调试视图，由导航层负责退回对话
 */

import { computed, ref, watch } from 'vue'

import BaseButton from '../base/BaseButton.vue'
import BaseModal from '../base/BaseModal.vue'
import BaseSwitch from '../base/BaseSwitch.vue'
import BaseTag from '../base/BaseTag.vue'
import { diagnosticsApi, systemApi } from '../../core/api'
import type { UpdateStatus } from '../../core/api/dto'
import { errorMessage } from '../../core/errors'
import { pushToast } from '../../core/toast'
import { useDebugStore } from '../../stores/debug'

const props = defineProps<{
  open: boolean
}>()

const emit = defineEmits<{
  close: []
  'toggle-debug': [unlocked: boolean]
}>()

/** 仓库地址：显示短式，复制完整链接 */
const REPO_LABEL = 'github.com/entergirl/Ramaria-s'
const REPO_URL = `https://${REPO_LABEL}`

const debug = useDebugStore()

const version = ref('')
const versionError = ref('')
const update = ref<UpdateStatus | null>(null)
const updateError = ref('')
const checking = ref(false)

const versionText = computed(() => version.value || versionError.value || '读取中…')
const versionTag = computed(() => (version.value ? `v${version.value}` : '—'))

/** 更新结果：未检查 / 已是最新 / 有新版本 / 检查失败 */
const updateText = computed(() => {
  if (updateError.value) {
    return updateError.value
  }
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

/** 首次打开时读取版本；只读一次，失败保留文案 */
watch(
  () => props.open,
  async (open) => {
    if (!open || version.value || versionError.value) {
      return
    }
    try {
      version.value = await systemApi.getVersion()
    } catch (error) {
      versionError.value = errorMessage(error, '版本读取失败')
    }
  },
)

async function checkUpdate(): Promise<void> {
  if (checking.value) {
    return
  }
  checking.value = true
  updateError.value = ''
  try {
    update.value = await diagnosticsApi.checkUpdate()
  } catch (error) {
    updateError.value = errorMessage(error, '检查更新失败')
  } finally {
    checking.value = false
  }
}

/** 复制仓库地址：外部打开需额外插件，这里以复制代替 */
async function copyRepo(): Promise<void> {
  try {
    await navigator.clipboard.writeText(REPO_URL)
    pushToast({ tone: 'success', message: '仓库地址已复制' })
  } catch {
    pushToast({ tone: 'warn', message: '复制失败，请手动选择文本' })
  }
}

function onDebugChange(value: boolean): void {
  emit('toggle-debug', value)
}
</script>

<template>
  <BaseModal :open="props.open" label="关于 Ramaria" @close="emit('close')">
    <template #head>
      <div class="about-head">
        <span class="about-mark" aria-hidden="true">珊</span>
        <div class="about-head-text">
          <p class="about-name">
            珊瑚菌 Ramaria
            <BaseTag tone="brand">{{ versionTag }}</BaseTag>
          </p>
          <p class="about-sub">本地优先的个人 AI 陪伴记忆系统</p>
        </div>
      </div>
    </template>

    <div class="about-rows">
      <div class="row">
        <span class="row-label">版本</span>
        <span class="row-value mono">{{ versionText }}</span>
      </div>
      <div class="row">
        <span class="row-label">仓库</span>
        <button type="button" class="repo" title="复制仓库地址" @click="copyRepo">
          {{ REPO_LABEL }}
        </button>
      </div>
      <div class="row">
        <span class="row-label">License</span>
        <span class="row-value">MIT</span>
      </div>
      <div class="row">
        <span class="row-label">更新</span>
        <span class="row-value">{{ updateText }}</span>
        <BaseButton variant="ghost" size="sm" :loading="checking" @click="checkUpdate">
          检查更新
        </BaseButton>
      </div>
    </div>

    <div class="about-divider" />

    <section class="about-block">
      <h3 class="about-heading">调试</h3>
      <BaseSwitch :model-value="debug.unlocked" label="显示调试视图与进阶设置" @update:model-value="onDebugChange" />
      <p class="about-note">
        解锁后导航出现「调试」键，设置页出现「进阶设置」；关闭时若正停留在调试视图会自动退回对话。
      </p>
    </section>
  </BaseModal>
</template>

<style scoped>
.about-head {
  display: flex;
  flex: 1;
  align-items: center;
  gap: var(--space-12);
  min-width: 0;
}

.about-mark {
  display: grid;
  place-items: center;
  flex: none;
  width: 42px;
  height: 42px;
  border-radius: var(--radius-full);
  background: var(--btn-bg);
  color: var(--btn-fg);
  font-size: var(--fs-body-lg);
  font-weight: var(--fw-bold);
  box-shadow: var(--btn-shadow);
}

.about-head-text {
  min-width: 0;
}

.about-name {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-8);
  color: var(--text-1);
  font-size: var(--fs-h3);
  font-weight: var(--fw-bold);
  line-height: 1.3;
}

.about-sub {
  margin-top: var(--space-4);
  color: var(--text-3);
  font-size: var(--fs-small);
}

.about-rows {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.row {
  display: flex;
  align-items: center;
  gap: var(--space-10);
  font-size: var(--fs-small);
}

.row-label {
  flex: none;
  width: 60px;
  color: var(--text-3);
}

.row-value {
  flex: 1;
  min-width: 0;
  color: var(--text-1);
  word-break: break-all;
}

.mono {
  font-family: var(--font-mono);
  font-size: var(--fs-code);
}

/* 仓库地址：看起来是文本，点一下复制 */
.repo {
  flex: 1;
  min-width: 0;
  padding: 0;
  border: none;
  background: none;
  color: var(--text-1);
  font-family: var(--font-mono);
  font-size: var(--fs-code);
  text-align: left;
  word-break: break-all;
  cursor: copy;
}

.repo:hover {
  color: var(--brand-text);
  text-decoration: underline;
}

.repo:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
  border-radius: var(--radius-tag);
}

.about-divider {
  height: 1px;
  margin: var(--space-14) 0;
  background: var(--border);
}

.about-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.about-heading {
  font-size: var(--fs-h3);
  font-weight: var(--fw-semibold);
  line-height: var(--lh-h3);
}

.about-note {
  color: var(--text-3);
  font-size: var(--fs-small);
}
</style>
