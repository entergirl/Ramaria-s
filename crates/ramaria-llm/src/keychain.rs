//! crates/ramaria-llm/src/keychain.rs - OS Keychain 密钥管理封装
//!
//! 设计特点:
//! - 封装 Windows Credential Manager API，提供安全的 API key 存取
//! - 使用 `ramaria/{provider}` 命名约定，与 keychain_poc.rs 保持一致
//! - 所有操作返回 `RamariaResult`，错误分类为 `Privacy`（密钥读写属于隐私域）
//! - 读取不存在的 key 返回 `Ok(None)`，不视为错误
//! - 不支持跨平台（macOS/Linux 待后续扩展）
//!
//! 安全约束:
//! - API key 不进入日志（仅在 debug 级别记录 target_name 的成功/失败状态）
//! - 不缓存密钥内容，每次调用 `get_api_key` 都从 keychain 实时读取
//! - 写入操作完全覆盖（overwrite），不保留旧值
//! - 内存覆盖只服务进程内临时注入（评测专用密钥），不落库、不写配置文件

use ramaria_core::error::{RamariaError, RamariaResult};

// =========================================================
// Keychain 类型
// =========================================================

/// OS Keychain 管理器。
///
/// 职责:
/// - 为 LLM provider 提供安全的 API key 读写接口
/// - 隔离平台相关 API 调用，上层 provider 不直接接触 windows crate
///
/// 平台支持:
/// - Windows: Credential Manager (CredReadW / CredWriteW / CredDeleteW)
/// - macOS/Linux: 待后续实现（当前返回 `Unsupported` 错误）
///
/// 字段约定:
/// - `override_key`: 进程内覆盖条目；命中时优先返回覆盖值，不再读系统凭据。
#[derive(Clone)]
pub struct Keychain {
    override_key: Option<OverrideKey>,
}

/// 单条内存覆盖（服务名 + 密钥明文，仅存在于进程内存）。
#[derive(Clone)]
struct OverrideKey {
    service: String,
    key: String,
}

// 手动实现 Debug：只暴露覆盖是否命中及服务名，绝不输出密钥内容
impl std::fmt::Debug for Keychain {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Keychain")
            .field(
                "override_service",
                &self
                    .override_key
                    .as_ref()
                    .map(|entry| entry.service.as_str()),
            )
            .finish()
    }
}

impl Keychain {
    /// 创建新的 Keychain 实例。
    ///
    /// 返回:
    /// - 无状态的 Keychain 实例，所有方法实时调用系统 API。
    pub fn new() -> Self {
        Self { override_key: None }
    }

    /// 创建带单条内存覆盖的实例。
    ///
    /// 参数:
    /// - `service`: 覆盖命中的服务标识（与 `get_api_key` 入参同一口径，如 `"deepseek"`）；
    /// - `key`: 密钥明文，只留在本实例内存中。
    ///
    /// 返回:
    /// - 只含该条覆盖的实例：读同名服务返回 `key`，其他服务照常走系统 keychain。
    ///
    /// 说明:
    /// - 写入 / 删除接口不受覆盖影响，仍作用于系统凭据；
    /// - Debug 输出不含密钥内容；
    /// - 用于进程内临时注入（如评测专用密钥），不落库、不写配置文件。
    pub fn with_override(service: &str, key: &str) -> Self {
        Self {
            override_key: Some(OverrideKey {
                service: service.to_string(),
                key: key.to_string(),
            }),
        }
    }

    /// 从 keychain 读取 API key。
    ///
    /// 参数:
    /// - `service`: 服务标识，例如 `"deepseek"`、`"openai"`。
    ///   内部会转换为 `ramaria/{service}` 格式作为 Credential Manager target_name。
    ///
    /// 返回:
    /// - `Ok(Some(key))`: 命中内存覆盖，或系统 keychain 中存在该凭据。
    /// - `Ok(None)`: key 不存在（CredReadW 返回 ERROR_NOT_FOUND）。
    /// - `Err(RamariaError::Privacy)`: keychain 读取失败。
    ///
    /// 说明: 命中内存覆盖时不访问系统凭据（服务名一致即返回覆盖值）。
    pub fn get_api_key(&self, service: &str) -> RamariaResult<Option<String>> {
        if let Some(entry) = &self.override_key
            && entry.service == service
        {
            return Ok(Some(entry.key.clone()));
        }
        read_credential(service)
    }

    /// 将 API key 写入 keychain（覆盖已有值）。
    ///
    /// 参数:
    /// - `service`: 服务标识。
    /// - `key`: API key 明文。
    ///
    /// 返回:
    /// - `Ok()`: 写入成功。
    /// - `Err(RamariaError::Privacy)`: 写入失败。
    pub fn set_api_key(&self, service: &str, key: &str) -> RamariaResult<()> {
        write_credential(service, key)
    }

    /// 从 keychain 删除 API key。
    ///
    /// 参数:
    /// - `service`: 服务标识。
    ///
    /// 返回:
    /// - `Ok()`: 删除成功或 key 本就不存在。
    /// - `Err(RamariaError::Privacy)`: 删除失败（权限不足等）。
    pub fn delete_api_key(&self, service: &str) -> RamariaResult<()> {
        delete_credential(service)
    }
}

impl Default for Keychain {
    fn default() -> Self {
        Self::new()
    }
}

// =========================================================
// Windows Credential Manager 实现
// =========================================================

#[cfg(windows)]
mod platform {
    use super::*;
    use std::ptr::null_mut;
    use windows::Win32::Foundation::FILETIME;
    use windows::Win32::Security::Credentials::{
        CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW,
        CredFree, CredReadW, CredWriteW,
    };
    use windows::core::PCWSTR;

    /// 构造 Credential Manager target name: `ramaria/{service}`
    fn target_wide(service: &str) -> Vec<u16> {
        format!("ramaria/{service}\0").encode_utf16().collect()
    }

    /// 写入凭据。
    pub(super) fn write_credential(service: &str, secret: &str) -> RamariaResult<()> {
        let mut target = target_wide(service);
        let blob = secret.as_bytes();

        let cred = CREDENTIALW {
            Flags: CRED_FLAGS(0),
            Type: CRED_TYPE_GENERIC,
            TargetName: windows::core::PWSTR(target.as_mut_ptr()),
            Comment: windows::core::PWSTR(null_mut()),
            LastWritten: FILETIME::default(),
            CredentialBlobSize: blob.len() as u32,
            CredentialBlob: blob.as_ptr() as *mut u8,
            Persist: CRED_PERSIST_LOCAL_MACHINE,
            AttributeCount: 0,
            Attributes: null_mut(),
            TargetAlias: windows::core::PWSTR(null_mut()),
            UserName: windows::core::PWSTR(null_mut()),
        };

        unsafe { CredWriteW(&cred, 0) }.map_err(|e| {
            tracing::error!(%service, error = %e, "写入 keychain 失败");
            RamariaError::privacy(format!(
                "无法将 API key 写入 keychain (service={service}): 系统凭据写入失败"
            ))
        })?;

        tracing::debug!(%service, "API key 已写入 keychain");
        Ok(())
    }

    /// 读取凭据。
    pub(super) fn read_credential(service: &str) -> RamariaResult<Option<String>> {
        let target = target_wide(service);
        let name = PCWSTR::from_raw(target.as_ptr());
        let mut ptr: *mut CREDENTIALW = null_mut();

        unsafe {
            match CredReadW(name, CRED_TYPE_GENERIC, 0, &mut ptr) {
                Ok(()) => {
                    let cred = &*ptr;
                    let bytes = std::slice::from_raw_parts(
                        cred.CredentialBlob,
                        cred.CredentialBlobSize as usize,
                    );
                    let result = String::from_utf8_lossy(bytes).to_string();
                    CredFree(ptr as *const _);
                    tracing::debug!(%service, "API key 已从 keychain 读取");
                    Ok(Some(result))
                }
                Err(e) => {
                    // ERROR_NOT_FOUND = 0x80070490，表示凭据不存在
                    if e.code().0 as u32 == 0x8007_0490_u32 {
                        tracing::debug!(%service, "keychain 中无此凭据");
                        Ok(None)
                    } else {
                        tracing::error!(%service, error = %e, "读取 keychain 失败");
                        Err(RamariaError::privacy(format!(
                            "无法从 keychain 读取 API key (service={service}): 系统凭据读取失败"
                        )))
                    }
                }
            }
        }
    }

    /// 删除凭据。
    pub(super) fn delete_credential(service: &str) -> RamariaResult<()> {
        let target = target_wide(service);
        let name = PCWSTR::from_raw(target.as_ptr());

        unsafe {
            match CredDeleteW(name, CRED_TYPE_GENERIC, 0) {
                Ok(()) => {
                    tracing::debug!(%service, "API key 已从 keychain 删除");
                    Ok(())
                }
                Err(e) => {
                    // 检查是否为 NOT_FOUND
                    // CredDeleteW 对不存在的凭据也会返回错误
                    let hresult = e.code().0 as u32;
                    if hresult == 0x8007_0490_u32 {
                        // 凭据本就不存在，视为成功
                        tracing::debug!(%service, "keychain 中无此凭据（删除视为成功）");
                        Ok(())
                    } else {
                        tracing::error!(%service, error = %e, "删除 keychain 凭据失败");
                        Err(RamariaError::privacy(format!(
                            "无法从 keychain 删除 API key (service={service}): 系统凭据删除失败"
                        )))
                    }
                }
            }
        }
    }
}

// =========================================================
// 非 Windows 平台存根
// =========================================================

#[cfg(not(windows))]
mod platform {
    use super::*;

    pub(super) fn write_credential(service: &str, _secret: &str) -> RamariaResult<()> {
        Err(RamariaError::unsupported(format!(
            "当前平台不支持 keychain 写入 (service={service})。仅 Windows 的 Credential Manager 已实现"
        )))
    }

    pub(super) fn read_credential(service: &str) -> RamariaResult<Option<String>> {
        Err(RamariaError::unsupported(format!(
            "当前平台不支持 keychain 读取 (service={service})。仅 Windows 的 Credential Manager 已实现"
        )))
    }

    pub(super) fn delete_credential(service: &str) -> RamariaResult<()> {
        Err(RamariaError::unsupported(format!(
            "当前平台不支持 keychain 删除 (service={service})。仅 Windows 的 Credential Manager 已实现"
        )))
    }
}

use platform::*;

// =========================================================
// 单元测试
// =========================================================

#[cfg(test)]
mod tests {
    //! 平台分支测试契约:
    //! - 非 Windows: get/set/delete 均返回 Unsupported（无系统实现）。
    //! - Windows: 读取确定不存在的 service 不返回 Ok(Some)（只读操作，不触碰真实凭据）。

    use super::*;

    // （原 keychain_new_creates_instance / keychain_default_creates_instance
    //  无任何断言，Keychain 构造恒成功，已删除）

    /// 非 Windows 平台存根：三个操作都应报 Unsupported，不得静默成功。
    #[cfg(not(windows))]
    #[test]
    fn non_windows_stubs_return_unsupported() {
        let keychain = Keychain::new();

        let read = keychain.get_api_key("ramaria-tests-nonexistent");
        assert!(read.is_err(), "非 Windows 读取应返回 Err");
        assert_eq!(
            read.unwrap_err().category(),
            "unsupported",
            "读取错误应分类为 unsupported"
        );

        let write = keychain.set_api_key("ramaria-tests-nonexistent", "dummy");
        assert!(write.is_err(), "非 Windows 写入应返回 Err");
        assert_eq!(
            write.unwrap_err().category(),
            "unsupported",
            "写入错误应分类为 unsupported"
        );

        let delete = keychain.delete_api_key("ramaria-tests-nonexistent");
        assert!(delete.is_err(), "非 Windows 删除应返回 Err");
        assert_eq!(
            delete.unwrap_err().category(),
            "unsupported",
            "删除错误应分类为 unsupported"
        );
    }

    /// Windows：读取不存在的 service 必须 Ok(None) 或 Err，绝不能 Ok(Some)。
    #[cfg(windows)]
    #[test]
    fn windows_read_missing_service_never_returns_some() {
        let keychain = Keychain::new();
        let result = keychain.get_api_key("ramaria-tests-nonexistent-9f1c7a2b");
        assert!(
            !matches!(&result, Ok(Some(_))),
            "读取确定不存在的 service 不应返回 Ok(Some)，实际: {result:?}"
        );
    }

    /// 内存覆盖优先于系统凭据：同名服务直接返回覆盖值，其他服务不受影响。
    #[test]
    fn with_override_reads_override_before_system() {
        let secret = "sk-eval-override-secret";
        let keychain = Keychain::with_override("deepseek", secret);

        let read = keychain.get_api_key("deepseek").expect("覆盖读取应成功");
        assert_eq!(read.as_deref(), Some(secret), "应返回覆盖值");

        let other = keychain.get_api_key("openai");
        assert!(
            !matches!(&other, Ok(Some(value)) if value == secret),
            "其他服务不得返回覆盖值，实际: {other:?}"
        );
    }

    /// Debug 输出不含密钥内容（只暴露是否覆盖与服务名）。
    #[test]
    fn debug_hides_override_key() {
        let secret = "sk-eval-override-secret";
        let keychain = Keychain::with_override("deepseek", secret);

        let text = format!("{keychain:?}");
        assert!(!text.contains(secret), "Debug 不得包含密钥内容: {text}");
        assert!(text.contains("deepseek"), "Debug 应标出覆盖服务名: {text}");
    }
}
