<script setup lang="ts">
/**
 * src/views/dev/gallery/ControlCells.vue — 控制件格位渲染
 *
 * 职责:
 * - 按行与状态短名渲染按钮、输入、下拉、开关、勾选、单选、分段与标签
 *
 * 设计特点:
 * - 每格只实例化一个组件，参数由状态短名直接映射，避免在模板里堆条件分支
 * - 展开态在挂载后由真实点击触发，与用户操作路径一致
 * - 交互态不复制生产样式，交由指针与键盘触发
 */
import { onMounted, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseCheckbox from '../../../components/base/BaseCheckbox.vue'
import BaseInput from '../../../components/base/BaseInput.vue'
import BaseRadio from '../../../components/base/BaseRadio.vue'
import BaseSegmented from '../../../components/base/BaseSegmented.vue'
import BaseSelect from '../../../components/base/BaseSelect.vue'
import BaseSwitch from '../../../components/base/BaseSwitch.vue'
import BaseTag from '../../../components/base/BaseTag.vue'

const props = defineProps<{
  row: string
  cell: string
}>()

const text = ref('Rama')
const selectHost = ref<HTMLElement | null>(null)
const selectValue = ref(props.cell === 'selected' ? 'online' : '')
const switchValue = ref(props.cell !== 'off')
const checkValue = ref(props.cell === 'checked' || props.cell === 'hover' || props.cell === 'focus')
const radioValue = ref(props.cell === 'checked' ? 'online' : 'local')
const segmentValue = ref(props.cell === 'selected' ? 'dark' : 'light')

const SELECT_OPTIONS = [
  { value: 'local', label: '本地推理服务' },
  { value: 'online', label: '线上 API' },
  { value: 'custom', label: '自定义地址' },
]

const SEGMENTS = [
  { value: 'light', label: '浅色' },
  { value: 'dark', label: '深色' },
]

/** 单选组名按格位区分，避免同页多个演示组互相取消 */
const radioName = `gallery-radio-${props.cell}`

onMounted(() => {
  if (props.row !== 'select' || props.cell !== 'open') {
    return
  }
  selectHost.value?.querySelector<HTMLButtonElement>('.select-trigger')?.click()
})
</script>

<template>
  <template v-if="row === 'button'">
    <BaseButton
      variant="primary"
      :loading="cell === 'loading'"
      :disabled="cell === 'disabled'"
    >保存并进入</BaseButton>
    <BaseButton v-if="cell === 'default'" variant="ghost">取消</BaseButton>
  </template>

  <BaseInput
    v-else-if="row === 'input'"
    v-model="text"
    label="显示名称"
    :hint="cell === 'validating' ? '校验中…' : '用于称呼与提示词'"
    :error="cell === 'invalid' ? '名称不能为空' : ''"
    :invalid="cell === 'invalid'"
    :validating="cell === 'validating'"
    :disabled="cell === 'disabled'"
  />

  <div v-else-if="row === 'select'" ref="selectHost" class="cell-block">
    <BaseSelect
      v-model="selectValue"
      label="推理服务"
      :options="SELECT_OPTIONS"
      hint="切换后需重新测试连接"
      :error="cell === 'invalid' ? '请选择一个服务' : ''"
      :invalid="cell === 'invalid'"
      :disabled="cell === 'disabled'"
    />
  </div>

  <BaseSwitch
    v-else-if="row === 'switch'"
    v-model="switchValue"
    label="空闲时自动保存"
    :saving="cell === 'saving'"
    :disabled="cell === 'disabled'"
  />

  <BaseCheckbox
    v-else-if="row === 'checkbox'"
    v-model="checkValue"
    label="我已知晓数据范围"
    :indeterminate="cell === 'indeterminate'"
    :invalid="cell === 'invalid'"
    :disabled="cell === 'disabled'"
  />

  <div v-else-if="row === 'radio'" class="cell-inline">
    <BaseRadio
      v-model="radioValue"
      :name="radioName"
      value="local"
      label="本地推理"
      :invalid="cell === 'invalid'"
      :disabled="cell === 'disabled'"
    />
    <BaseRadio
      v-model="radioValue"
      :name="radioName"
      value="online"
      label="线上 API"
      :invalid="cell === 'invalid'"
      :disabled="cell === 'disabled'"
    />
  </div>

  <BaseSegmented
    v-else-if="row === 'segmented'"
    v-model="segmentValue"
    :options="SEGMENTS"
    label="主题"
    :disabled="cell === 'disabled'"
  />

  <div v-else-if="row === 'tag'" class="cell-inline">
    <BaseTag
      :tone="cell === 'static' ? 'neutral' : 'brand'"
      :selected="cell === 'selected'"
      :removable="cell.startsWith('remove')"
    >默认人格</BaseTag>
    <BaseTag v-if="cell === 'static'" tone="warn">只读</BaseTag>
  </div>
</template>

<style scoped>
.cell-block {
  display: flex;
  flex-direction: column;
  gap: var(--space-8);
}

.cell-inline {
  display: flex;
  flex-wrap: wrap;
  align-items: center;
  gap: var(--space-12);
}
</style>
