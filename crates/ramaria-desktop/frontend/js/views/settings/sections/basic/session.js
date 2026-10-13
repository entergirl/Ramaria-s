/**
 * js/views/settings/sections/basic/session.js — Ramaria 设置页「会话」目录项
 *
 * 职责:
 * - 会话空闲自动保存时长（滑动块 5~60 分钟 + 自定义输入）
 *
 * 设计特点:
 * - 保存经 update_full_config 统一写入口双写；后端保存后热更新运行时阈值（立即生效）
 * - 滑动块滑到尽头（60）切换为自定义分钟输入
 *
 * 依赖:
 * - NS.$（shared.js）
 * - RamariaApi / RamariaToast
 */

(function (NS) {
    'use strict';

    var state = NS.state;

// =========================================================
// 会话区块（v1.4 M5：空闲自动保存时长滑动块）
// =========================================================

    function _renderSessionSection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">💬 会话</div>' +
            '<div class="settings-section-desc">设置会话空闲自动保存时长，修改后立即生效（无需重启）。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-session-card';
        card.innerHTML =
            '<div class="settings-form-group">' +
                '<label class="settings-form-label">空闲自动保存时长</label>' +
                '<div class="settings-row">' +
                    '<input class="settings-range" id="settings-idle-slider" type="range" min="5" max="60" step="1" value="10" />' +
                    '<span class="settings-range-value" id="settings-idle-value">10 分钟</span>' +
                '</div>' +
                '<div class="settings-form-hint">' +
                    '对话空闲超过该时长后自动保存并生成记忆摘要。滑动到最右端（60 分钟）可切换为自定义输入。' +
                '</div>' +
            '</div>' +
            '<div class="settings-form-group hidden" id="settings-idle-custom-group">' +
                '<label class="settings-form-label">自定义时长（分钟）</label>' +
                '<input class="settings-form-input" id="settings-idle-custom" type="number" min="1" placeholder="例如 90" />' +
                '<div class="settings-form-hint">自定义时长 ≥ 1 分钟，超出 60 分钟时保存为自定义值。</div>' +
            '</div>' +
            '<div class="settings-save-hint">' +
                '<button class="btn btn-primary btn-sm" id="settings-save-session">保存会话设置</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

        // 绑定事件
        var slider = NS.$('settings-idle-slider');
        var saveBtn = NS.$('settings-save-session');
        if (slider) slider.addEventListener('input', _handleIdleSlider);
        if (saveBtn) saveBtn.addEventListener('click', _handleSaveSession);
    }

    /**
     * 滑动块联动：滑到尽头（60）切换自定义输入，其余显示分钟数。
     */
    function _handleIdleSlider() {
        var slider = NS.$('settings-idle-slider');
        var valueEl = NS.$('settings-idle-value');
        var customGroup = NS.$('settings-idle-custom-group');
        var custom = NS.$('settings-idle-custom');
        if (!slider || !valueEl) return;

        var v = parseInt(slider.value, 10);
        if (v >= 60) {
            valueEl.textContent = '自定义';
            if (customGroup) customGroup.classList.remove('hidden');
            if (custom) custom.focus();
        } else {
            valueEl.textContent = v + ' 分钟';
            if (customGroup) customGroup.classList.add('hidden');
        }
    }

    /**
     * 回显：5~60 落到滑动块；其余（自定义值）滑动块置尽头 + 显示自定义输入。
     */
    function _fillSessionForm(config) {
        if (!config || !config.session) return;
        var minutes = config.session.l1_idle_minutes;
        if (typeof minutes !== 'number') return;

        var slider = NS.$('settings-idle-slider');
        var valueEl = NS.$('settings-idle-value');
        var customGroup = NS.$('settings-idle-custom-group');
        var custom = NS.$('settings-idle-custom');

        if (minutes >= 5 && minutes <= 60) {
            if (slider) slider.value = minutes;
            if (valueEl) valueEl.textContent = minutes + ' 分钟';
            if (customGroup) customGroup.classList.add('hidden');
        } else {
            if (slider) slider.value = 60;
            if (valueEl) valueEl.textContent = '自定义';
            if (customGroup) customGroup.classList.remove('hidden');
            if (custom) custom.value = minutes;
        }
    }

    /**
     * 保存会话设置：读当前值 → 完整配置更新（后端双写 + 热更新阈值）。
     */
    async function _handleSaveSession() {
        try {
            if (!state.fullConfig) {
                throw new Error('配置未加载，请刷新设置页后重试');
            }
            var minutes = _readIdleMinutes();
            if (minutes === null) return;

            // 深拷贝后仅改 session.l1_idle_minutes，其余字段原样回写
            var cfg = JSON.parse(JSON.stringify(state.fullConfig));
            cfg.session.l1_idle_minutes = minutes;

            var result = await RamariaApi.config.updateFull(cfg);
            if (result && result.fileOk === false && result.dbOk === false) {
                throw new Error('配置双写均失败');
            }
            // 后端保存成功后已热更新运行时阈值（与空闲检测线程联动，无需重启）
            state.fullConfig.session.l1_idle_minutes = minutes;
            RamariaToast.show('success', '会话设置已保存（立即生效）');
        } catch (err) {
            RamariaToast.show('error', '保存会话设置失败', err.message || '未知错误');
        }
    }

    /**
     * 读取当前选中的空闲时长（分钟）；非法输入返回 null 并提示。
     */
    function _readIdleMinutes() {
        var customGroup = NS.$('settings-idle-custom-group');
        var custom = NS.$('settings-idle-custom');
        var slider = NS.$('settings-idle-slider');
        if (customGroup && !customGroup.classList.contains('hidden') && custom) {
            var v = parseInt(custom.value, 10);
            if (isNaN(v) || v < 1) {
                RamariaToast.show('warning', '请输入有效的自定义时长（≥ 1 分钟）');
                return null;
            }
            return v;
        }
        if (slider) return parseInt(slider.value, 10);
        return null;
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.session = {
        /** 渲染会话目录项 */
        render: _renderSessionSection,
        /** 回显空闲自动保存时长 */
        fill: _fillSessionForm,
    };
})(window.RamariaSettingsNS);
