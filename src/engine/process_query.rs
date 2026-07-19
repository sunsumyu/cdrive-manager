//! 进程查询封装
//!
//! 查询进程信息和文件占用状态。

use std::path::Path;

/// 查询占用文件的进程列表。
///
/// 使用 Windows Restart Manager API 获取占用指定文件的进程。
#[cfg(windows)]
pub fn query_locking_processes(path: &Path) -> Result<Vec<String>, String> {
    // Restart Manager API 需要链接 rstrtmgr.lib
    // 当前版本：使用 CreateFile 检测是否被占用，返回通用描述
    use crate::engine::file_handle::is_file_in_use;

    if is_file_in_use(path)? {
        Ok(vec!["未知进程".to_string()])
    } else {
        Ok(Vec::new())
    }
}

#[cfg(not(windows))]
pub fn query_locking_processes(_path: &Path) -> Result<Vec<String>, String> {
    Ok(Vec::new())
}

/// 查询系统进程列表。
pub fn query_processes() -> Result<Vec<ProcessInfo>, String> {
    // TODO: 实现 NtQuerySystemInformation(SystemProcessInformation) 查询
    Ok(Vec::new())
}

/// 进程信息。
#[derive(Debug, Clone)]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub executable_path: String,
    pub memory_usage: u64,
}
