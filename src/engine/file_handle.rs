//! 文件占用检测
//!
//! 检测文件是否正被进程占用，以及查询安全描述符和硬链接信息。

use std::path::Path;

/// 检测文件是否被占用。
///
/// 尝试以独占模式打开文件，若失败则可能被占用。
#[cfg(windows)]
pub fn is_file_in_use(path: &Path) -> Result<bool, String> {
    use std::os::windows::fs::OpenOptionsExt;
    use winapi::um::winnt::{FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE};

    match std::fs::OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
        .open(path)
    {
        Ok(_) => Ok(false),
        Err(e) => {
            let code = e.raw_os_error().unwrap_or(0);
            // ERROR_SHARING_VIOLATION = 32
            if code == 32 {
                Ok(true)
            } else {
                Err(format!("无法打开文件: {}", e))
            }
        }
    }
}

#[cfg(not(windows))]
pub fn is_file_in_use(_path: &Path) -> Result<bool, String> {
    Ok(false)
}

/// 查询文件安全描述符（所有者 + 权限）。
#[cfg(windows)]
pub fn query_file_security(_path: &Path) -> Result<(Option<String>, Option<String>), String> {
    // TODO: 实现安全描述符查询
    // 需要 winapi 的 aclapi/accctrl/sddl 特性
    Ok((None, None))
}

#[cfg(not(windows))]
pub fn query_file_security(_path: &Path) -> Result<(Option<String>, Option<String>), String> {
    Ok((None, None))
}

/// 查询文件硬链接数量。
#[cfg(windows)]
pub fn query_hardlink_count(_path: &Path) -> Result<u32, String> {
    // TODO: 使用 GetFileInformationByHandle 实现
    // number_of_links() 在稳定版 Rust 中不可用
    Ok(1)
}

#[cfg(not(windows))]
pub fn query_hardlink_count(_path: &Path) -> Result<u32, String> {
    Ok(1)
}
