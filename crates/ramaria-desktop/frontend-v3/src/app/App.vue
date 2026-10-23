<script setup lang="ts">
/**
 * src/app/App.vue — 应用根组件
 *
 * 职责:
 * - 挂载视图出口与全局提示层
 * - 启动引导应用状态，并在状态变化时跟随跳转全屏页 / 主界面
 *
 * 设计特点:
 * - 状态跟随只在根组件一处 watch，视图不各自处理路由
 * - 首次引导前按 needs_setup 处理，进入全屏配置页不闪烁主界面
 * - 导航框架随 M5-004 接入，此处只保留出口
 */

import { onMounted, watch } from 'vue'
import { RouterView, useRoute, useRouter } from 'vue-router'

import ToastLayer from '../components/base/ToastLayer.vue'
import NavDock from '../components/nav/NavDock.vue'
import { FULLSCREEN_ROUTES } from './router'
import { useAppStore } from '../stores/app'

const app = useAppStore()
const router = useRouter()
const route = useRoute()

watch(
  () => app.appState,
  (state) => {
    const current = String(route.name ?? '')
    if (state === 'needs_setup' && current !== 'setup') {
      void router.replace({ name: 'setup' })
      return
    }
    if (state === 'fatal_error' && current !== 'error') {
      void router.replace({ name: 'error' })
      return
    }
    if (state === 'downloading_model' && current !== 'progress') {
      void router.replace({ name: 'progress' })
      return
    }
    if ((state === 'ready' || state === 'degraded') && FULLSCREEN_ROUTES.has(current)) {
      void router.replace({ name: 'chat' })
    }
  },
  { immediate: true },
)

onMounted(() => {
  void app.bootstrap()
})
</script>

<template>
  <RouterView />
  <NavDock v-if="!app.fullscreen" />
  <ToastLayer />
</template>
