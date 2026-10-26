<script setup lang="ts">
/**
 * src/views/chat/ChatImage.vue — 消息内图片
 *
 * 职责:
 * - 按消息标识与图片序号读取图片字节并渲染为图片
 * - 提供加载占位、长图标记、失败重试与不可读降级卡片
 *
 * 设计特点:
 * - 进入视口才发起读取，避免长会话一次性拉取全部图片
 * - MIME 由字节魔数嗅探，Blob URL 在用完后立即释放
 * - 读取或解码失败不抛错，降级为文本卡片或重试入口
 * - 高度按图片比例自适应，超长图收窄并标记「长图」
 */
import { onBeforeUnmount, onMounted, ref } from 'vue'

import BaseSkeleton from '../../components/base/BaseSkeleton.vue'
import { attachmentApi } from '../../core/api'
import { sniffImageMime } from '../../core/chat/imageText'

const props = defineProps<{
  messageId: string
  index: number
  desc: string
}>()

type ImageStatus = 'idle' | 'loading' | 'ready' | 'failed' | 'unavailable'

const status = ref<ImageStatus>('idle')
const url = ref('')
const tall = ref(false)
let objectUrl = ''
let observer: IntersectionObserver | null = null
let disposed = false

/** 长图判定：高度超过宽度的 2.2 倍 */
const TALL_RATIO = 2.2

/** 释放 Blob URL；已解码的位图仍由元素持有，可见性不受影响 */
function releaseBlob(): void {
  if (objectUrl) {
    URL.revokeObjectURL(objectUrl)
    objectUrl = ''
  }
}

/** 释放并清空绑定地址（重载 / 卸载时使用） */
function revoke(): void {
  releaseBlob()
  url.value = ''
}

async function load(): Promise<void> {
  if (status.value === 'loading' || status.value === 'ready') {
    return
  }
  status.value = 'loading'
  try {
    const bytes = await attachmentApi.readImage(props.messageId, props.index)
    if (disposed) {
      return
    }
    if (!bytes || (bytes as ArrayBuffer).byteLength === 0) {
      status.value = 'unavailable'
      return
    }
    const blob = new Blob([bytes], { type: sniffImageMime(bytes) })
    objectUrl = URL.createObjectURL(blob)
    url.value = objectUrl
  } catch {
    if (!disposed) {
      status.value = 'unavailable'
    }
  }
}

function onLoad(event: Event): void {
  const image = event.target as HTMLImageElement
  if (image.naturalWidth > 0) {
    tall.value = image.naturalHeight / image.naturalWidth > TALL_RATIO
  }
  status.value = 'ready'
  releaseBlob()
}

function onError(): void {
  revoke()
  status.value = 'failed'
}

function retry(): void {
  status.value = 'idle'
  tall.value = false
  void load()
}

onMounted(() => {
  if (typeof window !== 'undefined' && 'IntersectionObserver' in window) {
    observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) {
        observer?.disconnect()
        observer = null
        void load()
      }
    })
    const host = document.getElementById(`image-${props.messageId}-${props.index}`)
    if (host) {
      observer.observe(host)
    } else {
      void load()
    }
    return
  }
  void load()
})

onBeforeUnmount(() => {
  disposed = true
  observer?.disconnect()
  observer = null
  revoke()
})
</script>

<template>
  <figure
    v-if="status === 'ready'"
    :id="`image-${messageId}-${index}`"
    class="msg-image"
    :data-tall="tall ? '' : undefined"
  >
    <span v-if="tall" class="image-badge">长图</span>
    <img :src="url" :alt="desc || '图片'" @load="onLoad" @error="onError" />
    <figcaption class="image-cap">
      <span class="cap-name">{{ desc || '图片' }}</span>
    </figcaption>
  </figure>

  <button
    v-else-if="status === 'failed'"
    :id="`image-${messageId}-${index}`"
    type="button"
    class="msg-image is-failed"
    @click="retry"
  >
    <span class="fail-text">图片加载失败</span>
    <span class="image-cap">
      <span class="cap-name">{{ desc || '图片' }}</span>
      <span class="cap-action">重新加载</span>
    </span>
  </button>

  <div
    v-else-if="status === 'unavailable'"
    :id="`image-${messageId}-${index}`"
    class="image-card"
  >
    <span class="card-icon" aria-hidden="true">🖼</span>
    <span class="cap-name">{{ desc || '图片不可读取' }}</span>
  </div>

  <div
    v-else
    :id="`image-${messageId}-${index}`"
    class="msg-image is-loading"
    aria-hidden="true"
  >
    <BaseSkeleton shape="card" :lines="1" />
  </div>
</template>

<style scoped>
.msg-image {
  position: relative;
  display: flex;
  flex-direction: column;
  width: 236px;
  max-width: 100%;
  margin: 0;
  padding: 0;
  overflow: hidden;
  border: 1px solid var(--border);
  border-radius: var(--radius-inner);
  background: var(--bg-card);
  text-align: left;
}

.msg-image img {
  display: block;
  width: 100%;
  max-height: 320px;
  object-fit: cover;
}

.msg-image[data-tall] img {
  max-height: 420px;
  object-fit: contain;
  background: var(--bg-sunken);
}

.image-badge {
  position: absolute;
  top: var(--space-8);
  right: var(--space-8);
  z-index: var(--z-sticky);
  padding: 2px var(--space-8);
  border-radius: var(--radius-tag);
  background: var(--overlay-scrim);
  color: var(--bg-card);
  font-size: var(--fs-caption);
  font-weight: var(--fw-bold);
}

.image-cap {
  display: flex;
  align-items: center;
  justify-content: space-between;
  gap: var(--space-8);
  padding: var(--space-6) var(--space-10);
  border-top: 1px solid var(--border);
  color: var(--text-3);
  font-size: var(--fs-caption);
}

.cap-name {
  overflow: hidden;
  text-overflow: ellipsis;
  white-space: nowrap;
}

.cap-action {
  flex: none;
  color: var(--brand-text);
}

.msg-image.is-failed {
  cursor: pointer;
  background: var(--bg-subtle);
}

.msg-image.is-failed:hover {
  border-color: var(--brand-line);
}

.msg-image.is-failed:focus-visible {
  outline: none;
  box-shadow: var(--focus-ring);
}

.fail-text {
  display: grid;
  place-items: center;
  height: 116px;
  color: var(--text-3);
  font-size: var(--fs-small);
}

.image-card {
  display: flex;
  align-items: center;
  gap: var(--space-8);
  padding: var(--space-10) var(--space-12);
  border: 1px dashed var(--border-strong);
  border-radius: var(--radius-inner);
  background: var(--bg-subtle);
  color: var(--text-3);
  font-size: var(--fs-small);
}

.card-icon {
  flex: none;
}

.msg-image.is-loading {
  padding: var(--space-10);
  border-style: dashed;
}
</style>
