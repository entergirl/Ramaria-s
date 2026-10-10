//! crates/ramaria-channel/src/config/tests.rs - 通道运行期装配参数单元测试
//!
//! 设计特点:
//! - 覆盖组装矩阵：总开关 / 平台开关 / 地址与凭据缺失的降级组合
//! - 覆盖令牌与默认人格的空白归一
//! - 覆盖 is_empty 与主动开关透传

use super::*;

/// 构造「总开关开启」的基座配置（平台开关默认关）。
fn base_config() -> ChannelConfig {
    ChannelConfig {
        enabled: true,
        ..ChannelConfig::default()
    }
}

/// 总开关关闭时两平台均不装配（不触发地址 / 凭据检查）。
#[test]
fn disabled_master_switch_assembles_nothing() {
    let core = ChannelConfig {
        enabled: false,
        qq_enabled: true,
        qq_ws_url: "ws://127.0.0.1:3001".to_string(),
        telegram_enabled: true,
        ..ChannelConfig::default()
    };

    let runtime =
        ChannelRuntimeConfig::from_config(&core, Some("t".to_string()), Some("b".to_string()));
    assert!(runtime.qq.is_none(), "总开关关闭时 QQ 不应装配");
    assert!(runtime.telegram.is_none(), "总开关关闭时 Telegram 不应装配");
    assert!(runtime.is_empty());
}

/// QQ 启用且地址有效：地址取 trim 后值；令牌有值保留、无值 None。
#[test]
fn qq_assembled_with_valid_url_and_token() {
    let core = ChannelConfig {
        qq_enabled: true,
        qq_ws_url: "  ws://127.0.0.1:3001  ".to_string(),
        ..base_config()
    };

    let runtime = ChannelRuntimeConfig::from_config(&core, Some("secret-token".to_string()), None);
    assert!(!runtime.is_empty());
    let qq = runtime.qq.expect("地址有效时 QQ 应装配");
    assert_eq!(qq.ws_url, "ws://127.0.0.1:3001", "地址应取 trim 后值");
    assert_eq!(qq.access_token.as_deref(), Some("secret-token"));

    let runtime = ChannelRuntimeConfig::from_config(&core, None, None);
    let qq = runtime.qq.expect("无令牌时 QQ 仍应装配");
    assert_eq!(qq.access_token, None, "未配置令牌应为 None");
}

/// QQ 令牌空白归一为 None；有值取 trim 后值。
#[test]
fn qq_token_normalizes_blank_to_none() {
    let core = ChannelConfig {
        qq_enabled: true,
        ..base_config()
    };

    for blank in ["", "   ", "\t\n"] {
        let runtime = ChannelRuntimeConfig::from_config(&core, Some(blank.to_string()), None);
        let qq = runtime.qq.expect("QQ 应装配");
        assert_eq!(qq.access_token, None, "空白令牌 {blank:?} 应归一为 None");
    }

    let runtime =
        ChannelRuntimeConfig::from_config(&core, Some("  padded-token  ".to_string()), None);
    assert_eq!(
        runtime.qq.expect("QQ 应装配").access_token.as_deref(),
        Some("padded-token"),
        "令牌应取 trim 后值"
    );
}

/// QQ 启用但地址空白：跳过装配（降级不阻塞）。
#[test]
fn qq_skipped_when_url_blank() {
    for blank in ["", "   "] {
        let core = ChannelConfig {
            qq_enabled: true,
            qq_ws_url: blank.to_string(),
            ..base_config()
        };
        let runtime = ChannelRuntimeConfig::from_config(&core, Some("t".to_string()), None);
        assert!(runtime.qq.is_none(), "地址空白 {blank:?} 时应跳过装配");
    }
}

/// QQ 平台开关关闭时不装配（即使地址与令牌齐备）。
#[test]
fn qq_skipped_when_platform_disabled() {
    let core = ChannelConfig {
        qq_enabled: false,
        qq_ws_url: "ws://127.0.0.1:3001".to_string(),
        ..base_config()
    };
    let runtime = ChannelRuntimeConfig::from_config(&core, Some("t".to_string()), None);
    assert!(runtime.qq.is_none());
}

/// Telegram 启用且令牌有效（trim 后非空）才装配；缺失 / 空白跳过。
#[test]
fn telegram_requires_non_blank_token() {
    let core = ChannelConfig {
        telegram_enabled: true,
        ..base_config()
    };

    let runtime =
        ChannelRuntimeConfig::from_config(&core, None, Some("  bot-secret  ".to_string()));
    assert_eq!(
        runtime.telegram.expect("令牌有效时应装配").bot_token,
        "bot-secret",
        "令牌应取 trim 后值"
    );

    for missing in [None, Some(String::new()), Some("   ".to_string())] {
        let runtime = ChannelRuntimeConfig::from_config(&core, None, missing.clone());
        assert!(runtime.telegram.is_none(), "令牌 {missing:?} 时应跳过装配");
    }
}

/// 默认人格：空白归一为兜底常量；有值取 trim 后值。
#[test]
fn default_persona_uid_falls_back_when_blank() {
    let core = ChannelConfig {
        default_persona_uid: "   ".to_string(),
        ..ChannelConfig::default()
    };
    let runtime = ChannelRuntimeConfig::from_config(&core, None, None);
    assert_eq!(
        runtime.default_persona_uid, FALLBACK_DEFAULT_PERSONA_UID,
        "空白默认人格应回落兜底常量"
    );

    let core = ChannelConfig {
        default_persona_uid: "  char-0009  ".to_string(),
        ..ChannelConfig::default()
    };
    let runtime = ChannelRuntimeConfig::from_config(&core, None, None);
    assert_eq!(runtime.default_persona_uid, "char-0009");
}

/// 主动投递总开关与平台装配独立透传。
#[test]
fn proactive_flag_passed_through_independently() {
    let core = ChannelConfig {
        proactive_enabled: true,
        ..ChannelConfig::default()
    };
    let runtime = ChannelRuntimeConfig::from_config(&core, None, None);
    assert!(runtime.proactive_enabled, "主动开关应透传");
    assert!(runtime.is_empty(), "平台未启用时仍为空装配");
}
