/**
 * js/utils/format.js — Ramaria 格式化工具
 *
 * 职责:
 * - 时间格式化：相对时间（"3分钟前"/"2小时前"/"昨天 14:30"）和绝对时间
 * - 数字格式化：千分位、截断（1.2k）、百分比、文件大小
 * - 持续时间格式化（秒 → "2分30秒"）
 * - 全部中文友好，适合 UI 展示场景
 *
 * 设计特点:
 * - 通过 RamariaFormat 全局单例访问，纯函数无状态
 * - 时间处理基于 Unix 毫秒时间戳（与 Rust core 类型系统一致）
 * - 相对时间：60秒内→"刚刚"，60分内→"X分钟前"，24时内→"X小时前"
 *   48时内→"昨天 HH:MM"，7天内→"X天前"，其他→绝对日期
 * - 数字千分位使用中文习惯（逗号分隔："1,234"）
 * - 支持负数、小数、NaN 安全回退
 * - 零外部依赖，不依赖 Intl API（兼容旧浏览器）
 *
 * 用法:
 *   RamariaFormat.relativeTime(1718123456000);       // "3分钟前"
 *   RamariaFormat.absoluteTime(1718123456000);       // "2024-06-12 14:30"
 *   RamariaFormat.number(1234567);                   // "1,234,567"
 *   RamariaFormat.compactNumber(12345);              // "1.2万"
 *   RamariaFormat.fileSize(1536000);                 // "1.5 MB"
 *   RamariaFormat.duration(125);                     // "2分5秒"
 *
 * 依赖: 无
 */

var RamariaFormat = (function () {
    'use strict';

    // =========================================================
    // 常量
    // =========================================================

    var SECOND_MS = 1000;
    var MINUTE_MS = 60 * SECOND_MS;
    var HOUR_MS   = 60 * MINUTE_MS;
    var DAY_MS    = 24 * HOUR_MS;
    var WEEK_MS   = 7 * DAY_MS;

    // =========================================================
    // 内部辅助
    // =========================================================

    /**
     * 补零到两位数。
     */
    function _pad2(n) {
        return (n < 10 ? '0' : '') + n;
    }

    /**
     * 安全解析时间戳，返回 Date 对象或 null。
     *
     * 参数:
     * - `ts`: 毫秒级时间戳（数字）或 ISO 字符串
     */
    function _parseDate(ts) {
        if (ts === null || ts === undefined || ts === '') return null;

        var date;
        if (typeof ts === 'number') {
            // Unix 毫秒时间戳
            if (ts <= 0) return null;
            date = new Date(ts);
        } else if (typeof ts === 'string') {
            date = new Date(ts);
        } else {
            return null;
        }

        if (isNaN(date.getTime())) return null;
        return date;
    }

    /**
     * 获取本地化的今天零点时间戳。
     */
    function _todayStart() {
        var now = new Date();
        return new Date(now.getFullYear(), now.getMonth(), now.getDate()).getTime();
    }

    // =========================================================
    // 时间格式化
    // =========================================================

    /**
     * 相对时间（中文友好）。
     *
     * 参数:
     * - `ts`: Unix 毫秒时间戳，或 ISO 字符串
     * - `now`: 可选的参考时间（毫秒时间戳），默认当前时间
     *
     * 返回:
     * - 格式化字符串，如 "刚刚" / "3分钟前" / "2小时前" / "昨天 14:30" / "3天前" / "2024-06-12"
     *
     * 说明:
     * - 60 秒内 → "刚刚"
     * - 60 分钟内 → "X分钟前"
     * - 24 小时内 → "X小时前"
     * - 48 小时内，且过了一天 → "昨天 HH:MM"
     * - 7 天内 → "X天前"
     * - 同年 → "MM-DD HH:MM"
     * - 跨年 → "YYYY-MM-DD HH:MM"
     */
    function relativeTime(ts, now) {
        var date = _parseDate(ts);
        if (!date) return '未知时间';

        var nowMs = (typeof now === 'number' && now > 0) ? now : Date.now();
        var diffMs = nowMs - date.getTime();

        // 未来时间
        if (diffMs < 0) {
            return absoluteTime(ts);
        }

        // 刚刚（60秒内）
        if (diffMs < 60 * SECOND_MS) {
            return '刚刚';
        }

        // X分钟前（60分钟内）
        if (diffMs < HOUR_MS) {
            var minutes = Math.floor(diffMs / MINUTE_MS);
            return minutes + '分钟前';
        }

        // X小时前（24小时内）
        if (diffMs < DAY_MS) {
            var hours = Math.floor(diffMs / HOUR_MS);
            return hours + '小时前';
        }

        // 昨天（48小时内且跨天）
        var todayStart = _todayStart();
        var dateStart = new Date(date.getFullYear(), date.getMonth(), date.getDate()).getTime();
        if (dateStart === todayStart - DAY_MS) {
            return '昨天 ' + _pad2(date.getHours()) + ':' + _pad2(date.getMinutes());
        }

        // X天前（7天内）
        if (diffMs < WEEK_MS) {
            var days = Math.floor(diffMs / DAY_MS);
            return days + '天前';
        }

        // 同年
        var nowDate = new Date(nowMs);
        if (date.getFullYear() === nowDate.getFullYear()) {
            return (date.getMonth() + 1) + '月' + date.getDate() + '日 ' +
                   _pad2(date.getHours()) + ':' + _pad2(date.getMinutes());
        }

        // 跨年
        return date.getFullYear() + '年' +
               (date.getMonth() + 1) + '月' +
               date.getDate() + '日';
    }

    /**
     * 绝对时间（标准格式）。
     *
     * 参数:
     * - `ts`: Unix 毫秒时间戳，或 ISO 字符串
     * - `format`: 'datetime' | 'date' | 'time'（默认 'datetime'）
     *
     * 返回:
     * - datetime: "2024-06-12 14:30:05"
     * - date: "2024-06-12"
     * - time: "14:30:05"
     */
    function absoluteTime(ts, format) {
        var date = _parseDate(ts);
        if (!date) return '未知时间';

        format = format || 'datetime';

        var y = date.getFullYear();
        var m = _pad2(date.getMonth() + 1);
        var d = _pad2(date.getDate());
        var h = _pad2(date.getHours());
        var min = _pad2(date.getMinutes());
        var s = _pad2(date.getSeconds());

        switch (format) {
            case 'date':
                return y + '-' + m + '-' + d;
            case 'time':
                return h + ':' + min + ':' + s;
            case 'datetime':
            default:
                return y + '-' + m + '-' + d + ' ' + h + ':' + min + ':' + s;
        }
    }

    /**
     * 智能时间格式化：短时间用相对时间，超过阈值用绝对时间。
     *
     * 参数:
     * - `ts`: Unix 毫秒时间戳
     * - `thresholdDays`: 阈值天数（默认 7 天），超过后用绝对时间
     *
     * 返回:
     * - 格式化字符串
     */
    function smartTime(ts, thresholdDays) {
        var date = _parseDate(ts);
        if (!date) return '未知时间';

        var thresholdMs = (thresholdDays || 7) * DAY_MS;
        var diffMs = Date.now() - date.getTime();

        if (diffMs >= 0 && diffMs < thresholdMs) {
            return relativeTime(ts);
        }

        return absoluteTime(ts, 'date');
    }

    // =========================================================
    // 持续时间格式化
    // =========================================================

    /**
     * 格式化持续时间（秒）。
     *
     * 参数:
     * - `seconds`: 秒数（数字）
     *
     * 返回:
     * - 格式化字符串，如 "2分30秒" / "1小时15分" / "45秒" / "0.5秒"
     *
     * 说明:
     * - < 1秒 → "X.X秒"
     * - < 1分钟 → "X秒"
     * - < 1小时 → "X分Y秒"
     * - >= 1小时 → "X小时Y分"
     */
    function duration(seconds) {
        if (typeof seconds !== 'number' || isNaN(seconds) || seconds < 0) {
            return '0秒';
        }

        if (seconds < 1) {
            var tenths = Math.round(seconds * 10) / 10;
            return tenths + '秒';
        }

        if (seconds < 60) {
            return Math.floor(seconds) + '秒';
        }

        if (seconds < 3600) {
            var mins = Math.floor(seconds / 60);
            var secs = Math.floor(seconds % 60);
            if (secs === 0) return mins + '分钟';
            return mins + '分' + secs + '秒';
        }

        var hours = Math.floor(seconds / 3600);
        var remainMins = Math.floor((seconds % 3600) / 60);
        if (remainMins === 0) return hours + '小时';
        return hours + '小时' + remainMins + '分';
    }

    // =========================================================
    // 数字格式化
    // =========================================================

    /**
     * 千分位格式化。
     *
     * 参数:
     * - `num`: 数字
     *
     * 返回:
     * - 格式化字符串，如 "1,234,567"
     */
    function number(num) {
        if (typeof num !== 'number' || isNaN(num)) return '0';

        var parts = num.toString().split('.');
        var intPart = parts[0];
        var decPart = parts.length > 1 ? '.' + parts[1] : '';

        // 处理负号
        var sign = '';
        if (intPart.charAt(0) === '-') {
            sign = '-';
            intPart = intPart.substring(1);
        }

        // 添加千分位逗号
        var result = '';
        while (intPart.length > 3) {
            result = ',' + intPart.slice(-3) + result;
            intPart = intPart.slice(0, -3);
        }
        result = intPart + result;

        return sign + result + decPart;
    }

    /**
     * 紧凑数字（中文单位）。
     *
     * 参数:
     * - `num`: 数字
     *
     * 返回:
     * - 如 "123" / "1,234" / "1.2万" / "345.6万" / "1.2亿"
     *
     * 说明:
     * - < 10,000 → 千分位格式
     * - < 100,000,000 → "X.X万"
     * - >= 100,000,000 → "X.X亿"
     */
    function compactNumber(num) {
        if (typeof num !== 'number' || isNaN(num)) return '0';

        var absNum = Math.abs(num);
        var sign = num < 0 ? '-' : '';

        if (absNum < 10000) {
            return sign + number(Math.round(absNum));
        }

        if (absNum < 100000000) {
            var wan = absNum / 10000;
            return sign + (wan >= 100 ? Math.round(wan).toString() : wan.toFixed(1)) + '万';
        }

        var yi = absNum / 100000000;
        return sign + (yi >= 100 ? Math.round(yi).toString() : yi.toFixed(1)) + '亿';
    }

    /**
     * 百分比格式化。
     *
     * 参数:
     * - `ratio`: 0-1 的比值
     * - `decimals`: 小数位数（默认 1）
     *
     * 返回:
     * - 如 "42.5%" / "100%"
     */
    function percent(ratio, decimals) {
        if (typeof ratio !== 'number' || isNaN(ratio)) return '0%';

        var d = (typeof decimals === 'number' && decimals >= 0) ? decimals : 1;
        var pct = ratio * 100;

        if (pct >= 99.95) return '100%';
        if (pct <= 0.05) return '0%';

        return pct.toFixed(d) + '%';
    }

    /**
     * 文件大小格式化。
     *
     * 参数:
     * - `bytes`: 字节数
     * - `decimals`: 小数位数（默认 1）
     *
     * 返回:
     * - 如 "1.5 KB" / "23.4 MB" / "1.2 GB"
     */
    function fileSize(bytes, decimals) {
        if (typeof bytes !== 'number' || isNaN(bytes) || bytes < 0) return '0 B';

        var d = (typeof decimals === 'number' && decimals >= 0) ? decimals : 1;

        if (bytes < 1024) {
            return bytes + ' B';
        }

        var kb = bytes / 1024;
        if (kb < 1024) {
            return kb.toFixed(d) + ' KB';
        }

        var mb = kb / 1024;
        if (mb < 1024) {
            return mb.toFixed(d) + ' MB';
        }

        var gb = mb / 1024;
        return gb.toFixed(d) + ' GB';
    }

    /**
     * 数字截断显示。
     *
     * 参数:
     * - `num`: 数字
     * - `maxLen`: 最大总长度（含符号和小数点，默认 6）
     *
     * 返回:
     * - 字符串
     */
    function truncate(num, maxLen) {
        if (typeof num !== 'number' || isNaN(num)) return '0';

        maxLen = maxLen || 6;
        var str = num.toString();

        if (str.length <= maxLen) return str;

        // 对于大整数，尝试用紧凑格式
        if (Number.isInteger(num)) {
            return compactNumber(num);
        }

        // 对于小数，截断到 maxLen
        return str.substring(0, maxLen);
    }

    // =========================================================
    // 导出
    // =========================================================

    return {
        relativeTime: relativeTime,
        absoluteTime: absoluteTime,
        smartTime: smartTime,
        duration: duration,
        number: number,
        compactNumber: compactNumber,
        percent: percent,
        fileSize: fileSize,
        truncate: truncate
    };
})();

// 防止意外覆盖
Object.defineProperty(window, 'RamariaFormat', {
    value: RamariaFormat,
    writable: false,
    configurable: false,
});
