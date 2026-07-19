//! 服务查询封装
//!
//! 查询服务依赖关系和状态。

use std::path::Path;

/// 查询文件被哪些服务依赖。
pub fn query_service_dependencies(_path: &Path) -> Result<Vec<String>, String> {
    // TODO: 实现完整的服务依赖查询
    // 需要 Windows SCM API (OpenSCManager, EnumServicesStatusEx, QueryServiceConfig)
    Ok(Vec::new())
}
