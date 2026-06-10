//! rust/crates/ramaria-llm/src/keychain.rs - OS Keychain 密钥管理封装
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
#[derive(Debug, Clone)]
pub struct Keychain;

impl Keychain {
    /// 创建新的 Keychain 实例。
    ///
    /// 返回:
    /// - 无状态的 Keychain 实例，所有方法实时调用系统 API。
    pub fn new() -> Self {
        Self
    }

    /// 从 keychain 读取 API key。
    ///
    /// 参数:
    /// - `service`: 服务标识，例如 `"deepseek"`、`"openai"`。
    ///   内部会转换为 `ramaria/{service}` 格式作为 Credential Manager target_name。
    ///
    /// 返回:
    /// - `Ok(Some(key))`: key 存在且读取成功。
    /// - `Ok(None)`: key 不存在（CredReadW 返回 ERROR_NOT_FOUND）。
    /// - `Err(RamariaError::Privacy)`: keychain 读取失败。
    pub fn get_api_key(&self, service: &str) -> RamariaResult<Option<String>> {
        read_credential(service)
    }

    /// 将 API key 写入 keychain（覆盖已有值）。
    ///
    /// 参数:
    /// - `service`: 服务标识。
    /// - `key`: API key 明文。
    ///
    /// 返回:
    /// - `Ok(())`: 写入成功。
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
    /// - `Ok(())`: 删除成功或 key 本就不存在。
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
    use super::*;

    #[test]
    fn keychain_new_creates_instance() {
        let kc = Keychain::new();
        // 仅验证构造成功
        let _ = kc;
    }

    #[test]
    fn keychain_default_creates_instance() {
        let kc = Keychain::default();
        let _ = kc;
    }
}
