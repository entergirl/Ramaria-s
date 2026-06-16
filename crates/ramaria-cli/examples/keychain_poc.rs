//! POC: 验证 Windows Credential Manager 存取 API key
//!
//! `cargo run --example keychain_poc -p ramaria-cli`

use std::ptr::null_mut;
use windows::Win32::Foundation::{ERROR_NOT_FOUND, FILETIME};
use windows::Win32::Security::Credentials::{
    CRED_FLAGS, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC, CREDENTIALW, CredDeleteW, CredFree,
    CredReadW, CredWriteW,
};
use windows::core::PCWSTR;

fn main() {
    let accounts = ["llm.deepseek.api_key", "llm.openai.api_key"];

    // 0. 清理旧数据
    for a in &accounts {
        delete_credential(a);
    }

    // 1. 写入
    let keys = ["sk-poc-ds-abc123", "sk-poc-oai-xyz789"];
    for (i, a) in accounts.iter().enumerate() {
        write_credential(a, keys[i]);
        println!("wrote : {a}");
    }

    // 2. 读取并验证
    for (i, a) in accounts.iter().enumerate() {
        let val = read_credential(a).expect("read failed");
        assert_eq!(val, keys[i], "mismatch: {a}");
        println!("read  : {a} = {}...{}", &val[..4], &val[val.len() - 4..]);
    }

    // 3. 不存在的 key
    match read_credential("llm.nonexistent.api_key") {
        Err(e) => println!("absent: {e}"),
        Ok(v) => panic!("should not exist: {v}"),
    }

    // 4. 删除
    for a in &accounts {
        delete_credential(a);
        println!("deleted: {a}");
    }

    // 5. 确认已删除
    for a in &accounts {
        assert!(read_credential(a).is_err(), "still exists: {a}");
    }

    println!("\nPASS -- keychain ok");
}

fn target_wide(account: &str) -> Vec<u16> {
    format!("ramaria-poc/{account}\0").encode_utf16().collect()
}

fn write_credential(account: &str, secret: &str) {
    let mut target = target_wide(account);
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
    unsafe { CredWriteW(&cred, 0) }.expect("CredWriteW failed");
}

fn read_credential(account: &str) -> Result<String, String> {
    let target = target_wide(account);
    let name = PCWSTR::from_raw(target.as_ptr());
    let mut ptr: *mut CREDENTIALW = null_mut();
    unsafe {
        CredReadW(name, CRED_TYPE_GENERIC, 0, &mut ptr).map_err(|e| format!("CredReadW: {e}"))?;
        let cred = &*ptr;
        let bytes =
            std::slice::from_raw_parts(cred.CredentialBlob, cred.CredentialBlobSize as usize);
        let result = String::from_utf8_lossy(bytes).to_string();
        CredFree(ptr as *const _);
        Ok(result)
    }
}

fn delete_credential(account: &str) {
    let target = target_wide(account);
    let name = PCWSTR::from_raw(target.as_ptr());
    match unsafe { CredDeleteW(name, CRED_TYPE_GENERIC, 0) } {
        Ok(()) => {}
        Err(e) if e.code() == ERROR_NOT_FOUND.to_hresult() => {}
        Err(e) => panic!("CredDeleteW: {e}"),
    }
}
