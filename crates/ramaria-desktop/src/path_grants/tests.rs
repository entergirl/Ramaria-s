//! crates/ramaria-desktop/src/path_grants/tests.rs - 对话框授权票据单元测试
//!
//! 设计特点:
//! - 覆盖登记去重、上限滚动淘汰与子路径继承三类判定
//! - 纯内存结构，不触碰真实文件系统与用户目录
//! - 断言不写路径原值，失败信息不含绝对路径

use super::*;

#[test]
fn grant_file_then_has_file() {
    let mut grants = DialogGrants::new();
    let file = PathBuf::from("/data/presets/rama.toml");
    grants.grant_file(file.clone());

    assert!(grants.has_file(&file), "已登记文件应命中");
    assert!(
        !grants.has_file(Path::new("/data/presets/other.toml")),
        "未登记文件不应命中"
    );
}

#[test]
fn grant_dir_covers_children() {
    let mut grants = DialogGrants::new();
    let dir = PathBuf::from("/data/exports");
    grants.grant_dir(dir.clone());

    assert!(grants.has_dir(&dir), "已登记目录自身应命中");
    assert!(
        grants.has_dir(&dir.join("child")),
        "子目录应随父目录授权生效"
    );
    assert!(
        grants.has_file(&dir.join("child/chat.json")),
        "目录授权应覆盖其下文件"
    );
    assert!(
        !grants.has_dir(Path::new("/data/other")),
        "兄弟目录不应被授权"
    );
}

#[test]
fn grant_is_deduplicated() {
    let mut grants = DialogGrants::new();
    let dir = PathBuf::from("/data/exports");
    grants.grant_dir(dir.clone());
    grants.grant_dir(dir.clone());

    assert_eq!(grants.dirs().len(), 1, "重复登记应去重");
}

#[test]
fn oldest_entry_is_evicted_at_limit() {
    let mut grants = DialogGrants::new();
    for index in 0..MAX_ENTRIES {
        grants.grant_dir(PathBuf::from(format!("/data/d{index}")));
    }
    // 已达上限后再登记一条：最早登记的 d0 应被淘汰
    grants.grant_dir(PathBuf::from("/data/latest"));

    assert_eq!(grants.dirs().len(), MAX_ENTRIES, "列表长度应等于上限");
    assert!(!grants.has_dir(Path::new("/data/d0")), "最早的票据应被淘汰");
    assert!(
        grants.has_dir(Path::new("/data/latest")),
        "最新登记的票据应保留"
    );
}

#[test]
fn empty_grants_denies_everything() {
    let grants = DialogGrants::new();

    assert!(!grants.has_file(Path::new("/data/a.toml")));
    assert!(!grants.has_dir(Path::new("/data")));
    assert!(grants.dirs().is_empty());
}
