<script setup lang="ts">
/**
 * src/views/dev/gallery/FeedbackCells.vue — 反馈件格位渲染
 *
 * 职责:
 * - 按行与状态短名渲染模态、轻提示、进度、骨架与空态
 *
 * 设计特点:
 * - 模态挂在浮层上，格位内只留一个打开入口，避免遮住整页
 * - 轻提示直接内联渲染，不经过队列，便于并排比对类型
 * - 进度与骨架的终态只改状态参数，形状保持一致
 */
import { computed, ref } from 'vue'

import BaseButton from '../../../components/base/BaseButton.vue'
import BaseEmptyState from '../../../components/base/BaseEmptyState.vue'
import BaseModal from '../../../components/base/BaseModal.vue'
import BaseProgress from '../../../components/base/BaseProgress.vue'
import BaseSkeleton from '../../../components/base/BaseSkeleton.vue'
import BaseToast from '../../../components/base/BaseToast.vue'

const props = defineProps<{
  row: string
  cell: string
}>()

const open = ref(false)

const modalTitle = computed(() => {
  if (props.cell === 'danger') {
    return '删除这个人格'
  }
  if (props.cell === 'scroll') {
    return '提示词查看'
  }
  return '撤回这条消息'
})

const progressValue = computed(() => {
  if (props.cell === 'determinate') {
    return 42
  }
  if (props.cell === 'success') {
    return 100
  }
  if (props.cell === 'failed') {
    return 68
  }
  return undefined
})

const progressState = computed(() => {
  if (props.cell === 'success') {
    return 'success' as const
  }
  if (props.cell === 'failed') {
    return 'failed' as const
  }
  if (props.cell === 'timeout') {
    return 'timeout' as const
  }
  return 'running' as const
})

const progressLabel = computed(() => {
  if (props.cell === 'success') {
    return '构建完成'
  }
  if (props.cell === 'failed') {
    return '构建失败'
  }
  if (props.cell === 'timeout') {
    return '等待超时'
  }
  return '索引构建'
})

const skeletonShape = computed(() => {
  if (props.cell === 'static' || props.cell === 'text') {
    return 'text' as const
  }
  return props.cell as 'avatar' | 'card' | 'bubble' | 'table'
})

const emptyVariant = computed(() => (props.cell === 'action' ? 'action' : props.cell === 'blocked' ? 'blocked' : 'guide'))
</script>

<template>
  <template v-if="row === 'modal'">
    <BaseButton variant="secondary" size="sm" @click="open = true">打开预览</BaseButton>
    <BaseModal
      :open="open"
      :title="modalTitle"
      :variant="cell === 'danger' ? 'danger' : 'default'"
      :loading="cell === 'loading'"
      @close="open = false"
    >
      <p class="mock-text">撤回后消息内容在界面中折叠为系统提示，且无法恢复。</p>
      <template v-if="cell === 'scroll'">
        <p v-for="index in 8" :key="index" class="mock-text">
          第 {{ index }} 段：提示词正文在框内滚动，标题与页脚固定。
        </p>
      </template>
      <template #foot>
        <BaseButton variant="ghost" @click="open = false">取消</BaseButton>
        <BaseButton
          :variant="cell === 'danger' ? 'danger' : 'primary'"
          :loading="cell === 'loading'"
        >确认</BaseButton>
      </template>
    </BaseModal>
  </template>

  <BaseToast
    v-else-if="row === 'toast'"
    :tone="cell === 'action' ? 'info' : (cell as 'info' | 'success' | 'warn' | 'danger')"
    :message="
      cell === 'success'
        ? '对话已保存，摘要已生成'
        : cell === 'warn'
          ? '对话已保存，摘要未生成，可重试'
          : cell === 'danger'
            ? '索引构建失败，可稍后重试'
            : '已切换到本地推理服务'
    "
    :action-label="cell === 'action' ? '查看设置' : ''"
  />

  <BaseProgress
    v-else-if="row === 'progress'"
    :value="progressValue"
    :label="progressLabel"
    :state="progressState"
  />

  <BaseSkeleton
    v-else-if="row === 'skeleton'"
    :shape="skeletonShape"
    :lines="skeletonShape === 'text' ? 3 : 2"
    :motion="cell === 'static' ? 'static' : 'shimmer'"
  />

  <BaseEmptyState
    v-else-if="row === 'empty'"
    :variant="emptyVariant"
    :title="cell === 'blocked' ? '嵌入模型不可用' : cell === 'action' ? '还没有记忆' : '开始一段对话'"
    :description="
      cell === 'blocked'
        ? '语义检索关闭，浏览与对话仍可用。'
        : cell === 'action'
          ? '导入聊天记录后会自动沉淀为分层记忆。'
          : '从一句问候开始，记忆会在对话中慢慢积累。'
    "
  >
    <BaseButton v-if="cell !== 'guide'" :variant="cell === 'blocked' ? 'secondary' : 'primary'" size="sm">
      {{ cell === 'blocked' ? '去设置' : '去导入' }}
    </BaseButton>
  </BaseEmptyState>
</template>

<style scoped>
.mock-text {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}
</style>
