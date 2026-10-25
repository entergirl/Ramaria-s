/**
 * src/app/router.ts — 路由表与状态守卫
 *
 * 职责:
 * - 声明 9 个契约视图的路由，并把应用状态机的全屏态强制到对应页面
 *
 * 设计特点:
 * - hash 模式：Tauri 直接加载静态文件，无需服务端回退
 * - 视图全部懒加载，首屏只加载壳与当前视图
 * - 守卫首次导航前等待引导完成，避免用默认状态误判全屏页
 * - 守卫只处理「全屏态强制」；状态变化后的跟随跳转由 App 根组件 watch 承担
 * - 索引状态与模型下载都不参与路由（D-V30-010 / D-V30-044），只影响横幅与设置卡
 */

import { createRouter, createWebHashHistory, type RouteRecordRaw } from 'vue-router'

import { useAppStore } from '../stores/app'

/** 全屏页路由名：这些页面不渲染导航框架，且在就绪后不得停留 */
export const FULLSCREEN_ROUTES = new Set(['setup', 'error'])

/** 契约视图路由表 */
export const routes: RouteRecordRaw[] = [
  { path: '/', redirect: { name: 'chat' } },
  { path: '/chat', name: 'chat', component: () => import('../views/chat/index.vue') },
  { path: '/memory', name: 'memory', component: () => import('../views/memory/index.vue') },
  { path: '/persona', name: 'persona', component: () => import('../views/persona/index.vue') },
  { path: '/rules', name: 'rules', component: () => import('../views/rules/index.vue') },
  { path: '/import', name: 'import', component: () => import('../views/import/index.vue') },
  { path: '/settings', name: 'settings', component: () => import('../views/settings/index.vue') },
  { path: '/debug', name: 'debug', component: () => import('../views/debug/index.vue') },
  { path: '/setup', name: 'setup', component: () => import('../views/setup/index.vue') },
  { path: '/error', name: 'error', component: () => import('../views/error/index.vue') },
  { path: '/:pathMatch(.*)*', name: 'not-found', redirect: { name: 'chat' } },
]

/**
 * 开发期自检路由。
 *
 * 说明:
 * - 生产构建中 `import.meta.env.DEV` 为常量假，整个分支被移除，不产出该页面
 */
const devRoutes: RouteRecordRaw[] = import.meta.env.DEV
  ? [
      {
        path: '/dev/spike',
        name: 'dev-spike',
        component: () => import('../spike/SpikePage.vue'),
      },
      {
        path: '/dev/states',
        name: 'dev-states',
        component: () => import('../views/dev/StatesGallery.vue'),
      },
      {
        path: '/dev/rules',
        name: 'dev-rules',
        component: () => import('../views/dev/RulesPreview.vue'),
      },
    ]
  : []

/** 开发期自检路由：不受应用状态机约束 */
const DEV_ROUTE_NAMES = new Set(['dev-spike', 'dev-states', 'dev-rules'])

/** 创建应用路由（守卫依赖 Pinia 已安装） */
export function createAppRouter() {
  const router = createRouter({
    history: createWebHashHistory(),
    routes: [...routes, ...devRoutes],
  })

  router.beforeEach(async (to) => {
    const app = useAppStore()
    const target = String(to.name ?? '')

    // 开发期自检页不受状态机约束，便于任意状态下查看链路
    if (DEV_ROUTE_NAMES.has(target)) {
      return true
    }

    // 首次导航先等引导完成：默认状态是 needs_setup，提前判定会误跳配置页
    if (!app.booted) {
      await app.bootstrap()
    }

    if (app.appState === 'needs_setup') {
      return target === 'setup' ? true : { name: 'setup' }
    }
    if (app.appState === 'fatal_error') {
      return target === 'error' ? true : { name: 'error' }
    }
    if (FULLSCREEN_ROUTES.has(target)) {
      return { name: 'chat' }
    }
    return true
  })

  return router
}
