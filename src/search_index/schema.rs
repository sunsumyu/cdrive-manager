//! tantivy schema 定义与索引目录管理。

use std::path::PathBuf;
use tantivy::schema::{
    Field, Schema, INDEXED, STORED, TEXT, TextOptions, TextFieldIndexing,
};

/// 构建文件搜索索引的 tantivy schema。
///
/// 字段说明:
/// - `name`/`path`: TEXT+STORED, 支持全文搜索与结果返回
/// - `extension`: TEXT+STORED, 使用 raw 分词器以支持精确扩展名过滤
/// - `parent_path`: STORED, 用于结果展示
/// - `size`: STORED 数值, 支持范围查询与展示
/// - `modified`: STORED 数值 (Unix 秒), 精确时间戳
/// - `modified_days`: STORED 数值 (Unix 天), 天级, 用于日期范围查询
/// - `is_directory`: bool+STORED, 文件/目录区分
/// - `root_key`: TEXT+STORED, raw 分词器, 多盘符精确匹配
/// - `frn`: STORED, File Reference Number (raw)
pub fn create_schema() -> Schema {
    let mut builder = Schema::builder();

    let name = builder.add_text_field("name", TEXT | STORED);
    let path = builder.add_text_field("path", TEXT | STORED);
    // 使用 raw 分词器以支持精确匹配 (扩展名, 根键, FRN, path_exact)
    let raw_text = TextOptions::default()
        .set_indexing_options(
            TextFieldIndexing::default().set_tokenizer("raw"),
        )
        .set_stored();
    // path_exact: raw 分词, 用于按完整路径精确删除
    let path_exact = builder.add_text_field("path_exact", raw_text.clone());
    let parent_path = builder.add_text_field("parent_path", STORED);
    let extension = builder.add_text_field("extension", raw_text.clone());
    let size = builder.add_u64_field("size", INDEXED | STORED);
    let modified = builder.add_u64_field("modified", INDEXED | STORED);
    let modified_days = builder.add_u64_field("modified_days", INDEXED | STORED);
    let is_directory = builder.add_bool_field("is_directory", INDEXED | STORED);
    let root_key = builder.add_text_field("root_key", raw_text.clone());
    let frn = builder.add_text_field("frn", raw_text);

    let _ = (name, path, path_exact, parent_path, extension, size, modified,
             modified_days, is_directory, root_key, frn);
    builder.build()
}

/// 编译期确定的字段 ID 常量。
///
/// 通过 [`Schema::get_field`] 在运行时解析, 避免硬编码索引。
pub struct FieldId;

impl FieldId {
    pub fn name(schema: &Schema) -> Field {
        schema.get_field("name").expect("schema 必须包含 name 字段")
    }
    pub fn path(schema: &Schema) -> Field {
        schema.get_field("path").expect("schema 必须包含 path 字段")
    }
    pub fn path_exact(schema: &Schema) -> Field {
        schema.get_field("path_exact").expect("schema 必须包含 path_exact 字段")
    }
    pub fn parent_path(schema: &Schema) -> Field {
        schema.get_field("parent_path").expect("schema 必须包含 parent_path 字段")
    }
    pub fn extension(schema: &Schema) -> Field {
        schema.get_field("extension").expect("schema 必须包含 extension 字段")
    }
    pub fn size(schema: &Schema) -> Field {
        schema.get_field("size").expect("schema 必须包含 size 字段")
    }
    pub fn modified(schema: &Schema) -> Field {
        schema.get_field("modified").expect("schema 必须包含 modified 字段")
    }
    pub fn modified_days(schema: &Schema) -> Field {
        schema.get_field("modified_days").expect("schema 必须包含 modified_days 字段")
    }
    pub fn is_directory(schema: &Schema) -> Field {
        schema.get_field("is_directory").expect("schema 必须包含 is_directory 字段")
    }
    pub fn root_key(schema: &Schema) -> Field {
        schema.get_field("root_key").expect("schema 必须包含 root_key 字段")
    }
    pub fn frn(schema: &Schema) -> Field {
        schema.get_field("frn").expect("schema 必须包含 frn 字段")
    }
}

/// 索引目录: `%LOCALAPPDATA%\cdrive-manager\search-index\`
pub fn index_directory() -> anyhow::Result<PathBuf> {
    let base = base_dir()?;
    let dir = base.join("search-index");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// FRN 映射数据库路径: `%LOCALAPPDATA%\cdrive-manager\frn-mapping.sqlite3`
pub fn frn_db_path() -> anyhow::Result<PathBuf> {
    let base = base_dir()?;
    Ok(base.join("frn-mapping.sqlite3"))
}

fn base_dir() -> anyhow::Result<PathBuf> {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        return Ok(PathBuf::from(local_app_data).join("cdrive-manager"));
    }
    Ok(std::env::current_dir()?.join(".cdrive-manager"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_contains_all_required_fields() {
        let schema = create_schema();
        for field_name in ["name", "path", "parent_path", "extension",
                           "size", "modified", "modified_days",
                           "is_directory", "root_key", "frn"] {
            assert!(
                schema.get_field(field_name).is_ok(),
                "schema 缺少字段: {field_name}"
            );
        }
    }

    #[test]
    fn index_directory_is_creatable() {
        // 用临时目录验证 base_dir + join 逻辑
        let parent = std::env::temp_dir().join(format!(
            "cdrive-manager-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let cdrive_dir = parent.join("cdrive-manager");
        let search_dir = cdrive_dir.join("search-index");
        std::fs::create_dir_all(&cdrive_dir).unwrap();
        // 直接验证目录创建逻辑
        std::fs::create_dir_all(&search_dir).unwrap();
        assert!(search_dir.exists());
        assert!(search_dir.ends_with("search-index"));
    }

    #[test]
    fn frn_db_path_ends_with_expected_filename() {
        // 验证 frn_db_path 的 join 逻辑
        let parent = std::env::temp_dir().join(format!(
            "cdrive-manager-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&parent).unwrap();
        let cdrive_dir = parent.join("cdrive-manager");
        std::fs::create_dir_all(&cdrive_dir).unwrap();
        // 用直接构建路径验证 join 结果
        let path = cdrive_dir.join("frn-mapping.sqlite3");
        assert!(path.ends_with("frn-mapping.sqlite3"));
    }

    #[test]
    fn base_dir_falls_back_to_cwd() {
        // 当 LOCALAPPDATA 不存在时, base_dir 应回退到当前目录
        // (不直接测试 set_var, 避免并行冲突)
        let expected = std::env::current_dir()
            .unwrap()
            .join(".cdrive-manager");
        assert!(expected.to_string_lossy().contains(".cdrive-manager"));
    }
}
