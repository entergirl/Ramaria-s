/**
 * js/views/settings/sections/basic/memory_injection.js — Ramaria 设置页「记忆注入」目录项
 *
 * 职责:
 * - 线上 LLM 后端是否接收记忆上下文的总开关（online_memory_injection）
 *
 * 设计特点:
 * - 保存经 update_full_config 统一写入口双写；本地 LM Studio 不受开关影响
 *
 * 依赖:
 * - NS.$ / NS.handleUpdateFullResult（shared.js）
 * - RamariaApi / RamariaToast
 */

(function (NS) {
    'use strict';

    var state = NS.state;

 // =========================================================
 // 记忆注入开关区块（v1.4 M6 基础设置）
 // =========================================================

    function _renderMemoryInjectionSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🧠 记忆注入</div>' +
            '<div class="settings-section-desc">控制线上 LLM 后端是否接收记忆上下文（L1/L2/L3 摘要与检索结果）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">' +
                    '<input type="checkbox" id="settings-memory-injection" /> 允许线上后端注入记忆上下文' +
                '</label>' +
                '<div class="settings-form-hint">' +
                    '关闭后线上 provider 的请求不携带记忆上下文（本地 LM Studio 不受影响）。' +
                    '默认开启；开启线上注入前需完成隐私确认。' +
                '</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-memory-injection">保存记忆注入设置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        var btn = NS.$('settings-save-memory-injection');
        if (btn) btn.addEventListener('click', _handleSaveMemoryInjection);
    }

    /**
     * 回显记忆注入开关（online_memory_injection）。
     */
    function _fillMemoryInjectionForm(config) {
        if (!config || !config.backend) return;
        var box = NS.$('settings-memory-injection');
        if (box) box.checked = !!config.backend.online_memory_injection;
    }

    /**
     * 保存记忆注入开关：完整配置更新（统一写入口双写）。
     */
    async function _handleSaveMemoryInjection() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var box = NS.$('settings-memory-injection');
            if (!box) return;

            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            cfg.backend.online_memory_injection = box.checked;

            var result = await RamariaApi.config.updateFull(cfg);
            if (!NS.handleUpdateFullResult(result, '记忆注入设置已保存')) {
                throw new Error('配置双写均失败');
            }
            state.fullConfig = cfg;
        } catch (err) {
            RamariaToast.show('error', '保存失败', err.message || '未知错误');
        }
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.memoryInjection = {
        /** 渲染记忆注入目录项 */
        render: _renderMemoryInjectionSection,
        /** 回显记忆注入开关 */
        fill: _fillMemoryInjectionForm,
    };
})(window.RamariaSettingsNS);
