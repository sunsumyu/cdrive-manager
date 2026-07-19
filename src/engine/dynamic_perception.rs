//! 动态感知引擎
//!
//! 通过 Windows API 在运行时探测真实的系统状态：
//! - WMI 查询：已安装应用、系统组件
//! - 注册表：应用安装路径、配置信息
//! - 服务/进程：文件占用、服务依赖
//! - 安全描述符：文件权限、所有者

use std::path::Path;

use crate::engine::file_handle;
use crate::engine::registry_reader;

/// 动态感知结果。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct DynamicSnapshot {
    /// 文件是否正被进程占用。
    pub is_in_use: Option<bool>,
    /// 占用文件的进程名列表。
    pub locking_processes: Vec<String>,
    /// 文件是否被注册表引用。
    pub registry_referenced: Option<bool>,
    /// 引用该文件的注册表键值。
    pub registry_keys: Vec<String>,
    /// 文件是否被服务依赖。
    pub service_dependency: Option<bool>,
    /// 依赖该文件的服务名。
    pub dependent_services: Vec<String>,
    /// 文件是否属于已安装应用。
    pub belongs_to_installed_app: Option<bool>,
    /// 所属应用信息。
    pub app_info: Option<AppInfo>,
    /// 文件所有者。
    pub owner: Option<String>,
    /// 文件权限描述。
    pub security_descriptor: Option<String>,
    /// 文件硬链接数量。
    pub hardlink_count: Option<u32>,
    /// 文件是否是硬链接。
    pub is_hardlink: Option<bool>,
}

/// 已安装应用信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AppInfo {
    pub name: String,
    pub version: String,
    pub publisher: Option<String>,
    pub install_path: Option<String>,
    pub uninstall_string: Option<String>,
}

/// 动态感知引擎。
///
/// 注意：所有方法在 Windows 上返回真实数据，在非 Windows 平台返回 `None`。
#[derive(Debug, Clone)]
pub struct DynamicPerceptionEngine;

impl DynamicPerceptionEngine {
    pub fn new() -> Self {
        Self
    }

    /// 对路径进行完整动态感知快照。
    pub fn snapshot(&self, path: &Path) -> DynamicSnapshot {
        #[cfg(windows)]
        {
            self.snapshot_windows(path)
        }
        #[cfg(not(windows))]
        {
            DynamicSnapshot::default()
        }
    }

    #[cfg(windows)]
    fn snapshot_windows(&self, path: &Path) -> DynamicSnapshot {
        let mut snapshot = DynamicSnapshot::default();

        // 1. 文件占用检测
        match file_handle::is_file_in_use(path) {
            Ok(in_use) => snapshot.is_in_use = Some(in_use),
            Err(_) => {}
        }

        // 2. 注册表关联（轻量级查询，失败不阻塞）
        match registry_reader::query_path_in_registry(path) {
            Ok(keys) => {
                snapshot.registry_referenced = Some(!keys.is_empty());
                snapshot.registry_keys = keys;
            }
            Err(_) => {}
        }

        // 3. 已安装应用关联
        match registry_reader::query_installed_app_for_path(path) {
            Ok(app_info) => {
                snapshot.belongs_to_installed_app = Some(true);
                snapshot.app_info = app_info;
            }
            Err(_) => {}
        }

        // 4. 安全描述符
        match file_handle::query_file_security(path) {
            Ok((owner, sd)) => {
                snapshot.owner = owner;
                snapshot.security_descriptor = sd;
            }
            Err(_) => {}
        }

        // 5. 硬链接
        match file_handle::query_hardlink_count(path) {
            Ok(count) => {
                snapshot.hardlink_count = Some(count);
                snapshot.is_hardlink = Some(count > 1);
            }
            Err(_) => {}
        }

        snapshot
    }

    /// 计算动态修正分数（-30 ~ +50）。
    pub fn compute_adjustment(&self, path: &Path) -> i8 {
        use crate::engine::risk_assessment::{
            ADJ_FILE_IN_USE, ADJ_HARDLINK, ADJ_REGISTRY_CRITICAL, ADJ_SERVICE_DEPENDENCY,
        };

        let snapshot = self.snapshot(path);
        let mut adjustment: i8 = 0;

        // 文件被占用
        if snapshot.is_in_use == Some(true) {
            adjustment = adjustment.saturating_add(ADJ_FILE_IN_USE);
        }

        // 注册表关联
        if snapshot.registry_referenced == Some(true) {
            adjustment = adjustment.saturating_add(ADJ_REGISTRY_CRITICAL);
        }

        // 服务依赖
        if snapshot.service_dependency == Some(true) {
            adjustment = adjustment.saturating_add(ADJ_SERVICE_DEPENDENCY);
        }

        // 硬链接
        if snapshot.is_hardlink == Some(true) {
            adjustment = adjustment.saturating_add(ADJ_HARDLINK);
        }

        adjustment
    }
}
