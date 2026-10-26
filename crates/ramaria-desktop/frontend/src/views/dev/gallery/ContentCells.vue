<script setup lang="ts">
/**
 * src/views/dev/gallery/ContentCells.vue — 内容件格位渲染
 *
 * 职责:
 * - 按行与状态短名渲染卡片与消息气泡
 *
 * 设计特点:
 * - 卡片加载态由外框补骨架，组件本身只表达可点击与状态标记
 * - 气泡的发言人区只在群聊形态出现，与生产用法一致
 */
import BaseBubble from '../../../components/base/BaseBubble.vue'
import BaseCard from '../../../components/base/BaseCard.vue'
import BaseSkeleton from '../../../components/base/BaseSkeleton.vue'
import BaseTag from '../../../components/base/BaseTag.vue'

defineProps<{
  row: string
  cell: string
}>()
</script>

<template>
  <BaseCard
    v-if="row === 'card'"
    :clickable="cell === 'hover'"
    :selected="cell === 'selected'"
    :state="cell === 'loading' ? 'loading' : cell === 'error' ? 'error' : 'default'"
    :dirty="cell === 'dirty'"
    :dimmed="cell === 'dimmed'"
    error-text="这张记忆卡读取失败，可重试"
  >
    <BaseSkeleton v-if="cell === 'loading'" shape="text" :lines="2" />
    <template v-else>
      <p class="mock-title">周三的搬家</p>
      <p class="mock-text">情绪从紧张转向期待，提到想给阳台添一盆植物。</p>
      <BaseTag tone="brand">L1 摘要</BaseTag>
    </template>
  </BaseCard>

  <div v-else-if="row === 'bubble'" class="bubble-wrap" :data-role="cell">
    <BaseBubble
      :role="cell === 'user' ? 'user' : cell === 'system' ? 'system' : 'assistant'"
      :variant="
        cell === 'streaming'
          ? 'streaming'
          : cell === 'recalled'
            ? 'recalled'
            : cell === 'proactive'
              ? 'proactive'
              : cell === 'failed'
                ? 'failed'
                : 'normal'
      "
      :name="cell === 'group' ? '小珊' : cell === 'proactive' ? 'Rama' : ''"
      :avatar="cell === 'group' ? '珊' : ''"
      :tone="cell === 'group' ? 'accent' : 'default'"
    >
      <span class="mock-text">
        {{
          cell === 'system'
            ? '对话已封存，摘要已生成'
            : cell === 'failed'
              ? '发送失败，请检查网络后重试'
              : cell === 'streaming'
                ? '那就先歇一会儿'
                : '今天有点累，想早点睡'
        }}
      </span>
    </BaseBubble>
  </div>
</template>

<style scoped>
.bubble-wrap {
  display: flex;
  justify-content: flex-start;
}

.bubble-wrap[data-role='user'] {
  justify-content: flex-end;
}

.mock-title {
  color: var(--text-1);
  font-size: var(--fs-body);
  font-weight: var(--fw-semibold);
}

.mock-text {
  color: var(--text-2);
  font-size: var(--fs-small);
  line-height: var(--lh-small);
}
</style>
