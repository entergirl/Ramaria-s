/**
 * js/views/settings/sections/basic/privacy.js — Ramaria 设置页「隐私设置」目录项
 *
 * 职责:
 * - 隐私确认状态查看（状态 / 持久化标记 / 确认按钮）
 *
 * 设计特点:
 * - 确认经 chat 命令面写入；进入目录项时重新查询状态
 * - 查询失败静默降级为 "-"，不阻塞页面其余区块
 *
 * 依赖:
 * - NS.$（shared.js）
 * - RamariaApi / RamariaToast
 */

(function (NS) {
    'use strict';

    var state = NS.state;

 // =========================================================
 // 隐私设置区块
 // =========================================================

    function _renderPrivacySection(parent) {
        var section = document.createElement('div');
        section.className = 'settings-section';
        section.innerHTML =
            '<div class="settings-section-title">🔒 隐私设置</div>' +
            '<div class="settings-section-desc">管理线上服务的隐私确认和数据控制。' +
            '使用线上模型时，对话内容、记忆上下文与导入图片的理解请求也会发送到 API 服务器。</div>';

        var card = document.createElement('div');
        card.className = 'settings-card';
        card.id = 'settings-privacy-card';
        card.innerHTML =
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">隐私确认状态</div>' +
                    '<div class="settings-row-meta">当前后端对线上服务的确认状态</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-privacy-status">加载中...</span>' +
            '</div>' +
            '<div class="settings-row">' +
                '<div>' +
                    '<div class="settings-row-label">持久化确认</div>' +
                    '<div class="settings-row-meta">勾选后跨重启不再提示</div>' +
                '</div>' +
                '<span class="settings-row-value" id="settings-privacy-persistent">-</span>' +
            '</div>' +
            '<div class="settings-actions">' +
                '<button class="btn btn-secondary btn-sm" id="settings-confirm-privacy">确认隐私</button>' +
            '</div>';

        section.appendChild(card);
        parent.appendChild(section);

 // 绑定
        var confirmBtn = NS.$('settings-confirm-privacy');
        if (confirmBtn) {
            confirmBtn.addEventListener('click', async function () {
                try {
                    await RamariaApi.chat.confirmPrivacy(true);
                    RamariaToast.show('success', '隐私确认已保存');
                    await _refreshPrivacyStatus();
                } catch (err) {
                    RamariaToast.show('error', '确认失败', err.message || '');
                }
            });
        }
    }

    function _fillPrivacyInfo(status) {
        var statusEl = NS.$('settings-privacy-status');
        var persistentEl = NS.$('settings-privacy-persistent');

        if (statusEl) {
            if (!status) {
                statusEl.textContent = '-';
            } else if (status.status === 'NotNeeded') {
                statusEl.textContent = '无需确认（本地）';
            } else if (status.status === 'Confirmed') {
                statusEl.textContent = '✓ 已确认';
            } else {
                statusEl.textContent = '⚠ 需要确认';
            }
        }

        if (persistentEl) {
            persistentEl.textContent = (status && status.persistent) ? '✓ 是' : '✗ 否';
        }

        state.privacyStatus = status;
    }

    async function _refreshPrivacyStatus() {
        try {
            var status = await RamariaApi.chat.checkPrivacy();
            _fillPrivacyInfo(status);
        } catch (err) {
            console.error('[SettingsView] 查询隐私状态失败:', err);
        }
    }

 // =========================================================
 // 挂载面
 // =========================================================

    NS.parts.privacy = {
        /** 渲染隐私设置目录项 */
        render: _renderPrivacySection,
        /** 重新查询并回显隐私状态 */
        refreshStatus: _refreshPrivacyStatus,
    };
})(window.RamariaSettingsNS);
