# Everything 风格快速搜索功能 Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** 用 tantivy 全文搜索引擎替换现有 SQLite FTS5 搜索索引,实现 Everything 风格的即时文件搜索,支持完整 DSL 查询语法(关键字/扩展名/大小/日期/路径/正则/布尔运算)、可切换的表格/紧凑列表视图,以及完整的文件操作能力。

**Architecture:** 新建 tantivy 索引目录与现有 SQLite 缓存并存;FRN→路径 映射保留在轻量 SQLite 表中;DSL 解析器将查询字符串编译为 tantivy Query 对象;USN Journal 监听器适配新索引器实现增量更新;搜索面板新增视图切换与右键操作菜单。

**Tech Stack:** Rust 2024 edition, tantivy 0.22 (全文搜索), nom 7.1 (DSL 解析器), chrono 0.4 (日期处理), rusqlite 0.32 (FRN 映射), egui 0.32 (GUI), crossbeam-channel 0.5 (线程通信), trash 5 (文件删除), open 5 (打开文件)

## Global Constraints

- **平台:** Windows 10 19045 x64,USN Journal 与 MFT 功能为 Windows 专有(用 `#[cfg(windows)]` 守卫)
- **edition:** 2024 (Cargo.toml 已配置)
- **UI 语言:** 中文,所有用户可见字符串使用中文,复用现有 `format::bytes()` / `format::count()` 工具
- **字体:** 复用 main.rs 中已配置的 MSYH 中文字体
- **存储位置:** tantivy 索引目录为 `%LOCALAPPDATA%\cdrive-manager\search-index\`,FRN 映射库为 `%LOCALAPPDATA%\cdrive-manager\frn-mapping.sqlite3`,与现有 `scan-cache.sqlite3` 并存
- **保留兼容:** `search_index::FileSearchResult` 结构体字段不变,`spawn_search` / `spawn_build_index` / `spawn_usn_index_listener` 公共 API 签名保持兼容,app.rs 导入路径无需改动
- **测试:** 所有新模块必须有单元测试,沿用现有 `#[cfg(test)] mod tests` 模式
- **提交规范:** 每个任务结束提交一次,提交信息格式 `feat: <描述>` 或 `test: <描述>`
- **TDD:** 先写失败测试,再实现代码

---

## File Structure

新建/修改的文件及其职责:

| 文件 | 操作 | 职责 |
|------|------|------|
| `Cargo.toml` | 修改 | 添加 tantivy、nom、chrono 依赖 |
| `src/search_index/mod.rs` | 修改 | 更新公共 API 导出 |
| `src/search_index/schema.rs` | 新建 | tantivy schema 定义、字段常量、索引目录管理 |
| `src/search_index/frn_db.rs` | 新建 | FRN→路径 映射的 SQLite 操作(从 db.rs 抽取并保留) |
| `src/search_index/query.rs` | 新建 | DSL 解析器(nom)、QueryNode AST、tantivy Query 编译器 |
| `src/search_index/indexer.rs` | 新建 | SearchIndexer 核心类:打开/构建/增量更新/搜索 |
| `src/search_index/worker.rs` | 重写 | 后台索引构建/搜索线程,适配新 indexer |
| `src/search_index/usn_journal.rs` | 修改 | USN 事件处理适配新 indexer(UsnEvent 不变) |
| `src/search_index/db.rs` | 删除 | 被 schema.rs + frn_db.rs + indexer.rs 取代 |
| `src/app.rs` | 修改 | 搜索 UI 重构:视图切换、紧凑列表、右键操作菜单 |
| `tests/search_integration.rs` | 新建 | 端到端集成测试 |

**职责边界说明:** `schema.rs` 只管 schema 与目录路径;`frn_db.rs` 只管 FRN→路径 KV 查询;`query.rs` 只管字符串→AST→Query 的编译;`indexer.rs` 协调 schema/frn_db/query 完成索引读写与搜索;`worker.rs` 把 indexer 包装成后台线程;`usn_journal.rs` 保持原有 USN 监听,事件通过 indexer 处理。

---

### Task 1: 添加依赖并验证编译

**Files:**
- Modify: `Cargo.toml:8-25`

**Interfaces:**
- Produces: `tantivy`、`nom`、`chrono` 三个 crate 可在 `src/search_index/` 下引用

- [ ] **Step 1: 添加依赖到 Cargo.toml**

在 `[dependencies]` 段末尾(第 25 行 `walkdir = "2"` 之后)添加:

```toml
tantivy = "0.22"
nom = "7.1"
chrono = "0.4"
```

- [ ] **Step 2: 验证依赖能拉取并编译**

Run: `cargo build`
Expected: 编译成功(可能有未使用依赖警告,可忽略)

- [ ] **Step 3: 提交**

```bash
git add Cargo.toml Cargo.lock
git commit -m "feat: 添加 tantivy/nom/chrono 搜索引擎依赖"
```

---

### Task 2: 创建 tantivy schema 模块

**Files:**
- Create: `src/search_index/schema.rs`
- Modify: `src/search_index/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn create_schema() -> tantivy::schema::Schema`
  - `pub struct FieldId;` 含常量: `NAME`/`PATH`/`PARENT_PATH`/`EXTENSION`/`SIZE`/`MODIFIED`/`MODIFIED_DAYS`/`IS_DIRECTORY`/`ROOT_KEY`/`FRN` (均为 `tantivy::schema::Field`)
  - `pub fn index_directory() -> anyhow::Result<std::path::PathBuf>` (返回 `%LOCALAPPDATA%\cdrive-manager\search-index\`,不存在则创建)
  - `pub fn frn_db_path() -> anyhow::Result<std::path::PathBuf>` (返回 `%LOCALAPPDATA%\cdrive-manager\frn-mapping.sqlite3`)

- [ ] **Step 1: 编写 schema.rs 的失败测试**

创建 `src/search_index/schema.rs`:

```rust
//! tantivy schema 定义与索引目录管理。

use std::path::PathBuf;
use tantivy::schema::{Field, Schema, TEXT, STORED, INDEXED};

/// 构建文件搜索索引的 tantivy schema。
///
/// 字段说明见设计文档第 2 部分。`name`/`path` 使用 TEXT+STORED 支持全文搜索与结果返回;
/// `size`/`modified` 用 STORED 数值字段支持范围查询与展示;`modified_days` 为天级时间戳,
/// 专用于日期范围查询以减少索引膨胀。
pub fn create_schema() -> Schema {
    let mut builder = Schema::builder();

    let name = builder.add_text_field("name", TEXT | STORED);
    let path = builder.add_text_field("path", TEXT | STORED);
    let parent_path = builder.add_text_field("parent_path", STORED);
    let extension = builder.add_text_field("extension", TEXT | STORED);
    let size = builder.add_u64_field("size", STORED);
    let modified = builder.add_u64_field("modified", STORED);
    let modified_days = builder.add_u64_field("modified_days", INDEXED);
    let is_directory = builder.add_bool_field("is_directory", STORED);
    let root_key = builder.add_text_field("root_key", TEXT | STORED);
    let frn = builder.add_text_field("frn", STORED);

    let _ = (name, path, parent_path, extension, size, modified,
             modified_days, is_directory, root_key, frn);
    builder.build()
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
        // 所有字段必须存在
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
        // 使用临时环境变量避免污染真实 LOCALAPPDATA
        let tmp = tempfile_dir();
        std::env::set_var("LOCALAPPDATA", &tmp);
        let dir = index_directory().expect("应能创建索引目录");
        assert!(dir.exists(), "索引目录应已创建");
        assert!(dir.ends_with("search-index"));
    }

    #[test]
    fn frn_db_path_ends_with_expected_filename() {
        let tmp = tempfile_dir();
        std::env::set_var("LOCALAPPDATA", &tmp);
        let path = frn_db_path().expect("应能返回 FRN DB 路径");
        assert!(path.ends_with("frn-mapping.sqlite3"));
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "cdrive-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }
}
```

- [ ] **Step 2: 在 mod.rs 注册 schema 模块**

修改 `src/search_index/mod.rs`,在 `mod db;` 之前添加 `mod schema;`,并在 re-export 块中导出:

打开 `src/search_index/mod.rs`,将第 18 行 `mod db;` 替换为:

```rust
mod schema;
mod db;
mod usn_journal;
mod worker;
```

在 `pub use db::{...}` 块之后添加:

```rust
#[allow(unused_imports)]
pub use schema::{create_schema, index_directory, frn_db_path};
```

- [ ] **Step 3: 运行测试验证失败**

Run: `cargo test --lib search_index::schema`
Expected: PASS(此任务测试即实现,验证基础设施可用)

- [ ] **Step 4: 提交**

```bash
git add src/search_index/schema.rs src/search_index/mod.rs
git commit -m "feat: 添加 tantivy schema 与索引目录管理"
```

---

### Task 3: 创建 FRN 映射数据库模块

**Files:**
- Create: `src/search_index/frn_db.rs`
- Modify: `src/search_index/mod.rs`

**Interfaces:**
- Produces:
  - `pub fn open_frn_db() -> rusqlite::Result<rusqlite::Connection>`
  - `pub fn upsert_frn_path(conn: &Connection, root_key: &str, frn: &str, path: &str, parent_frn: Option<&str>, is_directory: bool) -> rusqlite::Result<()>`
  - `pub fn lookup_frn_path(conn: &Connection, root_key: &str, frn: &str) -> rusqlite::Result<Option<String>>`
  - `pub fn resolve_path_from_frn(conn: &Connection, root_key: &str, parent_frn: &str, file_name: &str) -> rusqlite::Result<Option<String>>`
  - `pub fn delete_frn_path(conn: &Connection, root_key: &str, frn: &str) -> rusqlite::Result<()>`
  - `pub fn clear_frn_for_root(conn: &Connection, root_key: &str) -> rusqlite::Result<()>`

- [ ] **Step 1: 编写 frn_db.rs 失败测试**

创建 `src/search_index/frn_db.rs`:

```rust
//! FRN(File Reference Number)→路径 映射的 SQLite 存储。
//!
//! USN Journal 通过 FRN 标识文件,但搜索索引需要完整路径。此模块维护
//! `(root_key, frn) → path` 的 KV 映射,供 USN 增量更新时解析路径。

use rusqlite::{Connection, OptionalExtension, params};
use crate::search_index::schema::frn_db_path;

/// 打开 FRN 映射数据库并初始化表结构。
pub fn open_frn_db() -> rusqlite::Result<Connection> {
    let path = frn_db_path().map_err(|e| {
        rusqlite::Error::ToSqlConversionFailure(e.into())
    })?;
    let conn = Connection::open(&path)?;
    conn.execute_batch(
        "PRAGMA journal_mode = WAL;
         PRAGMA synchronous = NORMAL;
         CREATE TABLE IF NOT EXISTS frn_path_map (
             root_key TEXT NOT NULL,
             frn TEXT NOT NULL,
             path TEXT NOT NULL,
             parent_frn TEXT,
             is_directory INTEGER NOT NULL DEFAULT 0,
             PRIMARY KEY (root_key, frn)
         );
         CREATE INDEX IF NOT EXISTS idx_frn_root ON frn_path_map(root_key, frn);
         CREATE INDEX IF NOT EXISTS idx_frn_parent ON frn_path_map(root_key, parent_frn);",
    )?;
    Ok(conn)
}

/// 插入或更新 FRN→路径 映射。
pub fn upsert_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
    path: &str,
    parent_frn: Option<&str>,
    is_directory: bool,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO frn_path_map (root_key, frn, path, parent_frn, is_directory)
         VALUES (?1, ?2, ?3, ?4, ?5)
         ON CONFLICT(root_key, frn) DO UPDATE SET
            path = excluded.path,
            parent_frn = excluded.parent_frn,
            is_directory = excluded.is_directory",
        params![root_key, frn, path, parent_frn, is_directory as i32],
    )?;
    Ok(())
}

/// 通过 FRN 查询已存储的路径。
pub fn lookup_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
        params![root_key, frn],
        |row| row.get::<_, String>(0),
    )
    .optional()
}

/// 通过父 FRN 与文件名解析路径(父路径 + 文件名)。
pub fn resolve_path_from_frn(
    conn: &Connection,
    root_key: &str,
    parent_frn: &str,
    file_name: &str,
) -> rusqlite::Result<Option<String>> {
    let parent_path: Option<String> = conn
        .query_row(
            "SELECT path FROM frn_path_map WHERE root_key = ?1 AND frn = ?2 LIMIT 1",
            params![root_key, parent_frn],
            |row| row.get(0),
        )
        .optional()?;
    Ok(parent_path.map(|p| format!("{}\\{}", p, file_name)))
}

/// 删除指定 FRN 的映射。
pub fn delete_frn_path(
    conn: &Connection,
    root_key: &str,
    frn: &str,
) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM frn_path_map WHERE root_key = ?1 AND frn = ?2",
        params![root_key, frn],
    )?;
    Ok(())
}

/// 清除指定 root_key 的所有 FRN 映射(用于全量重建)。
pub fn clear_frn_for_root(conn: &Connection, root_key: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM frn_path_map WHERE root_key = ?1",
        params![root_key],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn setup_temp_db() -> Connection {
        let tmp = std::env::temp_dir().join(format!(
            "cdrive-frn-test-{}-{}.sqlite3",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::env::set_var("LOCALAPPDATA", tmp.parent().unwrap());
        // 先删除可能残留的同名文件
        let _ = std::fs::remove_file(&tmp);
        open_frn_db().expect("应能打开 FRN DB")
    }

    #[test]
    fn upsert_and_lookup_round_trip() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\file.txt", Some("50"), false).unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert_eq!(path.as_deref(), Some("C:\\file.txt"));
    }

    #[test]
    fn upsert_overwrites_existing() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\old.txt", None, false).unwrap();
        upsert_frn_path(&conn, "c:/", "100", "C:\\new.txt", None, false).unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert_eq!(path.as_deref(), Some("C:\\new.txt"));
    }

    #[test]
    fn resolve_path_from_parent_frn() {
        let conn = setup_temp_db();
        // 父目录 FRN=50,路径 C:\Users
        upsert_frn_path(&conn, "c:/", "50", "C:\\Users", Some("1"), true).unwrap();
        let resolved = resolve_path_from_frn(&conn, "c:/", "50", "file.txt").unwrap();
        assert_eq!(resolved.as_deref(), Some("C:\\Users\\file.txt"));
    }

    #[test]
    fn delete_removes_mapping() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\file.txt", None, false).unwrap();
        delete_frn_path(&conn, "c:/", "100").unwrap();
        let path = lookup_frn_path(&conn, "c:/", "100").unwrap();
        assert!(path.is_none());
    }

    #[test]
    fn clear_for_root_removes_all() {
        let conn = setup_temp_db();
        upsert_frn_path(&conn, "c:/", "100", "C:\\a.txt", None, false).unwrap();
        upsert_frn_path(&conn, "c:/", "101", "C:\\b.txt", None, false).unwrap();
        upsert_frn_path(&conn, "d:/", "200", "D:\\c.txt", None, false).unwrap();
        clear_frn_for_root(&conn, "c:/").unwrap();
        assert!(lookup_frn_path(&conn, "c:/", "100").unwrap().is_none());
        assert!(lookup_frn_path(&conn, "c:/", "101").unwrap().is_none());
        // 其他 root 不受影响
        assert!(lookup_frn_path(&conn, "d:/", "200").unwrap().is_some());
    }
}
```

- [ ] **Step 2: 在 mod.rs 注册 frn_db 模块**

修改 `src/search_index/mod.rs`,在 `mod schema;` 之后添加 `mod frn_db;`,并在 re-export 块中导出:

```rust
mod schema;
mod frn_db;
mod db;
mod usn_journal;
mod worker;
```

在 schema re-export 之后添加:

```rust
#[allow(unused_imports)]
pub use frn_db::{
    open_frn_db, upsert_frn_path, lookup_frn_path,
    resolve_path_from_frn, delete_frn_path, clear_frn_for_root,
};
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib search_index::frn_db`
Expected: 5 个测试全部 PASS

- [ ] **Step 4: 提交**

```bash
git add src/search_index/frn_db.rs src/search_index/mod.rs
git commit -m "feat: 添加 FRN→路径映射数据库模块"
```

---

### Task 4: 实现 DSL 查询解析器 - 基础类型与简单关键字

**Files:**
- Create: `src/search_index/query.rs`
- Modify: `src/search_index/mod.rs`

**Interfaces:**
- Produces:
  - `pub enum QueryNode` (含 Keywords/Phrase/Extension/Size/SizeRange/Date/DateRange/Path/Regex/And/Or/Not/Group/Empty 变体)
  - `pub enum CompareOp` (Eq/Gt/Lt/Gte/Lte)
  - `pub enum DateValue` (Today/Yesterday/ThisWeek/ThisMonth/Absolute(chrono::NaiveDate))
  - `pub enum QueryParseError` (Empty/InvalidSyntax(String)/InvalidSize(String)/InvalidDate(String)/InvalidRegex(String))
  - `pub fn parse_query(input: &str) -> Result<QueryNode, QueryParseError>`

- [ ] **Step 1: 编写 query.rs 基础结构与关键字解析测试**

创建 `src/search_index/query.rs`,先定义所有类型与 `parse_query` 的骨架(只处理简单关键字):

```rust
//! 查询 DSL 解析器:将用户输入字符串编译为 tantivy Query。
//!
//! 支持的语法见设计文档第 3 部分。

use chrono::NaiveDate;

/// 查询 AST 节点。
#[derive(Debug, Clone, PartialEq)]
pub enum QueryNode {
    /// 空查询(匹配全部)
    Empty,
    /// 空格分隔的多个关键字(默认 AND,在 name/path 中搜索)
    Keywords(Vec<String>),
    /// 引号包裹的短语(连续匹配)
    Phrase(String),
    /// 扩展名过滤: ext:pdf,doc
    Extension(Vec<String>),
    /// 大小比较: size:>100MB
    Size { op: CompareOp, value: u64 },
    /// 大小范围: size:1KB-10MB
    SizeRange { min: u64, max: u64 },
    /// 日期比较: dm:>2024-01-01
    Date { op: CompareOp, value: DateValue },
    /// 日期范围: dm:2024-01-01..2024-12-31
    DateRange { start: DateValue, end: DateValue },
    /// 路径限定: path:Downloads
    Path(String),
    /// 正则表达式: regex:^Report-\d{4}
    Regex(String),
    /// 布尔 AND
    And(Box<QueryNode>, Box<QueryNode>),
    /// 布尔 OR
    Or(Box<QueryNode>, Box<QueryNode>),
    /// 布尔 NOT
    Not(Box<QueryNode>),
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum CompareOp {
    Eq,
    Gt,
    Lt,
    Gte,
    Lte,
}

#[derive(Debug, Clone, PartialEq)]
pub enum DateValue {
    Today,
    Yesterday,
    ThisWeek,
    ThisMonth,
    Absolute(NaiveDate),
}

#[derive(Debug, Clone, PartialEq)]
pub enum QueryParseError {
    Empty,
    InvalidSyntax(String),
    InvalidSize(String),
    InvalidDate(String),
    InvalidRegex(String),
}

impl std::fmt::Display for QueryParseError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Empty => write!(f, "查询为空"),
            Self::InvalidSyntax(s) => write!(f, "语法错误: {s}"),
            Self::InvalidSize(s) => write!(f, "无效的大小值: {s}"),
            Self::InvalidDate(s) => write!(f, "无效的日期: {s}"),
            Self::InvalidRegex(s) => write!(f, "无效的正则表达式: {s}"),
        }
    }
}

impl std::error::Error for QueryParseError {}

/// 解析查询字符串为 AST。
///
/// 当前实现:仅处理空查询与简单关键字(空格分隔)。
/// 后续任务扩展字段过滤、布尔运算、正则等。
pub fn parse_query(input: &str) -> Result<QueryNode, QueryParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(QueryNode::Empty);
    }

    // 检测 regex: 前缀(正则模式,后续任务处理)
    if trimmed.starts_with("regex:") {
        return parse_regex(&trimmed["regex:".len()..]);
    }

    // 检测引号短语(后续任务处理)
    if trimmed.starts_with('"') {
        return parse_phrase(trimmed);
    }

    // 检测字段限定语法(后续任务处理)
    if has_field_syntax(trimmed) {
        return parse_with_fields(trimmed);
    }

    // 默认:空格分隔的关键字
    let keywords: Vec<String> = trimmed
        .split_whitespace()
        .map(|s| s.to_owned())
        .collect();
    if keywords.is_empty() {
        return Ok(QueryNode::Empty);
    }
    Ok(QueryNode::Keywords(keywords))
}

/// 解析大小字符串为字节数。
///
/// 支持后缀: KB/K, MB/M, GB/G, TB/T(1024 进制),无后缀为字节。
pub fn parse_size(s: &str) -> Result<u64, QueryParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Err(QueryParseError::InvalidSize(s.to_owned()));
    }
    let (num_part, multiplier) = if let Some(rest) = s.strip_prefix(['<', '>', '=']) {
        return parse_size(rest);
    } else if let Some(rest) = s
        .strip_suffix("KB").or_else(|| s.strip_suffix("K"))
        .or_else(|| s.strip_suffix("kb")).or_else(|| s.strip_suffix("k"))
    {
        (rest, 1024u64)
    } else if let Some(rest) = s
        .strip_suffix("MB").or_else(|| s.strip_suffix("M"))
        .or_else(|| s.strip_suffix("mb")).or_else(|| s.strip_suffix("m"))
    {
        (rest, 1024u64 * 1024)
    } else if let Some(rest) = s
        .strip_suffix("GB").or_else(|| s.strip_suffix("G"))
        .or_else(|| s.strip_suffix("gb")).or_else(|| s.strip_suffix("g"))
    {
        (rest, 1024u64 * 1024 * 1024)
    } else if let Some(rest) = s
        .strip_suffix("TB").or_else(|| s.strip_suffix("T"))
        .or_else(|| s.strip_suffix("tb")).or_else(|| s.strip_suffix("t"))
    {
        (rest, 1024u64 * 1024 * 1024 * 1024)
    } else {
        (s, 1u64)
    };
    let num: u64 = num_part.trim().parse().map_err(|_| {
        QueryParseError::InvalidSize(s.to_owned())
    })?;
    num.checked_mul(multiplier)
        .ok_or_else(|| QueryParseError::InvalidSize(s.to_owned()))
}

fn has_field_syntax(s: &str) -> bool {
    let fields = ["ext:", "size:", "dm:", "path:"];
    let lower = s.to_ascii_lowercase();
    fields.iter().any(|f| lower.contains(f))
}

fn parse_regex(input: &str) -> Result<QueryNode, QueryParseError> {
    let pattern = input.trim();
    if pattern.is_empty() {
        return Err(QueryParseError::InvalidRegex("空正则".to_owned()));
    }
    // 验证正则有效性(用 tantivy 的 regex crate 间接验证)
    regex::Regex::new(pattern)
        .map_err(|e| QueryParseError::InvalidRegex(e.to_string()))?;
    Ok(QueryNode::Regex(pattern.to_owned()))
}

fn parse_phrase(input: &str) -> Result<QueryNode, QueryParseError> {
    // 后续任务实现完整短语解析
    let trimmed = input.trim();
    if let Some(end) = trimmed.rfind('"') {
        if trimmed.starts_with('"') && end > 0 {
            let phrase = &trimmed[1..end];
            return Ok(QueryNode::Phrase(phrase.to_owned()));
        }
    }
    Err(QueryParseError::InvalidSyntax("未闭合的引号".to_owned()))
}

fn parse_with_fields(input: &str) -> Result<QueryNode, QueryParseError> {
    // 后续任务实现字段语法解析
    // 暂时回退到关键字处理
    let keywords: Vec<String> = input
        .split_whitespace()
        .map(|s| s.to_owned())
        .collect();
    if keywords.is_empty() {
        Ok(QueryNode::Empty)
    } else {
        Ok(QueryNode::Keywords(keywords))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_returns_empty_node() {
        assert_eq!(parse_query("").unwrap(), QueryNode::Empty);
        assert_eq!(parse_query("   ").unwrap(), QueryNode::Empty);
    }

    #[test]
    fn single_keyword_returns_keywords_node() {
        let result = parse_query("report").unwrap();
        assert_eq!(result, QueryNode::Keywords(vec!["report".to_owned()]));
    }

    #[test]
    fn multiple_keywords_are_split_by_whitespace() {
        let result = parse_query("report 2024 pdf").unwrap();
        assert_eq!(result, QueryNode::Keywords(vec![
            "report".to_owned(),
            "2024".to_owned(),
            "pdf".to_owned(),
        ]));
    }

    #[test]
    fn parse_size_plain_bytes() {
        assert_eq!(parse_size("1024").unwrap(), 1024);
    }

    #[test]
    fn parse_size_with_kb_suffix() {
        assert_eq!(parse_size("1KB").unwrap(), 1024);
        assert_eq!(parse_size("2K").unwrap(), 2048);
    }

    #[test]
    fn parse_size_with_mb_suffix() {
        assert_eq!(parse_size("1MB").unwrap(), 1024 * 1024);
        assert_eq!(parse_size("100MB").unwrap(), 100 * 1024 * 1024);
    }

    #[test]
    fn parse_size_with_gb_suffix() {
        assert_eq!(parse_size("1GB").unwrap(), 1024u64 * 1024 * 1024);
    }

    #[test]
    fn parse_size_rejects_invalid() {
        assert!(parse_size("abc").is_err());
        assert!(parse_size("").is_err());
    }

    #[test]
    fn regex_prefix_parses_as_regex_node() {
        let result = parse_query("regex:^Report-\\d{4}").unwrap();
        assert_eq!(result, QueryNode::Regex("^Report-\\d{4}".to_owned()));
    }

    #[test]
    fn invalid_regex_returns_error() {
        assert!(parse_query("regex:[invalid").is_err());
    }

    #[test]
    fn phrase_with_quotes_parses_as_phrase() {
        let result = parse_query("\"annual report\"").unwrap();
        assert_eq!(result, QueryNode::Phrase("annual report".to_owned()));
    }

    #[test]
    fn unclosed_quote_returns_error() {
        assert!(parse_query("\"unclosed").is_err());
    }
}
```

注意: 此步骤引用了 `regex` crate(tantivy 已依赖,需在 Cargo.toml 显式添加)。在 Cargo.toml `[dependencies]` 添加:

```toml
regex = "1"
```

- [ ] **Step 2: 在 mod.rs 注册 query 模块**

修改 `src/search_index/mod.rs`:

```rust
mod schema;
mod frn_db;
mod query;
mod db;
mod usn_journal;
mod worker;
```

在 frn_db re-export 之后添加:

```rust
#[allow(unused_imports)]
pub use query::{parse_query, parse_size, QueryNode, CompareOp, DateValue, QueryParseError};
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib search_index::query`
Expected: 12 个测试全部 PASS

- [ ] **Step 4: 提交**

```bash
git add src/search_index/query.rs src/search_index/mod.rs Cargo.toml Cargo.lock
git commit -m "feat: 添加 DSL 查询解析器基础与简单关键字解析"
```

---

### Task 5: 扩展 DSL 解析器 - 字段语法与布尔运算

**Files:**
- Modify: `src/search_index/query.rs`

**Interfaces:**
- Produces: 扩展 `parse_query` 支持 `ext:`/`size:`/`dm:`/`path:` 字段语法、AND/OR/NOT 布尔运算、括号分组

- [ ] **Step 1: 编写字段语法与布尔运算的失败测试**

在 `src/search_index/query.rs` 的 `#[cfg(test)] mod tests` 中追加测试:

```rust
    #[test]
    fn ext_field_single_extension() {
        let result = parse_query("ext:pdf").unwrap();
        assert_eq!(result, QueryNode::Extension(vec!["pdf".to_owned()]));
    }

    #[test]
    fn ext_field_multiple_extensions() {
        let result = parse_query("ext:pdf,doc,xlsx").unwrap();
        assert_eq!(result, QueryNode::Extension(vec![
            "pdf".to_owned(),
            "doc".to_owned(),
            "xlsx".to_owned(),
        ]));
    }

    #[test]
    fn size_field_greater_than() {
        let result = parse_query("size:>100MB").unwrap();
        assert_eq!(result, QueryNode::Size {
            op: CompareOp::Gt,
            value: 100 * 1024 * 1024,
        });
    }

    #[test]
    fn size_field_less_than() {
        let result = parse_query("size:<1KB").unwrap();
        assert_eq!(result, QueryNode::Size {
            op: CompareOp::Lt,
            value: 1024,
        });
    }

    #[test]
    fn size_field_range() {
        let result = parse_query("size:1KB-10MB").unwrap();
        assert_eq!(result, QueryNode::SizeRange {
            min: 1024,
            max: 10 * 1024 * 1024,
        });
    }

    #[test]
    fn dm_field_today() {
        let result = parse_query("dm:today").unwrap();
        assert_eq!(result, QueryNode::Date {
            op: CompareOp::Eq,
            value: DateValue::Today,
        });
    }

    #[test]
    fn dm_field_absolute_date_greater() {
        let result = parse_query("dm:>2024-01-01").unwrap();
        assert_eq!(result, QueryNode::Date {
            op: CompareOp::Gt,
            value: DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
        });
    }

    #[test]
    fn dm_field_date_range() {
        let result = parse_query("dm:2024-01-01..2024-12-31").unwrap();
        assert_eq!(result, QueryNode::DateRange {
            start: DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 1, 1).unwrap()),
            end: DateValue::Absolute(NaiveDate::from_ymd_opt(2024, 12, 31).unwrap()),
        });
    }

    #[test]
    fn path_field_simple() {
        let result = parse_query("path:Downloads").unwrap();
        assert_eq!(result, QueryNode::Path("Downloads".to_owned()));
    }

    #[test]
    fn boolean_and_combines_nodes() {
        let result = parse_query("report AND pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(*left, QueryNode::Keywords(vec!["report".to_owned()]));
                assert_eq!(*right, QueryNode::Keywords(vec!["pdf".to_owned()]));
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }

    #[test]
    fn boolean_or_combines_nodes() {
        let result = parse_query("report OR summary").unwrap();
        match result {
            QueryNode::Or(left, right) => {
                assert_eq!(*left, QueryNode::Keywords(vec!["report".to_owned()]));
                assert_eq!(*right, QueryNode::Keywords(vec!["summary".to_owned()]));
            }
            other => panic!("期望 Or, 实际: {other:?}"),
        }
    }

    #[test]
    fn boolean_not_wraps_node() {
        let result = parse_query("NOT tmp").unwrap();
        match result {
            QueryNode::Not(inner) => {
                assert_eq!(*inner, QueryNode::Keywords(vec!["tmp".to_owned()]));
            }
            other => panic!("期望 Not, 实际: {other:?}"),
        }
    }

    #[test]
    fn parentheses_group_subquery() {
        let result = parse_query("(report OR summary) AND pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(*right, QueryNode::Keywords(vec!["pdf".to_owned()]));
                match *left {
                    QueryNode::Or(l, r) => {
                        assert_eq!(*l, QueryNode::Keywords(vec!["report".to_owned()]));
                        assert_eq!(*r, QueryNode::Keywords(vec!["summary".to_owned()]));
                    }
                    other => panic!("左子节点应为 Or, 实际: {other:?}"),
                }
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }

    #[test]
    fn combined_keywords_and_field() {
        let result = parse_query("report ext:pdf").unwrap();
        match result {
            QueryNode::And(left, right) => {
                assert_eq!(*left, QueryNode::Keywords(vec!["report".to_owned()]));
                assert_eq!(*right, QueryNode::Extension(vec!["pdf".to_owned()]));
            }
            other => panic!("期望 And, 实际: {other:?}"),
        }
    }
```

- [ ] **Step 2: 实现字段语法与布尔运算解析**

替换 `src/search_index/query.rs` 中的 `parse_query` 与 `parse_with_fields` 函数为完整实现:

```rust
/// 解析查询字符串为 AST。
pub fn parse_query(input: &str) -> Result<QueryNode, QueryParseError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok(QueryNode::Empty);
    }

    // regex: 前缀
    if let Some(rest) = trimmed.strip_prefix("regex:") {
        return parse_regex(rest);
    }

    // 完整解析:支持布尔运算、括号、字段语法
    let tokens = tokenize(trimmed)?;
    let mut parser = TokenStream::new(tokens);
    let node = parser.parse_or()?;
    Ok(node)
}

/// 词法 token
#[derive(Debug, Clone, PartialEq)]
enum Token {
    Word(String),        // 普通关键字或字段语法
    Phrase(String),      // 引号短语
    And,
    Or,
    Not,
    LParen,
    RParen,
}

fn tokenize(input: &str) -> Result<Vec<Token>, QueryParseError> {
    let mut tokens = Vec::new();
    let chars: Vec<char> = input.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        if c.is_whitespace() {
            i += 1;
            continue;
        }
        if c == '(' {
            tokens.push(Token::LParen);
            i += 1;
            continue;
        }
        if c == ')' {
            tokens.push(Token::RParen);
            i += 1;
            continue;
        }
        if c == '"' {
            // 短语
            let mut end = i + 1;
            while end < chars.len() && chars[end] != '"' {
                end += 1;
            }
            if end >= chars.len() {
                return Err(QueryParseError::InvalidSyntax("未闭合的引号".to_owned()));
            }
            let phrase: String = chars[i + 1..end].iter().collect();
            tokens.push(Token::Phrase(phrase));
            i = end + 1;
            continue;
        }
        // 读取单词(直到空白或括号)
        let mut end = i;
        while end < chars.len()
            && !chars[end].is_whitespace()
            && chars[end] != '('
            && chars[end] != ')'
        {
            end += 1;
        }
        let word: String = chars[i..end].iter().collect();
        match word.to_ascii_uppercase().as_str() {
            "AND" => tokens.push(Token::And),
            "OR" => tokens.push(Token::Or),
            "NOT" => tokens.push(Token::Not),
            _ => tokens.push(Token::Word(word)),
        }
        i = end;
    }
    Ok(tokens)
}

struct TokenStream {
    tokens: Vec<Token>,
    pos: usize,
}

impl TokenStream {
    fn new(tokens: Vec<Token>) -> Self {
        Self { tokens, pos: 0 }
    }

    fn peek(&self) -> Option<&Token> {
        self.tokens.get(self.pos)
    }

    fn next(&mut self) -> Option<Token> {
        let t = self.tokens.get(self.pos).cloned();
        if t.is_some() {
            self.pos += 1;
        }
        t
    }

    /// parse_or: parse_and (OR parse_and)*
    fn parse_or(&mut self) -> Result<QueryNode, QueryParseError> {
        let mut left = self.parse_and()?;
        while let Some(Token::Or) = self.peek() {
            self.next();
            let right = self.parse_and()?;
            left = QueryNode::Or(Box::new(left), Box::new(right));
        }
        Ok(left)
    }

    /// parse_and: parse_not (AND? parse_not)*  (隐式 AND)
    fn parse_and(&mut self) -> Result<QueryNode, QueryParseError> {
        let mut left = self.parse_not()?;
        loop {
            match self.peek() {
                Some(Token::And) => {
                    self.next();
                    let right = self.parse_not()?;
                    left = QueryNode::And(Box::new(left), Box::new(right));
                }
                Some(Token::Word(_)) | Some(Token::Phrase(_)) | Some(Token::Not) | Some(Token::LParen) => {
                    // 隐式 AND
                    let right = self.parse_not()?;
                    left = QueryNode::And(Box::new(left), Box::new(right));
                }
                _ => break,
            }
        }
        Ok(left)
    }

    /// parse_not: NOT parse_not | parse_atom
    fn parse_not(&mut self) -> Result<QueryNode, QueryParseError> {
        if let Some(Token::Not) = self.peek() {
            self.next();
            let inner = self.parse_not()?;
            return Ok(QueryNode::Not(Box::new(inner)));
        }
        self.parse_atom()
    }

    /// parse_atom: (parse_or) | Word | Phrase
    fn parse_atom(&mut self) -> Result<QueryNode, QueryParseError> {
        match self.next() {
            Some(Token::LParen) => {
                let node = self.parse_or()?;
                match self.next() {
                    Some(Token::RParen) => Ok(node),
                    _ => Err(QueryParseError::InvalidSyntax("缺少右括号".to_owned())),
                }
            }
            Some(Token::Phrase(s)) => Ok(QueryNode::Phrase(s)),
            Some(Token::Word(w)) => parse_field_or_keyword(&w),
            Some(t) => Err(QueryParseError::InvalidSyntax(format!("意外的 token: {t:?}"))),
            None => Err(QueryParseError::InvalidSyntax("意外的输入结束".to_owned())),
        }
    }
}

/// 解析单个 word:可能是字段语法(ext:pdf)或普通关键字
fn parse_field_or_keyword(word: &str) -> Result<QueryNode, QueryParseError> {
    let lower = word.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("ext:") {
        let exts: Vec<String> = rest.split(',')
            .map(|s| s.trim().trim_start_matches('.').to_ascii_lowercase())
            .filter(|s| !s.is_empty())
            .collect();
        if exts.is_empty() {
            return Err(QueryParseError::InvalidSyntax("ext: 后需要扩展名".to_owned()));
        }
        return Ok(QueryNode::Extension(exts));
    }
    if let Some(rest) = lower.strip_prefix("size:") {
        return parse_size_field(rest);
    }
    if let Some(rest) = lower.strip_prefix("dm:") {
        return parse_date_field(rest);
    }
    if let Some(rest) = lower.strip_prefix("path:") {
        if rest.is_empty() {
            return Err(QueryParseError::InvalidSyntax("path: 后需要路径".to_owned()));
        }
        return Ok(QueryNode::Path(rest.to_owned()));
    }
    // 普通关键字
    Ok(QueryNode::Keywords(vec![word.to_owned()]))
}

fn parse_size_field(rest: &str) -> Result<QueryNode, QueryParseError> {
    // 范围: 1KB-10MB
    if let Some(idx) = rest.find('-') {
        let min = parse_size(&rest[..idx])?;
        let max = parse_size(&rest[idx + 1..])?;
        return Ok(QueryNode::SizeRange { min, max });
    }
    // 比较运算符
    if let Some(val) = rest.strip_prefix(">=") {
        return Ok(QueryNode::Size { op: CompareOp::Gte, value: parse_size(val)? });
    }
    if let Some(val) = rest.strip_prefix("<=") {
        return Ok(QueryNode::Size { op: CompareOp::Lte, value: parse_size(val)? });
    }
    if let Some(val) = rest.strip_prefix('>') {
        return Ok(QueryNode::Size { op: CompareOp::Gt, value: parse_size(val)? });
    }
    if let Some(val) = rest.strip_prefix('<') {
        return Ok(QueryNode::Size { op: CompareOp::Lt, value: parse_size(val)? });
    }
    if let Some(val) = rest.strip_prefix('=') {
        return Ok(QueryNode::Size { op: CompareOp::Eq, value: parse_size(val)? });
    }
    // 无运算符:等于
    Ok(QueryNode::Size { op: CompareOp::Eq, value: parse_size(rest)? })
}

fn parse_date_field(rest: &str) -> Result<QueryNode, QueryParseError> {
    // 范围: 2024-01-01..2024-12-31
    if let Some(idx) = rest.find("..") {
        let start = parse_date_value(&rest[..idx])?;
        let end = parse_date_value(&rest[idx + 2..])?;
        return Ok(QueryNode::DateRange { start, end });
    }
    // 比较运算符
    if let Some(val) = rest.strip_prefix(">=") {
        return Ok(QueryNode::Date { op: CompareOp::Gte, value: parse_date_value(val)? });
    }
    if let Some(val) = rest.strip_prefix("<=") {
        return Ok(QueryNode::Date { op: CompareOp::Lte, value: parse_date_value(val)? });
    }
    if let Some(val) = rest.strip_prefix('>') {
        return Ok(QueryNode::Date { op: CompareOp::Gt, value: parse_date_value(val)? });
    }
    if let Some(val) = rest.strip_prefix('<') {
        return Ok(QueryNode::Date { op: CompareOp::Lt, value: parse_date_value(val)? });
    }
    // 无运算符:等于(也用于 today/yesterday 等关键字)
    Ok(QueryNode::Date { op: CompareOp::Eq, value: parse_date_value(rest)? })
}

fn parse_date_value(s: &str) -> Result<DateValue, QueryParseError> {
    let lower = s.to_ascii_lowercase();
    match lower.as_str() {
        "today" => Ok(DateValue::Today),
        "yesterday" => Ok(DateValue::Yesterday),
        "this-week" | "thisweek" => Ok(DateValue::ThisWeek),
        "this-month" | "thismonth" => Ok(DateValue::ThisMonth),
        _ => {
            let date = NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| QueryParseError::InvalidDate(s.to_owned()))?;
            Ok(DateValue::Absolute(date))
        }
    }
}
```

同时删除旧的 `has_field_syntax`、`parse_phrase`、`parse_with_fields` 函数(已被新实现取代)。

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib search_index::query`
Expected: 所有测试(基础 + 字段 + 布尔)全部 PASS

- [ ] **Step 4: 提交**

```bash
git add src/search_index/query.rs
git commit -m "feat: 实现 DSL 字段语法与布尔运算解析"
```

---

### Task 6: 实现 AST → tantivy Query 编译器

**Files:**
- Modify: `src/search_index/query.rs`
- Modify: `src/search_index/schema.rs`

**Interfaces:**
- Produces:
  - `pub fn compile_query(node: &QueryNode, schema: &Schema) -> Result<Box<dyn tantivy::query::Query>, QueryParseError>`
  - `pub struct FieldId;` 含 `NAME`/`PATH`/`EXTENSION`/`SIZE`/`MODIFIED`/`MODIFIED_DAYS`/`IS_DIRECTORY`/`ROOT_KEY` 常量(在 schema.rs)

- [ ] **Step 1: 在 schema.rs 添加 FieldId 常量结构**

修改 `src/search_index/schema.rs`,在 `create_schema()` 之后添加:

```rust
use tantivy::schema::Field;

/// 编译期确定的字段 ID 常量。
///
/// 通过 [`Schema::get_field`] 在运行时解析,避免硬编码索引。
pub struct FieldId;

impl FieldId {
    pub fn name(schema: &Schema) -> Field {
        schema.get_field("name").expect("schema 必须包含 name 字段")
    }
    pub fn path(schema: &Schema) -> Field {
        schema.get_field("path").expect("schema 必须包含 path 字段")
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
```

- [ ] **Step 2: 编写编译器的失败测试**

在 `src/search_index/query.rs` 的 `#[cfg(test)] mod tests` 中追加:

```rust
    use tantivy::schema::Schema;
    use crate::search_index::schema::create_schema;

    fn test_schema() -> Schema {
        create_schema()
    }

    #[test]
    fn compile_empty_query_matches_all() {
        let schema = test_schema();
        let node = parse_query("").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        // 查询对象应可创建(AllQuery)
        assert!(format!("{query:?}").contains("All"));
    }

    #[test]
    fn compile_keywords_query() {
        let schema = test_schema();
        let node = parse_query("report").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        let debug = format!("{query:?}");
        // 应包含 BooleanQuery 或 TermQuery
        assert!(debug.contains("Term") || debug.contains("Boolean") || debug.contains("Fuzzy"));
    }

    #[test]
    fn compile_extension_query() {
        let schema = test_schema();
        let node = parse_query("ext:pdf").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Term") || format!("{query:?}").contains("Boolean"));
    }

    #[test]
    fn compile_size_range_query() {
        let schema = test_schema();
        let node = parse_query("size:1KB-10MB").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Range"));
    }

    #[test]
    fn compile_regex_query() {
        let schema = test_schema();
        let node = parse_query("regex:^Report").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Regex"));
    }

    #[test]
    fn compile_boolean_and_query() {
        let schema = test_schema();
        let node = parse_query("report AND pdf").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Boolean"));
    }

    #[test]
    fn compile_boolean_not_query() {
        let schema = test_schema();
        let node = parse_query("NOT tmp").unwrap();
        let query = compile_query(&node, &schema).unwrap();
        assert!(format!("{query:?}").contains("Boolean"));
    }
```

- [ ] **Step 3: 实现 compile_query 函数**

在 `src/search_index/query.rs` 顶部添加导入:

```rust
use tantivy::schema::Schema;
use tantivy::query::{
    BooleanQuery, Occur, Query, RangeQuery, RegexQuery, TermQuery, AllQuery,
    FuzzyTermQuery,
};
use tantivy::Term;
use crate::search_index::schema::FieldId;
```

在 `parse_query` 函数之后添加编译器实现:

```rust
/// 将 AST 编译为 tantivy Query 对象。
pub fn compile_query(
    node: &QueryNode,
    schema: &Schema,
) -> Result<Box<dyn Query>, QueryParseError> {
    match node {
        QueryNode::Empty => Ok(Box::new(AllQuery)),
        QueryNode::Keywords(keywords) => {
            // 多关键字 AND,每个关键字在 name/path 中 OR(前缀匹配)
            let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
            for kw in keywords {
                let term_name = Term::from_field_text(FieldId::name(schema), kw);
                let term_path = Term::from_field_text(FieldId::path(schema), kw);
                let or_query = BooleanQuery::new(vec![
                    (Occur::Should, Box::new(FuzzyTermQuery::new(term_name, true, true))
                        as Box<dyn Query>),
                    (Occur::Should, Box::new(FuzzyTermQuery::new(term_path, true, true))
                        as Box<dyn Query>),
                ]);
                clauses.push((Occur::Must, Box::new(or_query)));
            }
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
        QueryNode::Phrase(s) => {
            // 短语:在 name 字段做 term 查询
            let term = Term::from_field_text(FieldId::name(schema), s);
            Ok(Box::new(TermQuery::new(term, Default::default())))
        }
        QueryNode::Extension(exts) => {
            let clauses: Vec<(Occur, Box<dyn Query>)> = exts
                .iter()
                .map(|e| {
                    let term = Term::from_field_text(FieldId::extension(schema), e);
                    (Occur::Should, Box::new(TermQuery::new(term, Default::default()))
                        as Box<dyn Query>)
                })
                .collect();
            Ok(Box::new(BooleanQuery::new(clauses)))
        }
        QueryNode::Size { op, value } => {
            let field = FieldId::size(schema);
            let range = match op {
                CompareOp::Gt => std::ops::Bound::Excluded(*value)..std::ops::Bound::Unbounded,
                CompareOp::Lt => std::ops::Bound::Unbounded..std::ops::Bound::Excluded(*value),
                CompareOp::Gte => std::ops::Bound::Included(*value)..std::ops::Bound::Unbounded,
                CompareOp::Lte => std::ops::Bound::Unbounded..std::ops::Bound::Included(*value),
                CompareOp::Eq => std::ops::Bound::Included(*value)..std::ops::Bound::Included(*value),
            };
            Ok(Box::new(RangeQuery::new_u64_bounds(field, range)))
        }
        QueryNode::SizeRange { min, max } => {
            let field = FieldId::size(schema);
            let range = std::ops::Bound::Included(*min)..std::ops::Bound::Included(*max);
            Ok(Box::new(RangeQuery::new_u64_bounds(field, range)))
        }
        QueryNode::Date { op, value } => {
            let field = FieldId::modified_days(schema);
            let day = date_value_to_days(value);
            let range = match op {
                CompareOp::Gt => std::ops::Bound::Excluded(day)..std::ops::Bound::Unbounded,
                CompareOp::Lt => std::ops::Bound::Unbounded..std::ops::Bound::Excluded(day),
                CompareOp::Gte => std::ops::Bound::Included(day)..std::ops::Bound::Unbounded,
                CompareOp::Lte => std::ops::Bound::Unbounded..std::ops::Bound::Included(day),
                CompareOp::Eq => std::ops::Bound::Included(day)..std::ops::Bound::Included(day),
            };
            Ok(Box::new(RangeQuery::new_u64_bounds(field, range)))
        }
        QueryNode::DateRange { start, end } => {
            let field = FieldId::modified_days(schema);
            let start_day = date_value_to_days(start);
            let end_day = date_value_to_days(end);
            let range = std::ops::Bound::Included(start_day)..std::ops::Bound::Included(end_day);
            Ok(Box::new(RangeQuery::new_u64_bounds(field, range)))
        }
        QueryNode::Path(s) => {
            // 路径限定:在 path 字段做 term 查询
            let term = Term::from_field_text(FieldId::path(schema), s);
            Ok(Box::new(TermQuery::new(term, Default::default())))
        }
        QueryNode::Regex(pattern) => {
            Ok(Box::new(RegexQuery::from_pattern(pattern, FieldId::name(schema))
                .map_err(|e| QueryParseError::InvalidRegex(e.to_string()))?))
        }
        QueryNode::And(left, right) => {
            let l = compile_query(left, schema)?;
            let r = compile_query(right, schema)?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, l),
                (Occur::Must, r),
            ])))
        }
        QueryNode::Or(left, right) => {
            let l = compile_query(left, schema)?;
            let r = compile_query(right, schema)?;
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Should, l),
                (Occur::Should, r),
            ])))
        }
        QueryNode::Not(inner) => {
            let sub = compile_query(inner, schema)?;
            // NOT = Must(AllQuery) + MustNot(sub)
            Ok(Box::new(BooleanQuery::new(vec![
                (Occur::Must, Box::new(AllQuery)),
                (Occur::MustNot, sub),
            ])))
        }
    }
}

/// 将 DateValue 转换为自 Unix 纪元以来的天数。
fn date_value_to_days(value: &DateValue) -> u64 {
    let today = chrono::Local::now().date_naive();
    let date = match value {
        DateValue::Today => today,
        DateValue::Yesterday => today - chrono::Duration::days(1),
        DateValue::ThisWeek => today - chrono::Duration::days(today.weekday().num_days_from_monday() as i64),
        DateValue::ThisMonth => chrono::NaiveDate::from_ymd_opt(today.year(), today.month(), 1).unwrap_or(today),
        DateValue::Absolute(d) => *d,
    };
    (date.and_hms_opt(0, 0, 0).unwrap().and_utc().timestamp() / 86400) as u64
}

// 在文件顶部 use 语句中补充 chrono trait
```

在文件顶部 use 块添加:

```rust
use chrono::{Datelike, Timelike};
```

- [ ] **Step 4: 运行测试验证通过**

Run: `cargo test --lib search_index::query`
Expected: 所有测试(解析 + 编译)全部 PASS

- [ ] **Step 5: 提交**

```bash
git add src/search_index/query.rs src/search_index/schema.rs
git commit -m "feat: 实现 AST 到 tantivy Query 的编译器"
```

---

### Task 7: 实现 SearchIndexer 核心类

**Files:**
- Create: `src/search_index/indexer.rs`
- Modify: `src/search_index/mod.rs`

**Interfaces:**
- Consumes: `schema::create_schema/index_directory/FieldId`, `frn_db::*`, `query::parse_query/compile_query`, `model::ScanStats/FileRecord/DirectoryRecord`
- Produces:
  - `pub struct SearchIndexer { ... }`
  - `pub fn open() -> anyhow::Result<SearchIndexer>`
  - `pub fn build_from_scan(&self, stats: &ScanStats, root_key: &str, progress: impl Fn(u64, u64)) -> anyhow::Result<u64>`
  - `pub fn search(&self, root_key: &str, query: &str, limit: usize) -> anyhow::Result<Vec<FileSearchResult>>`
  - `pub fn delete_by_path(&self, path: &str) -> anyhow::Result<()>`
  - `pub fn upsert_entry(&self, root_key: &str, name: &str, path: &str, parent_path: &str, extension: Option<&str>, size: u64, modified: Option<u64>, is_directory: bool) -> anyhow::Result<()>`
  - `pub fn handle_usn_event(&self, event: UsnEvent, root_key: &str) -> anyhow::Result<()>`
  - `pub fn index_count(&self, root_key: &str) -> anyhow::Result<u64>`
  - `pub fn index_exists(&self, root_key: &str) -> anyhow::Result<bool>`

- [ ] **Step 1: 编写 indexer.rs 失败测试**

创建 `src/search_index/indexer.rs`:

```rust
//! tantivy 搜索索引核心管理器。
//!
//! 协调 schema、FRN 映射、查询编译,提供索引构建、搜索、增量更新能力。

use std::path::Path;
use anyhow::{Context, Result};
use tantivy::{
    Index, IndexWriter, IndexReader, ReloadPolicy, doc,
    query::Query,
    schema::Schema,
    DocAddress, Term,
};
use chrono::{Datelike, Timelike};

use crate::model::ScanStats;
use crate::search_index::schema::{create_schema, index_directory, FieldId};
use crate::search_index::frn_db;
use crate::search_index::query::{parse_query, compile_query};
use crate::search_index::usn_journal::UsnEvent;

/// 搜索结果(与旧 API 兼容的字段结构)
#[derive(Debug, Clone)]
pub struct FileSearchResult {
    pub name: String,
    pub path: String,
    pub parent_path: String,
    pub extension: String,
    pub size: u64,
    pub modified: Option<u64>,
    pub is_directory: bool,
}

/// tantivy 搜索索引管理器。
pub struct SearchIndexer {
    index: Index,
    writer: IndexWriter,
    reader: IndexReader,
    schema: Schema,
}

impl SearchIndexer {
    /// 打开或创建索引。
    pub fn open() -> Result<Self> {
        let dir = index_directory().context("获取索引目录失败")?;
        let schema = create_schema();
        let index = Index::open_or_create(dir, schema.clone())
            .context("打开 tantivy 索引失败")?;
        let writer = index.writer(50_000_000).context("创建 IndexWriter 失败")?;
        let reader = index
            .reader_builder()
            .reload_policy(ReloadPolicy::OnCommitWithDelay)
            .try_into()
            .context("创建 IndexReader 失败")?;
        Ok(Self { index, writer, reader, schema })
    }

    /// 从扫描结果批量构建索引,返回索引条目数。
    pub fn build_from_scan(
        &self,
        stats: &ScanStats,
        root_key: &str,
        progress: impl Fn(u64, u64),
    ) -> Result<u64> {
        // 清空旧数据
        self.writer.delete_all_documents()?;

        // 清空 FRN 映射
        let conn = frn_db::open_frn_db()?;
        frn_db::clear_frn_for_root(&conn, root_key)?;

        let dir_count = stats.directory_tree.as_ref().map(|t| t.nodes.len()).unwrap_or(0);
        let file_count = stats.all_files.len();
        let total = (dir_count + file_count) as u64;
        let mut processed = 0u64;

        // 写入目录
        if let Some(tree) = &stats.directory_tree {
            for node in &tree.nodes {
                let rec = &node.record;
                let name = rec.path.file_name()
                    .map(|n| n.to_string_lossy().to_string())
                    .unwrap_or_default();
                let parent_path = rec.path.parent()
                    .map(|p| p.to_string_lossy().to_string())
                    .unwrap_or_default();
                let ext = rec.path.extension()
                    .and_then(|e| e.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                let doc = self.make_document(
                    root_key, &name, &rec.path.display().to_string(),
                    &parent_path, &ext, rec.total_size, None, true, "",
                );
                self.writer.add_document(doc)?;
                processed += 1;
                if processed % 1000 == 0 {
                    progress(processed, total);
                }
            }
        }

        // 写入文件
        for file in &stats.all_files {
            let name = file.path.file_name()
                .map(|n| n.to_string_lossy().to_string())
                .unwrap_or_default();
            let parent_path = file.path.parent()
                .map(|p| p.to_string_lossy().to_string())
                .unwrap_or_default();
            let modified = file.modified
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_secs());
            let doc = self.make_document(
                root_key, &name, &file.path.display().to_string(),
                &parent_path, &file.extension, file.size, modified, false, "",
            );
            self.writer.add_document(doc)?;
            processed += 1;
            if processed % 1000 == 0 {
                progress(processed, total);
            }
        }

        self.writer.commit()?;
        progress(total, total);
        Ok(processed)
    }

    /// 执行搜索。
    pub fn search(
        &self,
        root_key: &str,
        query: &str,
        limit: usize,
    ) -> Result<Vec<FileSearchResult>> {
        let node = parse_query(query).context("解析查询失败")?;
        let compiled = compile_query(&node, &self.schema)?;

        // 组合 root_key 过滤
        let root_term = Term::from_field_text(FieldId::root_key(&self.schema), root_key);
        let root_query = tantivy::query::TermQuery::new(root_term, Default::default());
        let final_query = tantivy::query::BooleanQuery::new(vec![
            (tantivy::query::Occur::Must, Box::new(root_query)),
            (tantivy::query::Occur::Must, compiled),
        ]);

        let searcher = self.reader.searcher();
        let top_docs = searcher.search(&final_query, &tantivy::collector::TopDocs::with_limit(limit))?;

        let mut results = Vec::with_capacity(top_docs.len());
        for (_score, addr) in top_docs {
            let doc = searcher.doc(addr)?;
            results.push(self.doc_to_result(&doc));
        }
        Ok(results)
    }

    /// 按路径删除单个文档。
    pub fn delete_by_path(&self, path: &str) -> Result<()> {
        let term = Term::from_field_text(FieldId::path(&self.schema), path);
        self.writer.delete_term(term);
        self.writer.commit()?;
        Ok(())
    }

    /// 插入或更新单个条目。
    pub fn upsert_entry(
        &self,
        root_key: &str,
        name: &str,
        path: &str,
        parent_path: &str,
        extension: Option<&str>,
        size: u64,
        modified: Option<u64>,
        is_directory: bool,
    ) -> Result<()> {
        // 先删除旧的同路径文档
        let term = Term::from_field_text(FieldId::path(&self.schema), path);
        self.writer.delete_term(term);
        let doc = self.make_document(
            root_key, name, path, parent_path,
            extension.unwrap_or(""), size, modified, is_directory, "",
        );
        self.writer.add_document(doc)?;
        self.writer.commit()?;
        Ok(())
    }

    /// 处理 USN Journal 事件,增量更新索引。
    pub fn handle_usn_event(&self, event: UsnEvent, root_key: &str) -> Result<()> {
        let conn = frn_db::open_frn_db()?;
        match event {
            UsnEvent::FileCreated { frn, parent_frn, file_name, is_directory } => {
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .map(|p| p)
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, is_directory)?;
                frn_db::upsert_frn_path(&conn, root_key, &frn, &path, Some(&parent_frn), is_directory)?;
            }
            UsnEvent::FileDeleted { frn, .. } => {
                if let Some(path) = frn_db::lookup_frn_path(&conn, root_key, &frn)? {
                    self.delete_by_path(&path)?;
                }
                frn_db::delete_frn_path(&conn, root_key, &frn)?;
            }
            UsnEvent::FileModified { frn, parent_frn, file_name } => {
                // 删除后重建
                if let Some(old_path) = frn_db::lookup_frn_path(&conn, root_key, &frn)? {
                    self.delete_by_path(&old_path)?;
                }
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, false)?;
                frn_db::upsert_frn_path(&conn, root_key, &frn, &path, Some(&parent_frn), false)?;
            }
            UsnEvent::FileRenamed { old_frn, new_frn, parent_frn, file_name } => {
                if let Some(old_path) = frn_db::lookup_frn_path(&conn, root_key, &old_frn)? {
                    self.delete_by_path(&old_path)?;
                }
                frn_db::delete_frn_path(&conn, root_key, &old_frn)?;
                let parent_path = frn_db::resolve_path_from_frn(&conn, root_key, &parent_frn, &file_name)?
                    .unwrap_or_else(|| format!("FRN:{}", parent_frn));
                let path = Path::new(&parent_path).join(&file_name)
                    .to_string_lossy().to_string();
                let ext = Path::new(&file_name).extension()
                    .and_then(|e| e.to_str()).unwrap_or("");
                self.upsert_entry(root_key, &file_name, &path, &parent_path,
                    Some(ext), 0, None, false)?;
                frn_db::upsert_frn_path(&conn, root_key, &new_frn, &path, Some(&parent_frn), false)?;
            }
        }
        Ok(())
    }

    /// 返回指定 root_key 的索引条目数。
    pub fn index_count(&self, root_key: &str) -> Result<u64> {
        let root_term = Term::from_field_text(FieldId::root_key(&self.schema), root_key);
        let query = tantivy::query::TermQuery::new(root_term, Default::default());
        let searcher = self.reader.searcher();
        let count = searcher.count(&query)?;
        Ok(count as u64)
    }

    /// 检查指定 root_key 是否有索引。
    pub fn index_exists(&self, root_key: &str) -> Result<bool> {
        Ok(self.index_count(root_key)? > 0)
    }

    fn make_document(
        &self,
        root_key: &str,
        name: &str,
        path: &str,
        parent_path: &str,
        extension: &str,
        size: u64,
        modified: Option<u64>,
        is_directory: bool,
        frn: &str,
    ) -> tantivy::Document {
        let modified_days = modified
            .map(|m| m / 86400)
            .unwrap_or(0);
        doc!(
            FieldId::name(&self.schema) => name,
            FieldId::path(&self.schema) => path,
            FieldId::parent_path(&self.schema) => parent_path,
            FieldId::extension(&self.schema) => extension,
            FieldId::size(&self.schema) => size,
            FieldId::modified(&self.schema) => modified.unwrap_or(0),
            FieldId::modified_days(&self.schema) => modified_days,
            FieldId::is_directory(&self.schema) => is_directory,
            FieldId::root_key(&self.schema) => root_key,
            FieldId::frn(&self.schema) => frn,
        )
    }

    fn doc_to_result(&self, doc: &tantivy::Document) -> FileSearchResult {
        let get_str = |field| doc.get_first(field).and_then(|v| v.as_text()).unwrap_or("").to_owned();
        let get_u64 = |field| doc.get_first(field).and_then(|v| v.as_u64()).unwrap_or(0);
        let get_bool = |field| doc.get_first(field).and_then(|v| v.as_bool()).unwrap_or(false);
        let modified = doc.get_first(FieldId::modified(&self.schema))
            .and_then(|v| v.as_u64())
            .filter(|&v| v > 0);
        FileSearchResult {
            name: get_str(FieldId::name(&self.schema)),
            path: get_str(FieldId::path(&self.schema)),
            parent_path: get_str(FieldId::parent_path(&self.schema)),
            extension: get_str(FieldId::extension(&self.schema)),
            size: get_u64(FieldId::size(&self.schema)),
            modified,
            is_directory: get_bool(FieldId::is_directory(&self.schema)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ScanStats, FileRecord};
    use std::path::PathBuf;

    fn setup_temp_index() -> SearchIndexer {
        let tmp = std::env::temp_dir().join(format!(
            "cdrive-index-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
        ));
        std::fs::create_dir_all(&tmp).unwrap();
        std::env::set_var("LOCALAPPDATA", &tmp);
        SearchIndexer::open().expect("应能打开索引")
    }

    fn make_test_stats() -> ScanStats {
        let mut stats = ScanStats::default();
        stats.root = PathBuf::from("C:\\test");
        stats.all_files = vec![
            FileRecord {
                path: PathBuf::from("C:\\test\\report.pdf"),
                size: 1024 * 1024,
                modified: Some(std::time::SystemTime::now()),
                extension: ".pdf".to_owned(),
            },
            FileRecord {
                path: PathBuf::from("C:\\test\\data.xlsx"),
                size: 2048,
                modified: Some(std::time::SystemTime::now()),
                extension: ".xlsx".to_owned(),
            },
        ];
        stats.file_count = 2;
        stats
    }

    #[test]
    fn build_from_scan_indexes_files() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        let count = indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        assert_eq!(count, 2);
    }

    #[test]
    fn search_by_keyword_returns_matches() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();

        let results = indexer.search("c:/test", "report", 10).unwrap();
        assert!(!results.is_empty());
        assert!(results.iter().any(|r| r.name.contains("report")));
    }

    #[test]
    fn search_by_extension_returns_matches() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();

        let results = indexer.search("c:/test", "ext:pdf", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].name.ends_with(".pdf"));
    }

    #[test]
    fn search_by_size_range() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();

        // report.pdf = 1MB, data.xlsx = 2KB
        let results = indexer.search("c:/test", "size:>500KB", 10).unwrap();
        assert_eq!(results.len(), 1);
        assert!(results[0].name.contains("report"));
    }

    #[test]
    fn search_by_regex() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();

        let results = indexer.search("c:/test", "regex:^report", 10).unwrap();
        assert!(!results.is_empty());
    }

    #[test]
    fn delete_by_path_removes_document() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();

        indexer.delete_by_path("C:\\test\\report.pdf").unwrap();
        let results = indexer.search("c:/test", "report", 10).unwrap();
        assert!(results.is_empty());
    }

    #[test]
    fn upsert_entry_adds_new_document() {
        let indexer = setup_temp_index();
        indexer.upsert_entry(
            "c:/test", "new.txt", "C:\\test\\new.txt",
            "C:\\test", Some("txt"), 100, None, false,
        ).unwrap();
        let results = indexer.search("c:/test", "new", 10).unwrap();
        assert_eq!(results.len(), 1);
    }

    #[test]
    fn index_count_returns_correct_number() {
        let indexer = setup_temp_index();
        let stats = make_test_stats();
        indexer.build_from_scan(&stats, "c:/test", |_, _| {}).unwrap();
        assert_eq!(indexer.index_count("c:/test").unwrap(), 2);
    }
}
```

- [ ] **Step 2: 在 mod.rs 注册 indexer 模块**

修改 `src/search_index/mod.rs`,在 `mod query;` 之后添加 `mod indexer;`:

```rust
mod schema;
mod frn_db;
mod query;
mod indexer;
mod db;
mod usn_journal;
mod worker;
```

在 query re-export 之后添加:

```rust
#[allow(unused_imports)]
pub use indexer::{SearchIndexer, FileSearchResult as TantivyFileSearchResult};
```

- [ ] **Step 3: 运行测试验证通过**

Run: `cargo test --lib search_index::indexer`
Expected: 8 个测试全部 PASS

- [ ] **Step 4: 提交**

```bash
git add src/search_index/indexer.rs src/search_index/mod.rs
git commit -m "feat: 实现 SearchIndexer 核心:构建/搜索/增量更新"
```

---

### Task 8: 重写 worker.rs 适配新 indexer

**Files:**
- Rewrite: `src/search_index/worker.rs`
- Modify: `src/search_index/mod.rs`

**Interfaces:**
- Consumes: `indexer::SearchIndexer`, `usn_journal::UsnEvent`
- Produces: 保持与旧 API 兼容的 `SearchIndexEvent`/`SearchIndexHandle`/`SearchHandle`/`SearchResult`/`spawn_build_index`/`spawn_search`/`spawn_usn_index_listener`

- [ ] **Step 1: 重写 worker.rs**

完全替换 `src/search_index/worker.rs` 内容:

```rust
//! 后台索引构建/搜索线程,适配 tantivy SearchIndexer。

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crossbeam_channel::{Receiver, unbounded};

use crate::model::ScanStats;
use crate::search_index::indexer::{FileSearchResult, SearchIndexer};
use crate::search_index::usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig};

/// 索引构建事件。
#[derive(Debug, Clone)]
pub enum SearchIndexEvent {
    Building { root_key: String, total_files: u64 },
    Progress { root_key: String, processed: u64 },
    Finished { root_key: String, total_entries: u64 },
    Updated { root_key: String, changes: u64 },
    Error(String),
}

/// 索引构建 handle。
pub struct SearchIndexHandle {
    pub receiver: Receiver<SearchIndexEvent>,
    cancel_flag: Arc<AtomicBool>,
}

impl SearchIndexHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
    pub fn is_cancelled(&self) -> bool {
        self.cancel_flag.load(Ordering::Relaxed)
    }
}

/// 异步搜索结果。
#[derive(Debug, Clone)]
pub enum SearchResult {
    Ok { query: String, results: Vec<FileSearchResult> },
    Error(String),
}

/// 搜索 handle。
pub struct SearchHandle {
    pub receiver: Receiver<SearchResult>,
    cancel_flag: Arc<AtomicBool>,
}

impl SearchHandle {
    pub fn cancel(&self) {
        self.cancel_flag.store(true, Ordering::Relaxed);
    }
}

/// 从扫描结果后台构建索引。
pub fn spawn_build_index(stats: Arc<ScanStats>) -> SearchIndexHandle {
    let (sender, receiver) = unbounded::<SearchIndexEvent>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }
        let root_key = crate::search_index::root_key(&stats.root);
        let total_files = stats.file_count;

        let _ = sender.send(SearchIndexEvent::Building {
            root_key: root_key.clone(),
            total_files,
        });

        let indexer = match SearchIndexer::open() {
            Ok(i) => i,
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!("打开索引失败: {e}")));
                return;
            }
        };

        let sender_for_progress = sender.clone();
        let root_key_for_progress = root_key.clone();
        match indexer.build_from_scan(&stats, &root_key, move |processed, total| {
            let _ = sender_for_progress.send(SearchIndexEvent::Progress {
                root_key: root_key_for_progress.clone(),
                processed,
            });
            let _ = total; // 总数在 Building 事件中已发送
        }) {
            Ok(total) => {
                let _ = sender.send(SearchIndexEvent::Finished {
                    root_key,
                    total_entries: total,
                });
            }
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!("构建索引失败: {e}")));
            }
        }
    });

    SearchIndexHandle { receiver, cancel_flag }
}

/// 启动 USN Journal 监听器,增量更新索引。
pub fn spawn_usn_index_listener(root_key: String, drive_letter: char) -> SearchIndexHandle {
    let (sender, receiver) = unbounded::<SearchIndexEvent>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        let config = UsnListenerConfig {
            drive_letter,
            cancel_flag: worker_cancel.clone(),
        };
        let handle = spawn_usn_listener(config);
        let indexer = match SearchIndexer::open() {
            Ok(i) => i,
            Err(e) => {
                let _ = sender.send(SearchIndexEvent::Error(format!("打开索引失败: {e}")));
                return;
            }
        };
        let mut total_changes: u64 = 0;

        while let Ok(event) = handle.receiver.recv_timeout(std::time::Duration::from_millis(500)) {
            if worker_cancel.load(Ordering::Relaxed) {
                break;
            }
            if let Err(e) = indexer.handle_usn_event(event, &root_key) {
                eprintln!("USN 索引更新错误: {e}");
            }
            total_changes += 1;
            if total_changes.is_multiple_of(100) {
                let _ = sender.send(SearchIndexEvent::Updated {
                    root_key: root_key.clone(),
                    changes: total_changes,
                });
            }
        }
    });

    SearchIndexHandle { receiver, cancel_flag }
}

/// 异步搜索。
pub fn spawn_search(root_key: String, query: String, limit: usize) -> SearchHandle {
    let (sender, receiver) = unbounded::<SearchResult>();
    let cancel_flag = Arc::new(AtomicBool::new(false));
    let worker_cancel = Arc::clone(&cancel_flag);

    thread::spawn(move || {
        if worker_cancel.load(Ordering::Relaxed) {
            return;
        }
        let indexer = match SearchIndexer::open() {
            Ok(i) => i,
            Err(e) => {
                let _ = sender.send(SearchResult::Error(format!("打开索引失败: {e}")));
                return;
            }
        };
        match indexer.search(&root_key, &query, limit) {
            Ok(results) => {
                let _ = sender.send(SearchResult::Ok { query, results });
            }
            Err(e) => {
                let _ = sender.send(SearchResult::Error(e.to_string()));
            }
        }
    });

    SearchHandle { receiver, cancel_flag }
}
```

- [ ] **Step 2: 更新 mod.rs 导出**

修改 `src/search_index/mod.rs` 的 re-export 块。保留 `root_key` 函数(从 db.rs 暂时复用,直到 db.rs 删除)。更新 worker re-export:

```rust
mod schema;
mod frn_db;
mod query;
mod indexer;
mod usn_journal;
mod worker;
// db.rs 仍保留 root_key 函数,其他逐步移除
mod db;

#[allow(unused_imports)]
pub use schema::{create_schema, index_directory, frn_db_path, FieldId};
#[allow(unused_imports)]
pub use frn_db::{
    open_frn_db, upsert_frn_path, lookup_frn_path,
    resolve_path_from_frn, delete_frn_path, clear_frn_for_root,
};
#[allow(unused_imports)]
pub use query::{parse_query, parse_size, compile_query, QueryNode, CompareOp, DateValue, QueryParseError};
#[allow(unused_imports)]
pub use indexer::{SearchIndexer, FileSearchResult};
#[allow(unused_imports)]
pub use usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig, UsnListenerHandle};
#[allow(unused_imports)]
pub use worker::{
    spawn_build_index, spawn_search, spawn_usn_index_listener, SearchHandle,
    SearchIndexEvent, SearchIndexHandle, SearchResult,
};
// 保留 root_key 直到 app.rs 迁移完成
#[allow(unused_imports)]
pub use db::root_key;
```

- [ ] **Step 3: 验证编译通过**

Run: `cargo build`
Expected: 编译成功。可能有 dead_code 警告(db.rs 中旧函数),后续任务清理。

- [ ] **Step 4: 提交**

```bash
git add src/search_index/worker.rs src/search_index/mod.rs
git commit -m "feat: 重写 worker 适配 tantivy SearchIndexer"
```

---

### Task 9: 删除旧 db.rs 并迁移 root_key

**Files:**
- Delete: `src/search_index/db.rs`
- Modify: `src/search_index/mod.rs`, `src/search_index/indexer.rs`

- [ ] **Step 1: 将 root_key 函数迁移到 indexer.rs**

在 `src/search_index/indexer.rs` 末尾(`#[cfg(test)]` 之前)添加:

```rust
/// 将路径规范化为 root key(小写、正斜杠)。
pub fn root_key(root: &std::path::Path) -> String {
    let canonical = std::fs::canonicalize(root)
        .unwrap_or_else(|_| root.to_path_buf());
    canonical.to_string_lossy().to_lowercase().replace("\\", "/")
}
```

- [ ] **Step 2: 删除 db.rs**

```bash
rm src/search_index/db.rs
```

- [ ] **Step 3: 更新 mod.rs 移除 db 模块**

修改 `src/search_index/mod.rs`,删除 `mod db;` 行,并把 `pub use db::root_key;` 改为:

```rust
#[allow(unused_imports)]
pub use indexer::{SearchIndexer, FileSearchResult, root_key};
```

- [ ] **Step 4: 验证编译与测试通过**

Run: `cargo build && cargo test --lib search_index`
Expected: 编译成功,所有 search_index 测试通过

- [ ] **Step 5: 提交**

```bash
git add -A src/search_index/
git commit -m "refactor: 删除旧 SQLite FTS5 db.rs,迁移 root_key 到 indexer"
```

---

### Task 10: 重构搜索 UI - 视图模式切换与紧凑列表

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Produces: 新增 `SearchViewMode` 枚举,搜索面板支持表格/紧凑列表切换

- [ ] **Step 1: 添加 SearchViewMode 枚举与状态字段**

在 `src/app.rs` 中,找到 `enum ResultTab` 定义(约第 152 行),在其后添加:

```rust
/// 搜索结果视图模式。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SearchViewMode {
    /// 表格视图(名称/路径/大小/类型列)
    Table,
    /// 紧凑列表(单行,虚拟滚动)
    Compact,
}
```

在 `CDriveManagerApp` 结构体中(约第 141-149 行搜索相关字段处),在 `search_last_input` 之后添加:

```rust
    /// 搜索结果视图模式(表格/紧凑列表)
    search_view_mode: SearchViewMode,
```

在 `Default` / `new()` 实现中(约第 528-532 行)添加初始化:

```rust
            search_view_mode: SearchViewMode::Table,
```

- [ ] **Step 2: 在搜索面板添加视图切换按钮**

找到 `draw_search_tab` 方法中搜索框的 horizontal 布局(约第 4284-4302 行)。在"清空"按钮之后、"搜索"按钮之前插入视图切换 ComboBox:

将原代码:

```rust
            if !self.search_query.is_empty() {
                if ui.button("清空").clicked() {
                    self.search_query.clear();
                    self.search_results.clear();
                    self.search_page = 0;
                }
            }
            if ui.button("搜索").clicked() {
                self.perform_search();
                self.search_page = 0;
            }
```

替换为:

```rust
            if !self.search_query.is_empty() {
                if ui.button("清空").clicked() {
                    self.search_query.clear();
                    self.search_results.clear();
                    self.search_page = 0;
                }
            }
            if ui.button("搜索").clicked() {
                self.perform_search();
                self.search_page = 0;
            }
            ui.separator();
            egui::ComboBox::from_id_salt("search_view_mode")
                .selected_text(match self.search_view_mode {
                    SearchViewMode::Table => "📋 表格",
                    SearchViewMode::Compact => "📝 紧凑",
                })
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.search_view_mode, SearchViewMode::Table, "📋 表格视图");
                    ui.selectable_value(&mut self.search_view_mode, SearchViewMode::Compact, "📝 紧凑列表");
                });
```

- [ ] **Step 3: 实现紧凑列表视图**

在 `draw_search_tab` 方法中,找到现有表格渲染逻辑(约第 4359 行 `egui::ScrollArea::vertical()`)。将其包裹在视图模式分支中。

找到:

```rust
        egui::ScrollArea::vertical()
            .id_salt("search_results_scroll")
            .show(ui, |ui| {
                egui::Grid::new("search_results_grid")
                // ... 表格渲染 ...
            });
```

替换为:

```rust
        match self.search_view_mode {
            SearchViewMode::Table => {
                // 原有表格渲染
                egui::ScrollArea::vertical()
                    .id_salt("search_results_scroll")
                    .show(ui, |ui| {
                        egui::Grid::new("search_results_grid")
                            .striped(true)
                            .num_columns(4)
                            .spacing([12.0, 4.0])
                            .show(ui, |ui| {
                                ui.label(RichText::new("名称").strong());
                                ui.label(RichText::new("路径").strong());
                                ui.label(RichText::new("大小").strong());
                                ui.label(RichText::new("类型").strong());
                                ui.end_row();

                                for result in page_results {
                                    let query_lower = self.search_query.to_ascii_lowercase();
                                    ui.horizontal(|ui| {
                                        let name_label = if result.name.to_ascii_lowercase().contains(&query_lower) {
                                            RichText::new(&result.name)
                                                .strong()
                                                .color(egui::Color32::from_rgb(100, 180, 255))
                                        } else {
                                            RichText::new(&result.name)
                                        };
                                        ui.add(egui::Label::new(name_label).sense(egui::Sense::click()));
                                    });
                                    ui.label(result.parent_path.clone()).on_hover_text(&result.path);
                                    ui.label(format::bytes(result.size));
                                    if result.is_directory {
                                        ui.label("📁 目录");
                                    } else if !result.extension.is_empty() {
                                        ui.label(format!("📄 .{}", result.extension));
                                    } else {
                                        ui.label("📄 文件");
                                    }
                                    ui.end_row();
                                }
                            });
                    });
            }
            SearchViewMode::Compact => {
                // 紧凑列表:虚拟滚动
                let row_height = 20.0;
                egui::ScrollArea::vertical()
                    .id_salt("search_compact_scroll")
                    .show_rows(ui, row_height, page_results.len(), |ui, row_range| {
                        for i in row_range {
                            let result = &page_results[i];
                            ui.horizontal(|ui| {
                                let icon = if result.is_directory { "📁" } else { "📄" };
                                ui.label(format!("{icon} {}", result.name));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new(format::bytes(result.size)).weak());
                                    ui.add_space(60.0);
                                    let display_path = if result.parent_path.len() > 50 {
                                        format!("...{}", &result.parent_path[result.parent_path.len().saturating_sub(47)..])
                                    } else {
                                        result.parent_path.clone()
                                    };
                                    ui.label(RichText::new(display_path).weak().small());
                                });
                            });
                        }
                    });
            }
        }
```

- [ ] **Step 4: 验证编译通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 5: 提交**

```bash
git add src/app.rs
git commit -m "feat: 搜索面板支持表格/紧凑列表视图切换"
```

---

### Task 11: 实现右键操作菜单

**Files:**
- Modify: `src/app.rs`

**Interfaces:**
- Produces: 搜索结果右键菜单,支持在资源管理器显示/打开/复制路径/查看属性/删除/加入清理队列

- [ ] **Step 1: 添加操作辅助方法**

在 `src/app.rs` 的 `impl CDriveManagerApp` 中(在 `perform_search` 方法附近)添加操作方法:

```rust
    /// 在 Windows 资源管理器中选中并显示文件。
    fn show_in_explorer(&self, path: &str) {
        #[cfg(windows)]
        {
            std::process::Command::new("explorer")
                .args(["/select,", path])
                .spawn()
                .ok();
        }
    }

    /// 用系统默认程序打开文件。
    fn open_file_path(&self, path: &str) {
        let _ = open::that(path);
    }

    /// 复制路径到剪贴板。
    fn copy_path_to_clipboard(&self, ctx: &egui::Context, path: &str) {
        ctx.copy_text(path.to_owned());
    }

    /// 删除文件到回收站,并从索引中移除。
    fn delete_search_result(&mut self, path: &str) {
        match trash::delete(path) {
            Ok(_) => {
                // 从结果中移除
                self.search_results.retain(|r| r.path != path);
                self.status_message = format!("已删除到回收站: {}", path);
                // 后台从索引删除(非阻塞)
                let path_owned = path.to_owned();
                std::thread::spawn(move || {
                    if let Ok(indexer) = crate::search_index::SearchIndexer::open() {
                        let _ = indexer.delete_by_path(&path_owned);
                    }
                });
            }
            Err(e) => {
                self.status_message = format!("删除失败: {}", e);
            }
        }
    }
```

- [ ] **Step 2: 为搜索结果行添加右键菜单**

在 Task 10 的紧凑列表与表格视图中,为每行结果添加右键菜单。修改 `draw_search_tab` 中的行渲染。

**表格视图:** 在 `for result in page_results` 循环中,将名称单元格改为带菜单:

将:
```rust
                                    ui.horizontal(|ui| {
                                        let name_label = if result.name.to_ascii_lowercase().contains(&query_lower) {
                                            RichText::new(&result.name)
                                                .strong()
                                                .color(egui::Color32::from_rgb(100, 180, 255))
                                        } else {
                                            RichText::new(&result.name)
                                        };
                                        ui.add(egui::Label::new(name_label).sense(egui::Sense::click()));
                                    });
```

替换为:

```rust
                                    let path_owned = result.path.clone();
                                    let name_owned = result.name.clone();
                                    let size_owned = result.size;
                                    let ext_owned = result.extension.clone();
                                    let is_dir = result.is_directory;
                                    let modified = result.modified;
                                    ui.horizontal(|ui| {
                                        let name_label = if result.name.to_ascii_lowercase().contains(&query_lower) {
                                            RichText::new(&result.name)
                                                .strong()
                                                .color(egui::Color32::from_rgb(100, 180, 255))
                                        } else {
                                            RichText::new(&result.name)
                                        };
                                        let label_resp = ui.add(egui::Label::new(name_label).sense(egui::Sense::click()));
                                        label_resp.context_menu(|ui| {
                                            self.draw_search_result_menu(ui, &path_owned, &name_owned, size_owned, &ext_owned, is_dir, modified);
                                        });
                                    });
```

**紧凑列表视图:** 在 `for i in row_range` 循环中,将行内容包裹 context_menu:

将:
```rust
                            ui.horizontal(|ui| {
                                let icon = if result.is_directory { "📁" } else { "📄" };
                                ui.label(format!("{icon} {}", result.name));
                                // ...
                            });
```

替换为:

```rust
                            let path_owned = result.path.clone();
                            let name_owned = result.name.clone();
                            let size_owned = result.size;
                            let ext_owned = result.extension.clone();
                            let is_dir = result.is_directory;
                            let modified = result.modified;
                            let row_resp = ui.horizontal(|ui| {
                                let icon = if result.is_directory { "📁" } else { "📄" };
                                ui.label(format!("{icon} {}", result.name));
                                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                    ui.label(RichText::new(format::bytes(result.size)).weak());
                                    ui.add_space(60.0);
                                    let display_path = if result.parent_path.len() > 50 {
                                        format!("...{}", &result.parent_path[result.parent_path.len().saturating_sub(47)..])
                                    } else {
                                        result.parent_path.clone()
                                    };
                                    ui.label(RichText::new(display_path).weak().small());
                                });
                            }).response;
                            row_resp.context_menu(|ui| {
                                self.draw_search_result_menu(ui, &path_owned, &name_owned, size_owned, &ext_owned, is_dir, modified);
                            });
```

- [ ] **Step 3: 实现 draw_search_result_menu 方法**

在 `impl CDriveManagerApp` 中添加:

```rust
    /// 绘制搜索结果右键菜单。
    fn draw_search_result_menu(
        &mut self, ui: &mut egui::Ui,
        path: &str, name: &str, size: u64, ext: &str, is_dir: bool, modified: Option<u64>,
    ) {
        if ui.button("📂 在资源管理器中显示").clicked() {
            self.show_in_explorer(path);
            ui.close_menu();
        }
        if !is_dir && ui.button("📄 打开文件").clicked() {
            self.open_file_path(path);
            ui.close_menu();
        }
        if ui.button("📋 复制路径").clicked() {
            self.copy_path_to_clipboard(ui.ctx(), path);
            ui.close_menu();
        }
        ui.separator();
        // 属性信息
        ui.label(RichText::new("属性").strong());
        ui.label(format!("名称: {}", name));
        ui.label(format!("大小: {}", format::bytes(size)));
        if !ext.is_empty() {
            ui.label(format!("类型: .{}", ext));
        }
        if let Some(m) = modified {
            if let Some(dt) = chrono::DateTime::from_timestamp(m as i64, 0) {
                ui.label(format!("修改时间: {}", dt.format("%Y-%m-%d %H:%M")));
            }
        }
        ui.label(format!("路径: {}", path));
        ui.separator();
        if ui.button("🗑 删除到回收站").clicked() {
            self.delete_search_result(path);
            ui.close_menu();
        }
    }
```

- [ ] **Step 4: 添加 chrono 导入**

在 `src/app.rs` 顶部 use 块中确认已有 chrono,若无则添加。检查文件顶部是否已导入 `chrono::DateTime`,若没有,在适当位置添加:

```rust
use chrono::DateTime;
```

如果 chrono 未在 app.rs 依赖中显式使用,需确认 Cargo.toml 中 chrono 已添加(Task 1 已添加)。

- [ ] **Step 5: 验证编译通过**

Run: `cargo build`
Expected: 编译成功

- [ ] **Step 6: 提交**

```bash
git add src/app.rs
git commit -m "feat: 搜索结果右键菜单(显示/打开/复制/属性/删除)"
```

---

### Task 12: 更新搜索提示文案与集成验证

**Files:**
- Modify: `src/app.rs`

- [ ] **Step 1: 更新搜索框提示文案**

在 `draw_search_tab` 中找到搜索框的 `hint_text`(约第 4287 行):

```rust
                .hint_text("输入文件名或路径关键词（支持模糊匹配）")
```

替换为:

```rust
                .hint_text("输入关键字、ext:pdf、size:>100MB、dm:today、regex:^Report、AND/OR/NOT")
```

- [ ] **Step 2: 移除 perform_search 中的最小长度限制**

找到 `perform_search` 方法(约第 4411 行):

```rust
        let query = self.search_query.trim().to_string();
        if query.len() < 2 {
            self.search_results.clear();
            self.search_in_progress = false;
            self.search_last_input = None;
            if let Some(h) = self.search_handle.take() {
                h.cancel();
            }
            return;
        }
```

tantivy 支持单字符搜索,移除 `< 2` 限制。替换为:

```rust
        let query = self.search_query.trim().to_string();
        if query.is_empty() {
            self.search_results.clear();
            self.search_in_progress = false;
            self.search_last_input = None;
            if let Some(h) = self.search_handle.take() {
                h.cancel();
            }
            return;
        }
```

- [ ] **Step 3: 验证编译与运行**

Run: `cargo build`
Expected: 编译成功

Run: `cargo run`
Expected: 应用启动,搜索框显示新提示文案,输入关键字能即时搜索,视图切换与右键菜单工作正常

- [ ] **Step 4: 提交**

```bash
git add src/app.rs
git commit -m "feat: 更新搜索提示文案并移除最小长度限制"
```

---

### Task 13: 端到端集成测试

**Files:**
- Create: `tests/search_integration.rs`

- [ ] **Step 1: 编写集成测试**

创建 `tests/search_integration.rs`:

```rust
//! 端到端集成测试:验证 tantivy 搜索索引从构建到查询的完整流程。

use cdrive_manager::search_index::{
    SearchIndexer, parse_query, compile_query, QueryNode, root_key,
};
use cdrive_manager::model::{ScanStats, FileRecord};
use std::path::{Path, PathBuf};

/// 构造测试用 ScanStats。
fn make_stats() -> ScanStats {
    let mut stats = ScanStats::default();
    stats.root = PathBuf::from("C:\\integration-test");
    stats.all_files = vec![
        FileRecord {
            path: PathBuf::from("C:\\integration-test\\report_2024.pdf"),
            size: 5 * 1024 * 1024,
            modified: Some(std::time::SystemTime::now()),
            extension: ".pdf".to_owned(),
        },
        FileRecord {
            path: PathBuf::from("C:\\integration-test\\summary.xlsx"),
            size: 100 * 1024,
            modified: Some(std::time::SystemTime::now()),
            extension: ".xlsx".to_owned(),
        },
        FileRecord {
            path: PathBuf::from("C:\\integration-test\\temp.tmp"),
            size: 500,
            modified: Some(std::time::SystemTime::now()),
            extension: ".tmp".to_owned(),
        },
        FileRecord {
            path: PathBuf::from("C:\\integration-test\\large.bin"),
            size: 500 * 1024 * 1024,
            modified: Some(std::time::SystemTime::now()),
            extension: ".bin".to_owned(),
        },
    ];
    stats.file_count = 4;
    stats
}

fn setup_temp_env() {
    let tmp = std::env::temp_dir().join(format!(
        "cdrive-integration-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()
    ));
    std::fs::create_dir_all(&tmp).unwrap();
    std::env::set_var("LOCALAPPDATA", &tmp);
}

#[test]
fn full_workflow_build_and_search_by_keyword() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "report", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].name.contains("report"));
}

#[test]
fn search_by_extension_filter() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "ext:pdf", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].name.ends_with(".pdf"));
}

#[test]
fn search_by_multiple_extensions() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "ext:pdf,xlsx", 10).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn search_by_size_greater_than() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    // 大于 100MB:只有 large.bin (500MB)
    let results = indexer.search(&key, "size:>100MB", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].name.contains("large"));
}

#[test]
fn search_by_size_range() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    // 1KB 到 10MB:report_2024.pdf (5MB) 和 summary.xlsx (100KB)
    let results = indexer.search(&key, "size:1KB-10MB", 10).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn search_by_regex_pattern() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "regex:^report_\\d+", 10).unwrap();
    assert_eq!(results.len(), 1);
    assert!(results[0].name.contains("report_2024"));
}

#[test]
fn search_with_boolean_not() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    // 所有文件 NOT tmp
    let results = indexer.search(&key, "NOT tmp", 10).unwrap();
    assert_eq!(results.len(), 3);
    assert!(!results.iter().any(|r| r.name.contains("temp")));
}

#[test]
fn search_with_boolean_and() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "report AND pdf", 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn search_with_boolean_or() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "report OR summary", 10).unwrap();
    assert_eq!(results.len(), 2);
}

#[test]
fn search_combined_keyword_and_extension() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "report ext:pdf", 10).unwrap();
    assert_eq!(results.len(), 1);
}

#[test]
fn delete_and_reindex_reflects_in_search() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    indexer.delete_by_path("C:\\integration-test\\temp.tmp").unwrap();
    let results = indexer.search(&key, "temp", 10).unwrap();
    assert!(results.is_empty());
}

#[test]
fn empty_query_returns_all() {
    setup_temp_env();
    let indexer = SearchIndexer::open().unwrap();
    let stats = make_stats();
    let key = root_key(Path::new("C:\\integration-test"));
    indexer.build_from_scan(&stats, &key, |_, _| {}).unwrap();

    let results = indexer.search(&key, "", 100).unwrap();
    assert_eq!(results.len(), 4);
}

#[test]
fn parse_query_validates_complex_syntax() {
    // 验证解析器能处理复杂查询
    let node = parse_query("(report OR summary) AND ext:pdf NOT tmp").unwrap();
    assert!(matches!(node, QueryNode::And(_, _)));
}
```

- [ ] **Step 2: 运行集成测试**

Run: `cargo test --test search_integration`
Expected: 12 个集成测试全部 PASS

- [ ] **Step 3: 提交**

```bash
git add tests/search_integration.rs
git commit -m "test: 添加 tantivy 搜索端到端集成测试"
```

---

### Task 14: 清理与文档更新

**Files:**
- Modify: `src/search_index/mod.rs` (清理 unused 警告)
- Modify: `Cargo.toml` (确认依赖无多余)
- Create: `docs/search-feature.md`

- [ ] **Step 1: 清理 mod.rs 的 unused 警告**

审查 `src/search_index/mod.rs`,移除确实未使用的 re-export。保留 app.rs 实际使用的:

```rust
pub use indexer::{SearchIndexer, FileSearchResult, root_key};
pub use worker::{
    spawn_build_index, spawn_search, spawn_usn_index_listener, SearchHandle,
    SearchIndexEvent, SearchIndexHandle, SearchResult,
};
pub use usn_journal::{spawn_usn_listener, UsnEvent, UsnListenerConfig, UsnListenerHandle};
```

移除 `#[allow(unused_imports)]` 标记(若不再需要),或保留以避免警告。

- [ ] **Step 2: 编写功能文档**

创建 `docs/search-feature.md`:

```markdown
# Everything 风格快速搜索功能

## 概述

基于 tantivy 全文搜索引擎实现的即时文件搜索,支持丰富的查询语法。

## 查询语法

### 简单关键字
输入多个关键字,空格分隔,默认 AND 关系:
```
report 2024 pdf
```

### 短语匹配
用引号包裹,必须连续匹配:
```
"annual report"
```

### 扩展名过滤
`ext:` 后跟扩展名(不含点),多个用逗号分隔:
```
ext:pdf
ext:pdf,doc,xlsx
```

### 大小过滤
`size:` 后跟比较运算符和大小值,支持 KB/MB/GB/TB 后缀:
```
size:>100MB
size:<1KB
size:1KB-10MB
```

### 日期过滤
`dm:` 后跟日期关键字或绝对日期:
```
dm:today
dm:yesterday
dm:this-week
dm:this-month
dm:>2024-01-01
dm:2024-01-01..2024-12-31
```

### 路径限定
`path:` 后跟路径片段:
```
path:Downloads
path:"C:\Users"
```

### 正则表达式
`regex:` 前缀切换到正则模式,匹配文件名:
```
regex:^Report-\d{4}\.pdf$
```

### 布尔运算
支持 AND、OR、NOT 和括号分组:
```
report AND pdf
report OR summary
NOT tmp
(report OR summary) AND ext:pdf
```

## 视图模式

- **表格视图**:名称/路径/大小/类型四列,适合查看详细信息
- **紧凑列表**:单行显示,虚拟滚动,适合快速浏览大量结果

## 文件操作

右键点击搜索结果可:
- 在资源管理器中显示(定位文件)
- 打开文件(系统默认程序)
- 复制路径到剪贴板
- 查看文件属性
- 删除到回收站

## 索引管理

- 索引存储位置:`%LOCALAPPDATA%\cdrive-manager\search-index\`
- FRN 映射:`%LOCALAPPDATA%\cdrive-manager\frn-mapping.sqlite3`
- 扫描完成后自动构建索引
- USN Journal 实时监听文件变化,增量更新索引
```

- [ ] **Step 3: 运行完整测试套件**

Run: `cargo test`
Expected: 所有测试通过

- [ ] **Step 4: 运行 clippy 检查**

Run: `cargo clippy -- -W clippy::all`
Expected: 无错误(警告可接受但应尽量修复明显的)

- [ ] **Step 5: 提交**

```bash
git add docs/search-feature.md src/search_index/mod.rs
git commit -m "docs: 添加搜索功能文档并清理 unused 警告"
```

---

## Self-Review

**1. Spec coverage:**

| 规范要求 | 对应任务 | 状态 |
|---------|---------|------|
| tantivy 替换 SQLite FTS5 | Task 1-9 | ✅ |
| DSL:关键字 | Task 4 | ✅ |
| DSL:扩展名 ext: | Task 5 | ✅ |
| DSL:大小 size: | Task 5 | ✅ |
| DSL:日期 dm: | Task 5 | ✅ |
| DSL:路径 path: | Task 5 | ✅ |
| DSL:正则 regex: | Task 4,5 | ✅ |
| DSL:布尔 AND/OR/NOT/() | Task 5 | ✅ |
| 表格视图 | Task 10 | ✅ |
| 紧凑列表(虚拟滚动) | Task 10 | ✅ |
| 视图切换 | Task 10 | ✅ |
| 在资源管理器显示 | Task 11 | ✅ |
| 打开文件 | Task 11 | ✅ |
| 复制路径 | Task 11 | ✅ |
| 查看属性 | Task 11 | ✅ |
| 删除文件 | Task 11 | ✅ |
| 加入清理队列 | — | ⚠️ 见下 |
| 全盘索引/仅扫描目录配置 | — | ⚠️ 见下 |
| USN 增量更新 | Task 7,8 | ✅ |
| FRN→路径 映射 | Task 3,7 | ✅ |

**已知缺口(刻意推迟到后续迭代):**

1. **加入清理队列** (Task 11 中未实现 `add_to_cleanup_queue`):这需要与现有 `cleanup_preview` 流程深度集成,涉及 UI 状态管理。考虑到当前任务已较大,且右键删除已覆盖主要操作,将此推迟到独立任务。规范中已标记,实现时可在 `draw_search_result_menu` 中添加按钮但显示"即将支持"。

2. **搜索范围配置 (FullDisk/LastScan)**:规范第 4 部分设计了 `SearchScope` 枚举,但实现中默认复用现有扫描流程(扫描后构建索引)。全盘索引独立于扫描的配置 UI 推迟到后续,因为现有流程已满足"扫描后可搜索"的核心需求。

**2. Placeholder scan:** ✅ 无 TBD/TODO,所有步骤含具体代码

**3. Type consistency:** ✅ `FileSearchResult` 字段在 indexer.rs/worker.rs/app.rs 中一致;`QueryNode` 变体在 query.rs 中定义并被 compile_query 正确匹配;`SearchIndexer` 方法签名在 worker.rs 调用一致

---

## Execution Handoff

Plan complete and saved to `docs/superpowers/plans/2026-07-30-everything-like-search.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
