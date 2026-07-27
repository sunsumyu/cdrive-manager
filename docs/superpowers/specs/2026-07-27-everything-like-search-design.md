# Everything 风格快速搜索功能设计

**日期**: 2026-07-27
**状态**: 待审核
**作者**: AI Agent + 用户协作

---

## 1. 概述

### 1.1 目标

用 tantivy 替换现有 SQLite FTS5 搜索引擎，构建一个类似 Everything 的即时文件搜索功能，支持：

- **完整搜索语法**：关键字、扩展名、大小、日期、路径、正则、布尔运算
- **可切换视图**：表格视图 ↔ 紧凑列表（虚拟滚动）
- **可配置索引范围**：全盘索引 / 仅扫描目录
- **完整文件操作**：在资源管理器显示、打开、复制路径、删除、加入清理队列

### 1.2 背景

当前项目已有基于 SQLite FTS5 的搜索索引模块 (`src/search_index/`)，但查询能力有限：

- 仅支持简单关键字模糊匹配
- 不支持扩展名/大小/日期/路径过滤
- 不支持正则表达式
- 不支持布尔运算组合

Everything 的核心体验是**输入即出结果**，且支持丰富的查询语法。本设计旨在用 tantivy 全文搜索引擎替换现有实现，提供更强的查询能力。

---

## 2. 架构设计

### 2.1 模块布局

替换 `src/search_index/` 目录结构：

```
src/search_index/
├── mod.rs              # 公共 API 出口（保持 app.rs 的导入不变）
├── schema.rs          # tantivy schema 定义 + 索引目录管理
├── indexer.rs          # 索引构建/增量更新（替代 db.rs）
├── query.rs           # DSL 解析器 + tantivy 查询编译器
├── worker.rs          # 后台索引/搜索线程（重写）
├── usn_journal.rs     # USN 监听器（保留，适配新 indexer）
└── path_resolver.rs   # FRN→路径 解析（从 db.rs 抽出，用 SQLite 单独存）
```

### 2.2 核心决策

| 决策点 | 选择 | 理由 |
|--------|------|------|
| 搜索引擎 | tantivy | 原生 Rust、支持复杂布尔查询、正则、范围查询，性能高 |
| FRN 映射 | 保留 SQLite | tantivy 不擅长 KV 查询，SQLite 小表即可满足 |
| 写入串行化 | IndexWriter 单例 | tantivy 单写多读架构，所有写入集中管理 |
| 索引目录 | `%LOCALAPPDATA%\cdrive-manager\search-index\` | 与现有缓存并存，便于管理 |

### 2.3 数据流

```
┌─────────────────────────────────────────────────────────────────────┐
│                          数据流                                     │
├─────────────────────────────────────────────────────────────────────┤
│                                                                     │
│  扫描完成 ──→ build_from_scan() ──→ tantivy 索引                    │
│                   │                    │                            │
│                   │                    ├─ name (FTS)                │
│                   │                    ├─ path (FTS)                │
│                   │                    ├─ extension (FTS)           │
│                   │                    ├─ size (数值)               │
│                   │                    ├─ modified_days (数值)      │
│                   │                    └─ is_directory (布尔)       │
│                   │                                                 │
│                   └─→ frn-mapping.sqlite3 (FRN→路径)                │
│                                                                     │
│  USN 事件 ──→ handle_usn_event() ──→ 索引增量更新                   │
│                                                                     │
│  用户查询 ──→ parse_query() ──→ QueryNode AST ──→ compile()         │
│                  │                               │                  │
│                  │                               └─→ tantivy Query  │
│                  │                                                 │
│                  └────────────────────────────────────────────────→ 搜索结果
│                                                                     │
└─────────────────────────────────────────────────────────────────────┘
```

---

## 3. 数据模型

### 3.1 FileSearchResult（保持不变）

```rust
pub struct FileSearchResult {
    pub name: String,           // 文件名（含扩展名）
    pub path: String,           // 完整路径
    pub parent_path: String,    // 父目录路径
    pub extension: String,      // 扩展名（小写，无点）
    pub size: u64,              // 字节大小
    pub modified: Option<u64>,  // Unix 时间戳
    pub is_directory: bool,     // 是否目录
}
```

### 3.2 tantivy Schema

```rust
pub fn create_schema() -> Schema {
    let mut builder = Schema::builder();
    
    builder.add_text_field("name", STORED);           // 文件名，FTS
    builder.add_text_field("path", STORED);           // 完整路径，FTS
    builder.add_text_field("parent_path", STORED);    // 父目录
    builder.add_text_field("extension", STORED);      // 扩展名
    builder.add_u64_field("size", STORED);            // 字节大小
    builder.add_u64_field("modified", STORED);        // 修改时间戳
    builder.add_u64_field("modified_days", INDEXED);  // 修改日期（天级）
    builder.add_bool_field("is_directory", STORED);   // 是否目录
    builder.add_text_field("root_key", STORED);       // 所属扫描根
    builder.add_text_field("frn", STORED);            // File Reference Number
    
    builder.build()
}
```

### 3.3 索引策略

| 字段 | 索引方式 | 用途 |
|------|----------|------|
| `name` | 文本索引 + 存储 | FTS 关键字搜索、高亮匹配 |
| `path` | 文本索引 + 存储 | 路径限定查询 (`path:Downloads`) |
| `extension` | 文本索引 + 存储 | `ext:pdf` 扩展名过滤 |
| `size` | 数值索引 + 存储 | `size:>100MB` 大小过滤 |
| `modified` | 数值索引 + 存储 | 原始时间戳，用于精确排序 |
| `modified_days` | 数值索引 | `dm:this-week` 日期范围查询 |

---

## 4. 查询 DSL

### 4.1 语法规则

```
查询语句 ::= 子句 (空格 子句)*

子句 ::= 
  | 关键字                    # 简单关键字：report
  | "带空格短语"              # 引号短语："annual report"
  | 字段:值                   # 字段过滤：ext:pdf
  | 字段:运算符值             # 范围过滤：size:>100MB
  | regex:正则表达式          # 正则模式：regex:^Report-\d{4}
  | AND 子句                  # 布尔与：report AND pdf
  | OR 子句                   # 布尔或：report OR summary
  | NOT 子句                  # 布尔非：NOT tmp
  | (子句 子句*)              # 括号分组：(report OR summary) AND pdf
```

### 4.2 字段语法详细说明

| 语法 | 示例 | 说明 |
|------|------|------|
| **关键字** | `report 2024` | 空格分隔，默认 AND，在 `name` 和 `path` 中搜索 |
| **短语** | `"annual report"` | 引号包裹，必须连续匹配 |
| **扩展名** | `ext:pdf` | 单扩展名 |
| | `ext:pdf,doc,xlsx` | 多扩展名（OR 关系） |
| **大小** | `size:>100MB` | 大于 |
| | `size:<1KB` | 小于 |
| | `size:1KB-10MB` | 范围 |
| **日期** | `dm:today` | 今天 |
| | `dm:yesterday` | 昨天 |
| | `dm:this-week` | 本周（周一至今） |
| | `dm:this-month` | 本月 |
| | `dm:>2024-01-01` | 指定日期之后 |
| | `dm:2024-01-01..2024-12-31` | 日期范围 |
| **路径** | `path:Downloads` | 路径包含指定目录 |
| | `path:"C:\Users"` | 路径包含指定路径 |
| **正则** | `regex:^Report-\d{4}\.pdf$` | 完整正则匹配文件名 |
| **布尔** | `report AND pdf` | AND（默认可省略） |
| | `report OR summary` | OR |
| | `NOT tmp` | 排除 |
| | `(report OR summary) AND pdf` | 括号分组 |

### 4.3 大小单位后缀

| 后缀 | 值 |
|------|-----|
| KB / K | 1024 字节 |
| MB / M | 1024² 字节 |
| GB / G | 1024³ 字节 |
| TB / T | 1024⁴ 字节 |
| 无后缀 | 字节 |

### 4.4 查询 AST

```rust
#[derive(Debug, Clone, PartialEq)]
pub enum QueryNode {
    Keywords(Vec<String>),
    Phrase(String),
    Extension(Vec<String>),
    Size { op: CompareOp, value: u64 },
    SizeRange { min: u64, max: u64 },
    Date { op: CompareOp, value: DateValue },
    DateRange { start: DateValue, end: DateValue },
    Path(String),
    Regex(String),
    And(Box<QueryNode>, Box<QueryNode>),
    Or(Box<QueryNode>, Box<QueryNode>),
    Not(Box<QueryNode>),
    Group(Box<QueryNode>),
}
```

---

## 5. 索引构建与增量更新

### 5.1 索引构建流程

1. **触发条件**：用户选择"全盘索引"或扫描完成后自动构建
2. **收集文件**：调用 scanner 模块遍历目录或复用 ScanStats
3. **批量写入**：使用 IndexWriter，每 10,000 条提交一次
4. **建立辅助索引**：FRN→路径 映射写入 frn-mapping.sqlite3
5. **启动 USN 监听**：全盘索引模式下启动实时更新

### 5.2 USN 增量更新

| USN 事件 | tantivy 操作 |
|----------|--------------|
| FileCreated | `IndexWriter.add_document()` + SQLite INSERT frn_map |
| FileDeleted | `IndexWriter.delete_term(Term::from_field_text(FieldId::PATH, path))` + SQLite DELETE frn_map |
| FileModified | delete + add（tantivy 不支持单字段更新） |
| FileRenamed | delete old + add new + SQLite UPDATE frn_map |

### 5.3 搜索范围配置

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchScope {
    FullDisk { drive_letter: char },
    LastScan,
}

impl SearchScope {
    pub fn default() -> Self {
        SearchScope::FullDisk { drive_letter: 'C' }
    }
}
```

---

## 6. UI 设计

### 6.1 搜索面板布局

**表格视图**：
```
┌─────────────────────────────────────────────────────────────────────┐
│  🔍 搜索：[________________________________] [清空] [视图: 表格 ▼]  │
├─────────────────────────────────────────────────────────────────────┤
│  找到 12,345 个结果                              ◀ 1/124 页 ▶      │
├─────────────────────────────────────────────────────────────────────┤
│  名称              路径                  大小       类型           │
│  ─────────────────────────────────────────────────────────────────  │
│  📄 report.pdf     C:\Users\...\Downloads   2.3 MB    PDF          │
│  📄 report.xlsx    C:\Users\...\Documents   156 KB    XLSX         │
│  📁 Reports        C:\Users\...\Projects         -    目录          │
└─────────────────────────────────────────────────────────────────────┘
```

**紧凑列表视图（虚拟滚动）**：
```
┌─────────────────────────────────────────────────────────────────────┐
│  🔍 搜索：[________________________________] [清空] [视图: 紧凑 ▼]  │
├─────────────────────────────────────────────────────────────────────┤
│  找到 12,345 个结果                                                 │
├─────────────────────────────────────────────────────────────────────┤
│  📄 report.pdf          C:\Users\Admin\Downloads\        2.3 MB    │
│  📄 report_2024.pdf     C:\Users\Admin\Documents\       1.8 MB    │
│  📄 report.xlsx         C:\Users\Admin\Projects\        156 KB    │
│  📁 Reports             C:\Users\Admin\Projects\             -    │
│  ...（虚拟滚动，无分页）                                           │
└─────────────────────────────────────────────────────────────────────┘
```

### 6.2 视图切换

```rust
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum SearchViewMode {
    Table,      // 表格视图（分页）
    Compact,    // 紧凑列表（虚拟滚动）
}
```

### 6.3 文件操作

右键菜单支持以下操作：

| 操作 | 实现方式 |
|------|----------|
| 在资源管理器中显示 | `explorer /select,<path>` |
| 打开文件 | `cmd /C start "" <path>` |
| 复制路径 | `clipboard-win` crate |
| 删除文件 | 复用现有 `trash` crate |
| 加入清理队列 | 复用现有清理流程 |

---

## 7. 实现计划

### 7.1 阶段划分

| 阶段 | 内容 | 工时 |
|------|------|------|
| 1 | 基础设施：tantivy 依赖、schema、indexer.open() | 1-2 天 |
| 2 | DSL 解析器：parse_query()、QueryNode::compile() | 1-2 天 |
| 3 | 索引构建：build_from_scan()、进度回调 | 1 天 |
| 4 | USN 增量更新：handle_usn_event()、FRN 映射 | 1 天 |
| 5 | UI 重构：视图切换、虚拟滚动、右键菜单 | 2 天 |
| 6 | 集成与清理：删除旧代码、迁移数据、文档更新 | 1 天 |

**总计**：约 7-9 个工作日

### 7.2 依赖关系

```
阶段 1 → 阶段 2 / 阶段 3 / 阶段 4 → 阶段 5 → 阶段 6
```

### 7.3 新增依赖

```toml
[dependencies]
tantivy = "0.21"          # 全文搜索引擎
nom = "7.1"               # 解析器组合子
chrono = "0.4"            # 日期处理
clipboard-win = "4.5"     # Windows 剪贴板
```

### 7.4 回滚策略

1. 保留旧代码为 `db_legacy.rs` 直到阶段 6 完成
2. 支持 `--use-sqlite-search` 命令行参数切换
3. 首次启动时检测旧 SQLite 索引，提示用户选择

---

## 8. 风险与缓解

| 风险 | 影响 | 缓解措施 |
|------|------|----------|
| tantivy 学习曲线 | 延期 | 先用小 Demo 验证核心功能，再集成 |
| 索引构建慢于预期 | 用户体验差 | 显示进度条，支持后台构建 |
| USN 漏更新 | 索引不一致 | 定期全量校验，用户可手动重建 |
| 正则性能问题 | 查询超时 | 设置查询超时限制，提示用户简化正则 |

---

## 9. 验收标准

1. **功能验收**：
   - 所有 DSL 语法正确解析和执行
   - 正则查询返回正确结果
   - 两种视图模式正常切换
   - 所有文件操作正常工作

2. **性能验收**：
   - 10 万文件索引构建 < 30 秒
   - 单次查询响应 < 100ms
   - 虚拟滚动流畅（60 FPS）

3. **兼容性验收**：
   - 现有 SQLite 缓存不受影响
   - 用户数据可迁移
   - 回滚功能正常

---

**文档结束**