//! 注册表读取封装
//!
//! 查询注册表中与应用安装、文件关联等相关的键值。
//!
//! 核心功能：
//! 1. 枚举已安装应用（HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall）
//! 2. 判断文件是否属于已安装应用
//! 3. 查询应用安装路径

use std::path::Path;
use crate::engine::dynamic_perception::AppInfo;

// ============================================================
// 已安装应用枚举
// ============================================================

/// 枚举所有已安装应用。
///
/// 遍历注册表键：
/// - `HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall`（64 位应用）
/// - `HKLM\SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall`（32 位应用）
#[cfg(windows)]
pub fn enumerate_installed_apps() -> Result<Vec<AppInfo>, String> {
    use winapi::um::winreg::{RegOpenKeyExW, RegEnumKeyExW, RegCloseKey, RegQueryValueExW, HKEY_LOCAL_MACHINE};
    use winapi::um::winnt::{KEY_READ, REG_SZ};
    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::shared::minwindef::{DWORD, HKEY};
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let mut apps = Vec::new();

    // 64 位应用注册表路径
    let paths = [
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall",
        r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall",
    ];

    for subkey in &paths {
        let wide_subkey: Vec<u16> = subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let mut hkey: HKEY = std::ptr::null_mut();

        let result = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                wide_subkey.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            )
        };

        if result != 0 {
            continue;
        }

        // 枚举子键
        let mut index: DWORD = 0;
        loop {
            let mut name_buf = vec![0u16; 256];
            let mut name_len: DWORD = name_buf.len() as DWORD;

            let result = unsafe {
                RegEnumKeyExW(
                    hkey,
                    index,
                    name_buf.as_mut_ptr(),
                    &mut name_len,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };

            if result != 0 {
                break;
            }

            let name = String::from_utf16_lossy(&name_buf[..name_len as usize]);

            // 读取应用信息
            if let Some(app) = read_app_info_from_subkey(hkey, &name) {
                // 只保留有安装路径的应用
                if app.install_path.is_some() || !app.name.is_empty() {
                    apps.push(app);
                }
            }

            index += 1;
        }

        unsafe { RegCloseKey(hkey) };
    }

    // 去重（按名称）
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(app.name.clone()));

    Ok(apps)
}

#[cfg(not(windows))]
pub fn enumerate_installed_apps() -> Result<Vec<AppInfo>, String> {
    Ok(Vec::new())
}

/// 从子键读取应用信息。
#[cfg(windows)]
fn read_app_info_from_subkey(parent: winapi::shared::minwindef::HKEY, subkey_name: &str) -> Option<AppInfo> {
    use winapi::um::winreg::{RegOpenKeyExW, RegQueryValueExW, RegCloseKey, HKEY_LOCAL_MACHINE};
    use winapi::um::winnt::{KEY_READ, REG_SZ};
    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::shared::minwindef::HKEY;

    let subkey_path = format!(
        r"SOFTWARE\Microsoft\Windows\CurrentVersion\Uninstall\{}",
        subkey_name
    );
    let wide_subkey: Vec<u16> = subkey_path.encode_utf16().chain(std::iter::once(0)).collect();
    let mut hkey: HKEY = std::ptr::null_mut();

    let result = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            wide_subkey.as_ptr(),
            0,
            KEY_READ,
            &mut hkey,
        )
    };

    if result != 0 {
        // 尝试 WOW6432Node
        let wow_subkey = format!(
            r"SOFTWARE\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall\{}",
            subkey_name
        );
        let wide_wow: Vec<u16> = wow_subkey.encode_utf16().chain(std::iter::once(0)).collect();
        let result = unsafe {
            RegOpenKeyExW(
                HKEY_LOCAL_MACHINE,
                wide_wow.as_ptr(),
                0,
                KEY_READ,
                &mut hkey,
            )
        };
        if result != 0 {
            return None;
        }
    }

    let display_name = read_registry_string(hkey, "DisplayName");
    let install_location = read_registry_string(hkey, "InstallLocation");
    let publisher = read_registry_string(hkey, "Publisher");
    let uninstall_string = read_registry_string(hkey, "UninstallString");
    let display_version = read_registry_string(hkey, "DisplayVersion");

    unsafe { RegCloseKey(hkey) };

    let name = display_name?;
    if name.is_empty() {
        return None;
    }

    Some(AppInfo {
        name,
        version: display_version.unwrap_or_default(),
        publisher,
        install_path: install_location,
        uninstall_string,
    })
}

/// 读取注册表字符串值。
#[cfg(windows)]
fn read_registry_string(hkey: winapi::shared::minwindef::HKEY, value_name: &str) -> Option<String> {
    use winapi::um::winreg::RegQueryValueExW;
    use winapi::um::winnt::REG_SZ;
    use winapi::shared::winerror::ERROR_SUCCESS;
    use winapi::shared::minwindef::{DWORD, LPBYTE};

    let wide_name: Vec<u16> = value_name.encode_utf16().chain(std::iter::once(0)).collect();
    let mut data_type: DWORD = 0;
    let mut data_len: DWORD = 0;

    // 第一次调用获取大小
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            wide_name.as_ptr(),
            std::ptr::null_mut(),
            &mut data_type,
            std::ptr::null_mut(),
            &mut data_len,
        )
    };

    if result != 0 || data_type != 1 || data_len == 0 {
        return None;
    }

    let mut data_buf = vec![0u8; data_len as usize];
    let result = unsafe {
        RegQueryValueExW(
            hkey,
            wide_name.as_ptr(),
            std::ptr::null_mut(),
            &mut data_type,
            data_buf.as_mut_ptr() as LPBYTE,
            &mut data_len,
        )
    };

    if result != 0 {
        return None;
    }

    // 转换为字符串（去掉末尾 null）
    let wide_slice: &[u16] = unsafe {
        std::slice::from_raw_parts(
            data_buf.as_ptr() as *const u16,
            data_buf.len() / 2,
        )
    };

    let s = String::from_utf16_lossy(wide_slice);
    Some(s.trim_end_matches('\0').to_string())
}

// ============================================================
// 路径关联查询
// ============================================================

/// 查询文件是否属于已安装应用。
pub fn query_installed_app_for_path(path: &Path) -> Result<Option<AppInfo>, String> {
    let apps = enumerate_installed_apps()?;
    let path_str = path.to_string_lossy().to_ascii_lowercase();

    for app in apps {
        if let Some(ref install_path) = app.install_path {
            let install_lower = install_path.to_ascii_lowercase();
            if path_str.starts_with(&install_lower) {
                return Ok(Some(app));
            }
        }
    }

    Ok(None)
}

/// 查询路径是否在注册表中被引用。
pub fn query_path_in_registry(_path: &Path) -> Result<Vec<String>, String> {
    // TODO: 实现更全面的注册表引用搜索
    // 当前版本：通过已安装应用关联判断
    Ok(Vec::new())
}
