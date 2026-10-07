//! crates/ramaria-llm/src/provider/macros.rs - 在线 Provider 实现宏
//!
//! 设计特点:
//! - `impl_online_provider_constructors!`: 生成 new / with_cache / resolve_api_key
//! - `impl_online_provider!`: 生成完整 `LlmProvider` trait 实现
//! - 消除 DeepSeek/OpenAI 间约 97% 的重复实现（仅 service/display 名不同）
//! - 宏展开发生在 crate 内，通过 `$crate::provider::*` 引用共享基础设施

// =========================================================
// 在线 Provider 实现宏（消除 DeepSeek/OpenAI 间的 ~97% 重复）
// =========================================================

/// 为在线 LLM provider 生成构造器（new / with_cache / resolve_api_key）。
///
/// 与 `impl_online_provider!` 配套：该宏生成 trait 实现，本宏生成固有构造方法，
/// 消除 DeepSeek/OpenAI 构造逻辑的重复（约 60 行）。
///
/// 参数:
/// - `$struct_name`: provider 结构体名
/// - `$service`: keychain service name（如 `"deepseek"`）
/// - `$display`: 人类可读名称（如 `"DeepSeek"`）
#[macro_export]
macro_rules! impl_online_provider_constructors {
    ($struct_name:ident, $service:literal, $display:literal) => {
        impl $struct_name {
            /// 创建 $display Provider。
            ///
            /// 参数:
            /// - `config`: 后端配置（含默认 capability）。
            /// - `keychain`: OS keychain 实例，用于读取 API key。
            ///
            /// 返回:
            /// - 成功时返回 provider 实例。
            /// - API key 不存在或读取失败不在此处报错（延迟到 `chat`/`validate` 时检查）。
            pub fn new(
                config: ramaria_core::types::BackendConfig,
                keychain: std::sync::Arc<$crate::keychain::Keychain>,
            ) -> ramaria_core::error::RamariaResult<Self> {
                // keychain 读取失败在此降级为未配置（不阻断构造），调用时会提示用户。
                let (api_key, key_status) = $crate::provider::resolve_constructor_key(
                    keychain.get_api_key($service),
                    $service,
                );

                let base = $crate::provider::ProviderBase::new(config, api_key)?;

                tracing::info!(
                    key_status,
                    base_url = %base.transport().base_url(),
                    concat!($display, "Provider 已创建")
                );

                Ok(Self { base, keychain })
            }

            /// 接入 LLM 响应精确缓存（v1.5 C 三层生成缓存）。
            ///
            /// 参数:
            /// - `cache`: 缓存实现（通常为 `ramaria_storage::SqliteLlmCache`）。
            ///
            /// 说明:
            /// - 缓存查询/写入失败均静默降级走真实 LLM，不阻塞主流程。
            pub fn with_cache(
                self,
                cache: std::sync::Arc<dyn ramaria_core::traits::LlmResponseCache>,
            ) -> Self {
                Self {
                    base: self.base.with_cache(cache),
                    keychain: self.keychain,
                }
            }

            /// 从 keychain 获取 API key。
            fn resolve_api_key(&self) -> ramaria_core::error::RamariaResult<Option<String>> {
                self.keychain.get_api_key($service)
            }
        }
    };
}

/// 为在线 LLM provider 生成完整的 `LlmProvider` trait 实现。
///
/// DeepSeek 和 OpenAI 的实现逻辑完全相同，差异仅在于字符串常量。
/// 此宏消除 ~150 行重复代码。
///
/// 用法:
/// 宏调用需 provider 类型已定义并实现所需字段，示例仅示意，不参与编译。
/// ```ignore
/// impl_online_provider!(DeepSeekProvider, "deepseek", "DeepSeek");
/// impl_online_provider!(OpenAIProvider, "openai", "OpenAI");
/// ```
///
/// 参数:
/// - `$struct_name`: provider 结构体名
/// - `$service`: keychain service name（如 `"deepseek"`）
/// - `$display`: 人类可读名称（如 `"DeepSeek"`）
#[macro_export]
macro_rules! impl_online_provider {
    ($struct_name:ident, $service:literal, $display:literal) => {
        #[async_trait::async_trait]
        impl ramaria_core::traits::LlmProvider for $struct_name {
            async fn chat(
                &self,
                request: &ramaria_core::traits::ChatRequest,
            ) -> ramaria_core::error::RamariaResult<String> {
                let api_key = self.resolve_api_key()?;
                if api_key.is_none() {
                    return Err(ramaria_core::error::RamariaError::privacy(format!(
                        concat!(
                            $display,
                            " API key 未配置。请在设置中配置 ",
                            $display,
                            " API key 后再试。"
                        )
                    )));
                }
                self.base.set_api_key(api_key);
                self.base.chat(request).await
            }

            async fn chat_vision(
                &self,
                request: &ramaria_core::traits::ChatRequest,
                image_data_uris: &[String],
            ) -> ramaria_core::error::RamariaResult<String> {
                let api_key = self.resolve_api_key()?;
                if api_key.is_none() {
                    return Err(ramaria_core::error::RamariaError::privacy(format!(
                        concat!(
                            $display,
                            " API key 未配置。请在设置中配置 ",
                            $display,
                            " API key 后再试。"
                        )
                    )));
                }
                self.base.set_api_key(api_key);
                self.base.chat_with_images(request, image_data_uris).await
            }

            async fn chat_stream(
                &self,
                request: &ramaria_core::traits::ChatRequest,
            ) -> ramaria_core::error::RamariaResult<
                std::pin::Pin<
                    Box<
                        dyn futures::Stream<
                                Item = ramaria_core::error::RamariaResult<
                                    ramaria_core::traits::StreamDelta,
                                >,
                            > + Send,
                    >,
                >,
            > {
                let api_key = self.resolve_api_key()?;
                if api_key.is_none() {
                    return Err(ramaria_core::error::RamariaError::privacy(format!(
                        concat!(
                            $display,
                            " API key 未配置。请在设置中配置 ",
                            $display,
                            " API key 后再试。"
                        )
                    )));
                }
                self.base.set_api_key(api_key);
                self.base.chat_stream(request).await
            }

            fn capability(&self) -> &ramaria_core::types::ModelCapability {
                self.base.capability()
            }

            fn config(&self) -> &ramaria_core::types::BackendConfig {
                self.base.backend_config()
            }

            async fn validate(&self) -> ramaria_core::error::RamariaResult<()> {
                let api_key = self.resolve_api_key()?;
                if api_key.is_none() {
                    return Err(ramaria_core::error::RamariaError::privacy(format!(
                        concat!(
                            $display,
                            " API key 未配置。请先在 keychain 中设置 ",
                            $display,
                            " API key。"
                        )
                    )));
                }
                self.base.set_api_key(api_key);
                self.base.validate().await
            }

            async fn health_check(&self) -> ramaria_core::error::RamariaResult<()> {
                let api_key = self.resolve_api_key()?;
                if api_key.is_none() {
                    return Err(ramaria_core::error::RamariaError::privacy(format!(
                        concat!(
                            $display,
                            " API key 未配置。请先在 keychain 中设置 ",
                            $display,
                            " API key。"
                        )
                    )));
                }
                self.base.set_api_key(api_key);
                self.base.health_check().await
            }

            fn name(&self) -> &'static str {
                $display
            }
        }
    };
}
