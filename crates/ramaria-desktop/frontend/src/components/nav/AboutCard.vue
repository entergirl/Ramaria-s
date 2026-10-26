<script setup lang="ts">
/**
 * src/components/nav/AboutCard.vue — 关于浮层
 *
 * 职责:
 * - 展示版本号与索引状态摘要，提供导航位置预设与调试开关
 *
 * 设计特点:
 * - 版本与索引摘要只读，不在此处触发重建
 * - 导航位置用分段控件表达 5 个预设，选中即生效
 * - 调试开关即时生效：关闭时若停留在调试视图，由导航层负责退回对话
 */

import { computed, ref, watch } from 'vue'

import BaseModal from '../base/BaseModal.vue'
import BaseSegmented from '../base/BaseSegmented.vue'
import BaseSwitch from '../base/BaseSwitch.vue'
import { systemApi } from '../../core/api'
import { errorMessage } from '../../core/errors'
import { NAV_PRESET_LABELS, type NavPreset } from './geometry'
import { useAppStore } from '../../stores/app'
import { useDebugStore } from '../../stores/debug'

const props = defineProps<{
  open: boolean
  preset: NavPreset
}>()

const emit = defineEmits<{
  close: []
  'select-preset': [preset: NavPreset]
  'toggle-debug': [unlocked: boolean]
}>()

const app = useAppStore()
const debug = useDebugStore()

const version = ref('')
const versionError = ref('')

const presetOptions = (Object.keys(NAV_PRESET_LABELS) as NavPreset[]).map((value) => ({
  value,
  label: NAV_PRESET_LABELS[value],
}))

/** 索引摘要：状态 + 进度 / 缺失数，避免只显示状态枚举 */
const indexText = computed(() => {
  const status = app.indexStatus
  if (!status) {
    return '未知'
  }
  if (status.status === 'building') {
    return `构建中 ${status.percent}%`
  }
  if (status.status === 'ready') {
    return status.failed > 0 ? `已就绪 · ${status.failed} 条待补` : '已就绪'
  }
  if (status.status === 'idle') {
    return '未建立'
  }
  return '构建失败'
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

function onPresetChange(value: string): void {
  emit('select-preset', value as NavPreset)
}

function onDebugChange(value: boolean): void {
  emit('toggle-debug', value)
}
</script>

<template>
  <BaseModal :open="props.open" title="关于 Ramaria" @close="emit('close')">
    <dl class="about-list">
      <dt>版本</dt>
      <dd>{{ version || versionError || '读取中…' }}</dd>
      <dt>索引</dt>
      <dd>{{ indexText }}</dd>
    </dl>

    <section class="about-block">
      <h3 class="about-heading">导航位置</h3>
      <BaseSegmented
        :model-value="props.preset"
        :options="presetOptions"
        label="导航位置预设"
        @update:model-value="onPresetChange"
      />
      <p class="about-note">拖动核心圆键可自由摆放，双击回到所选预设。</p>
    </section>

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
.about-list {
  display: grid;
  grid-template-columns: auto 1fr;
  gap: var(--space-8) var(--space-16);
  margin: 0 0 var(--gap-section);
  font-size: var(--fs-small);
}

.about-list dt {
  color: var(--text-3);
}

.about-list dd {
  margin: 0;
  color: var(--text-1);
  font-family: var(--font-mono);
}

.about-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
  margin-bottom: var(--gap-section);
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
