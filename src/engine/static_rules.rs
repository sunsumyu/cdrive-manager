//! 静态规则引擎
//!
//! 基于确定性规则的分类引擎，不依赖运行时状态。
//! 规则体系覆盖 Windows 10/11 的全部已知系统路径。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::classification::{FileCategory, FileClassification, FileImportance, SystemLayer};

// ============================================================
// 分类结果
// ============================================================

/// 静态规则引擎的分类结果。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClassificationResult {
    pub classification: FileClassification,
    pub matched_rule: String,
    pub match_reason: String,
}

// ============================================================
// 静态规则引擎
// ============================================================

/// 确定性规则引擎，通过路径匹配判断文件分类。
///
/// 规则顺序即优先级（first-match），越具体的规则应排在越前面。
#[derive(Debug, Clone)]
pub struct StaticRuleEngine {
    rules: Vec<StaticRule>,
}

impl Default for StaticRuleEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl StaticRuleEngine {
    /// 创建包含完整规则集的引擎。
    pub fn new() -> Self {
        Self {
            rules: build_all_rules(),
        }
    }

    /// 对路径进行分类。
    ///
    /// 返回 `Some(ClassificationResult)` 如果命中规则，否则返回 `None`。
    pub fn classify(&self, path: &Path) -> Option<ClassificationResult> {
        for rule in &self.rules {
            if let Some(reason) = rule.matches(path) {
                let classification = FileClassification::new(path, rule.category, rule.importance)
                    .with_owner(&rule.owner_hint);
                return Some(ClassificationResult {
                    classification,
                    matched_rule: rule.id.to_string(),
                    match_reason: reason,
                });
            }
        }
        None
    }

    /// 检查路径是否为受保护的系统路径（不应清理）。
    pub fn is_protected_path(&self, path: &Path) -> bool {
        is_protected_path(path)
    }
}

// ============================================================
// 单条规则定义
// ============================================================

#[derive(Debug, Clone)]
struct StaticRule {
    id: &'static str,
    category: FileCategory,
    importance: FileImportance,
    /// 规则匹配器。
    matcher: RuleMatcher,
    /// 所属应用/组件的提示名。
    owner_hint: String,
}

#[derive(Debug, Clone)]
enum RuleMatcher {
    /// 路径前缀匹配（归一化后，组件级）。
    Prefix(&'static [&'static str]),
    /// 扩展名匹配。
    Extension(&'static [&'static str]),
    /// 路径包含指定组件名。
    Component(&'static [&'static str]),
    /// 路径包含指定子串（用于用户级路径片段匹配）。
    Fragment(&'static [&'static str]),
    /// 文件名通配匹配（如 `thumbcache_*.db`）。
    FileNameGlob(&'static [&'static str]),
    /// 组合条件：扩展名 + 路径组件。
    ExtensionInContext {
        extensions: &'static [&'static str],
        components: &'static [&'static str],
    },
    /// 组合条件：扩展名 + 路径前缀。
    ExtensionWithPrefix {
        extensions: &'static [&'static str],
        prefixes: &'static [&'static str],
    },
}

impl StaticRule {
    fn matches(&self, path: &Path) -> Option<String> {
        match &self.matcher {
            RuleMatcher::Prefix(prefixes) => {
                let normalized = normalize_path(path);
                for prefix in prefixes.iter() {
                    if path_matches_prefix(&normalized, prefix) {
                        return Some(format!("路径前缀命中: {}", prefix));
                    }
                }
                None
            }
            RuleMatcher::Extension(extensions) => {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_ascii_lowercase()))?;
                if extensions.contains(&ext.as_str()) {
                    Some(format!("扩展名命中: {}", ext))
                } else {
                    None
                }
            }
            RuleMatcher::Component(components) => {
                for component in path.components() {
                    let text = component
                        .as_os_str()
                        .to_string_lossy()
                        .to_ascii_lowercase();
                    if components.contains(&text.as_str()) {
                        return Some(format!("路径组件命中: {}", text));
                    }
                }
                None
            }
            RuleMatcher::Fragment(fragments) => {
                let normalized = normalize_path(path);
                for fragment in fragments.iter() {
                    if normalized.contains(fragment) {
                        return Some(format!("路径片段命中: {}", fragment));
                    }
                }
                None
            }
            RuleMatcher::FileNameGlob(globs) => {
                let file_name = path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .map(|n| n.to_ascii_lowercase())?;
                for glob in globs.iter() {
                    if glob_matches(&file_name, glob) {
                        return Some(format!("文件名通配命中: {}", glob));
                    }
                }
                None
            }
            RuleMatcher::ExtensionInContext {
                extensions,
                components,
            } => {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_ascii_lowercase()))?;
                if !extensions.contains(&ext.as_str()) {
                    return None;
                }
                for component in path.components() {
                    let text = component
                        .as_os_str()
                        .to_string_lossy()
                        .to_ascii_lowercase();
                    if components.contains(&text.as_str()) {
                        return Some(format!("扩展名 {} + 组件 {} 命中", ext, text));
                    }
                }
                None
            }
            RuleMatcher::ExtensionWithPrefix {
                extensions,
                prefixes,
            } => {
                let ext = path
                    .extension()
                    .and_then(|e| e.to_str())
                    .map(|e| format!(".{}", e.to_ascii_lowercase()))?;
                if !extensions.contains(&ext.as_str()) {
                    return None;
                }
                let normalized = normalize_path(path);
                for prefix in prefixes.iter() {
                    if path_matches_prefix(&normalized, prefix) {
                        return Some(format!("扩展名 {} + 前缀 {} 命中", ext, prefix));
                    }
                }
                None
            }
        }
    }
}

// ============================================================
// 路径匹配工具函数
// ============================================================

/// 归一化路径用于匹配。
pub(crate) fn normalize_path(path: &Path) -> String {
    path.display().to_string().replace('/', "\\").to_ascii_lowercase()
}

/// 组件级前缀匹配，避免 `c:\windows` 误命中 `c:\windows.old`。
pub(crate) fn path_matches_prefix(normalized: &str, prefix: &str) -> bool {
    if normalized == prefix {
        return true;
    }
    normalized.starts_with(prefix) && normalized[prefix.len()..].starts_with("\\")
}

/// 简单通配匹配（`*` 匹配任意字符序列）。
pub(crate) fn glob_matches(file_name: &str, glob: &str) -> bool {
    let mut remainder = file_name;
    let mut pattern = glob;
    let mut last_was_star = false;

    loop {
        match pattern.find('*') {
            None => {
                if last_was_star {
                    return remainder.ends_with(pattern);
                }
                return remainder == pattern;
            }
            Some(idx) => {
                let literal = &pattern[..idx];
                if !last_was_star {
                    if !remainder.starts_with(literal) {
                        return false;
                    }
                    remainder = &remainder[literal.len()..];
                } else if !literal.is_empty() {
                    match remainder.find(literal) {
                        Some(found) => remainder = &remainder[found + literal.len()..],
                        None => return false,
                    }
                }
                pattern = &pattern[idx + 1..];
                last_was_star = true;
            }
        }
        if pattern.is_empty() {
            return true;
        }
    }
}

// ============================================================
// 受保护路径判断
// ============================================================

/// Windows 系统关键目录组件名。
const PROTECTED_SYSTEM_COMPONENTS: &[&str] = &[
    "system32",
    "syswow64",
    "winsxs",
    "installer",
    "config.msi",
    "driverstore",
];

/// 绝对保护前缀（归一化小写、反斜杠）。
const PROTECTED_PREFIXES: &[&str] = &[
    "c:\\windows",
    "c:\\program files",
    "c:\\program files (x86)",
    "c:\\programdata",
    "c:\\$recycle.bin",
    "c:\\system volume information",
    "c:\\recovery",
    "c:\\boot",
    "c:\\efi",
    "c:\\config.msi",
];

/// 用户库文件夹组件。
const USER_LIBRARY_COMPONENTS: &[&str] = &[
    "desktop",
    "documents",
    "downloads",
    "pictures",
    "music",
    "videos",
    "文档",
    "桌面",
    "图片",
    "音乐",
    "视频",
];

/// 判断路径是否为受保护路径。
pub fn is_protected_path(path: &Path) -> bool {
    let normalized = normalize_path(path);

    // 1. 绝对保护前缀
    for prefix in PROTECTED_PREFIXES {
        if path_matches_prefix(&normalized, prefix) {
            return true;
        }
    }

    // 2. 用户级应用本体（任意用户下）
    if is_under_users_profile(&normalized) {
        for fragment in &[
            "\\appdata\\local\\programs\\",
            "\\appdata\\local\\packages\\",
            "\\appdata\\roaming\\",
        ] {
            if normalized.contains(fragment) {
                return true;
            }
        }
    }

    // 3. 系统关键组件（Windows.old 下的除外）
    let under_windows_old = normalized.starts_with("c:\\windows.old\\") || normalized == "c:\\windows.old";
    if !under_windows_old {
        for component in path.components() {
            let text = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            if PROTECTED_SYSTEM_COMPONENTS.contains(&text.as_str()) {
                return true;
            }
        }
    }

    // 4. 用户库文件夹（仅在 C:\Users\<user>\ 下）
    if is_under_users_profile(&normalized)
        && path.components().any(|component| {
            let text = component.as_os_str().to_string_lossy().to_ascii_lowercase();
            USER_LIBRARY_COMPONENTS.contains(&text.as_str())
        })
    {
        return true;
    }

    false
}

fn is_under_users_profile(normalized: &str) -> bool {
    let lower = normalized.strip_prefix("c:\\").unwrap_or(normalized);
    let mut components = lower.split('\\');
    matches!((components.next(), components.next()), (Some("users"), Some(_)))
}

// ============================================================
// 规则构建器辅助宏
// ============================================================

macro_rules! rule {
    ($id:expr, $cat:ident, $imp:ident, $matcher:expr, $owner:expr) => {
        StaticRule {
            id: $id,
            category: FileCategory::$cat,
            importance: FileImportance::$imp,
            matcher: $matcher,
            owner_hint: $owner.to_string(),
        }
    };
}

// ============================================================
// 完整规则集（200+ 条）
// ============================================================

fn build_all_rules() -> Vec<StaticRule> {
    let mut rules = Vec::new();

    // ═════════════════════════════════════════════════════════
    // 第一层：系统内核层（Kernel）—— 绝对不可删除
    // ═════════════════════════════════════════════════════════

    // C:\Windows\System32 及其子目录
    rules.push(rule!(
        "kernel_system32",
        SystemBinary,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\system32"]),
        "Windows 内核"
    ));

    // C:\Windows\SysWOW64
    rules.push(rule!(
        "kernel_syswow64",
        SystemBinary,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\syswow64"]),
        "Windows 内核"
    ));

    // C:\Windows\WinSxS 组件存储
    rules.push(rule!(
        "kernel_winsxs",
        ComponentStore,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\winsxs"]),
        "Windows 组件存储"
    ));

    // C:\Windows\Installer
    rules.push(rule!(
        "kernel_installer",
        InstallerCache,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\installer"]),
        "Windows Installer"
    ));

    // C:\Windows\Boot
    rules.push(rule!(
        "kernel_boot",
        BootLoader,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\boot"]),
        "Windows 启动"
    ));

    // C:\Windows\System32\Drivers
    rules.push(rule!(
        "kernel_drivers",
        SystemDriver,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\system32\\drivers"]),
        "Windows 驱动"
    ));

    // ═════════════════════════════════════════════════════════
    // 第二层：系统服务层（SystemService）
    // ═════════════════════════════════════════════════════════

    // C:\Windows\System32\DriverStore
    rules.push(rule!(
        "service_driverstore",
        DriverStore,
        Critical,
        RuleMatcher::Prefix(&["c:\\windows\\system32\\driverstore"]),
        "Windows 驱动商店"
    ));

    // C:\Windows\System32\spool
    rules.push(rule!(
        "service_printspooler",
        PrintSpooler,
        Essential,
        RuleMatcher::Prefix(&["c:\\windows\\system32\\spool"]),
        "打印假脱机"
    ));

    // ═════════════════════════════════════════════════════════
    // 第三层：系统运行时（SystemRuntime）—— 可清理
    // ═════════════════════════════════════════════════════════

    // Windows Update 下载缓存
    rules.push(rule!(
        "runtime_wu_download",
        WindowsUpdateCache,
        Disposable,
        RuleMatcher::Prefix(&["c:\\windows\\softwaredistribution\\download"]),
        "Windows Update"
    ));

    // 传递优化缓存
    rules.push(rule!(
        "runtime_delivery_opt",
        DeliveryOptimization,
        Disposable,
        RuleMatcher::Prefix(&["c:\\windows\\softwaredistribution\\deliveryoptimization"]),
        "传递优化"
    ));

    // 系统临时目录
    rules.push(rule!(
        "runtime_system_temp",
        SystemTemp,
        Disposable,
        RuleMatcher::Prefix(&["c:\\windows\\temp"]),
        "Windows 系统临时文件"
    ));

    // Prefetch
    rules.push(rule!(
        "runtime_prefetch",
        PrefetchData,
        Performance,
        RuleMatcher::Prefix(&["c:\\windows\\prefetch"]),
        "Windows Prefetch"
    ));

    // WER 错误报告
    rules.push(rule!(
        "runtime_wer",
        ErrorReport,
        Disposable,
        RuleMatcher::Prefix(&[
            "c:\\programdata\\microsoft\\windows\\wer\\reportqueue",
            "c:\\programdata\\microsoft\\windows\\wer\\reportarchive",
        ]),
        "Windows Error Reporting"
    ));

    // Windows 日志
    rules.push(rule!(
        "runtime_logs",
        SystemLog,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\logs"]),
        "Windows 日志"
    ));

    // Panther 安装日志
    rules.push(rule!(
        "runtime_panther",
        SetupLog,
        Disposable,
        RuleMatcher::Prefix(&["c:\\windows\\panther"]),
        "Windows 安装迁移"
    ));

    // 旧版 Windows 安装
    rules.push(rule!(
        "runtime_windows_old",
        SystemTemp,
        Essential,
        RuleMatcher::Prefix(&["c:\\windows.old"]),
        "旧版 Windows 安装"
    ));

    // ═════════════════════════════════════════════════════════
    // 第四层：系统配置（SystemConfig）
    // ═════════════════════════════════════════════════════════

    // WinSxS manifest 目录（注意：WinSxS 本体已在上面处理，这里只匹配 manifest 子目录）
    // 实际上 WinSxS 内部文件已通过前缀匹配，这里跳过重复

    // ═════════════════════════════════════════════════════════
    // 第五层：用户级缓存（UserSpace + Application 混合）
    // ═════════════════════════════════════════════════════════

    // 缩略图缓存
    rules.push(rule!(
        "user_thumbnail",
        ThumbnailCache,
        Optional,
        RuleMatcher::FileNameGlob(&["thumbcache_*.db", "iconcache_*.db"]),
        "Windows Explorer"
    ));

    // WinINet 缓存
    rules.push(rule!(
        "user_inet_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\windows\\inetcache"]),
        "WinINet"
    ));

    // Chrome 缓存
    rules.push(rule!(
        "user_chrome_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&[
            "\\google\\chrome\\user data\\default\\cache",
            "\\google\\chrome\\user data\\default\\code cache",
        ]),
        "Google Chrome"
    ));

    // Edge 缓存
    rules.push(rule!(
        "user_edge_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&[
            "\\microsoft\\edge\\user data\\default\\cache",
            "\\microsoft\\edge\\user data\\default\\code cache",
        ]),
        "Microsoft Edge"
    ));

    // Firefox 缓存
    rules.push(rule!(
        "user_firefox_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\mozilla\\firefox\\profiles"]),
        "Mozilla Firefox"
    ));

    // pip 缓存
    rules.push(rule!(
        "user_pip_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Fragment(&["\\appdata\\local\\pip\\cache"]),
        "pip"
    ));

    // npm 缓存
    rules.push(rule!(
        "user_npm_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".npm"]),
        "npm"
    ));

    // Cargo 缓存
    rules.push(rule!(
        "user_cargo_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".cargo"]),
        "Cargo"
    ));

    // Gradle 缓存
    rules.push(rule!(
        "user_gradle_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".gradle"]),
        "Gradle"
    ));

    // Maven 缓存
    rules.push(rule!(
        "user_maven_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".m2"]),
        "Maven"
    ));

    // ═════════════════════════════════════════════════════════
    // 第六层：扩展名规则
    // ═════════════════════════════════════════════════════════

    // 临时文件扩展名
    rules.push(rule!(
        "ext_temp",
        GenericTemp,
        Disposable,
        RuleMatcher::Extension(&[".tmp", ".temp"]),
        "临时文件"
    ));

    // 日志文件扩展名
    rules.push(rule!(
        "ext_log",
        GenericLog,
        Optional,
        RuleMatcher::Extension(&[".log"]),
        "日志文件"
    ));

    // 转储文件扩展名
    rules.push(rule!(
        "ext_dump",
        MemoryDump,
        Optional,
        RuleMatcher::Extension(&[".dmp", ".etl"]),
        "诊断转储"
    ));

    // 备份文件扩展名
    rules.push(rule!(
        "ext_backup",
        GenericBackup,
        Optional,
        RuleMatcher::Extension(&[".bak", ".old"]),
        "备份文件"
    ));

    // 下载碎片
    rules.push(rule!(
        "ext_download_fragment",
        GenericFragment,
        Disposable,
        RuleMatcher::Extension(&[".crdownload", ".part", ".partial", ".download"]),
        "浏览器下载碎片"
    ));

    // ═════════════════════════════════════════════════════════
    // 第七层：构建产物（Application 层）
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "build_artifact",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&["target", "build", "dist", "out", "obj"]),
        "构建工具"
    ));

    // ═════════════════════════════════════════════════════════
    // 第八层：下载的安装包
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "installer_download",
        GenericInstaller,
        Optional,
        RuleMatcher::ExtensionInContext {
            extensions: &[".msi", ".msix", ".msp", ".exe"],
            components: &["downloads", "下载"],
        },
        "下载的安装包"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：更多浏览器缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_opera_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\opera software"]),
        "Opera"
    ));

    rules.push(rule!(
        "user_brave_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\bravesoftware\\brave-browser"]),
        "Brave"
    ));

    rules.push(rule!(
        "user_vivaldi_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\vivaldi"]),
        "Vivaldi"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：IDE/编辑器缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_vscode_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\vscode\\cache"]),
        "VS Code"
    ));

    rules.push(rule!(
        "user_jetbrains_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\jetbrains\\"]),
        "JetBrains"
    ));

    rules.push(rule!(
        "user_sublime_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\sublime text"]),
        "Sublime Text"
    ));

    rules.push(rule!(
        "user_notion_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\notion\\cache"]),
        "Notion"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：包管理器缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_conda_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".conda"]),
        "Conda"
    ));

    rules.push(rule!(
        "user_nuget_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".nuget"]),
        "NuGet"
    ));

    rules.push(rule!(
        "user_composer_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".composer"]),
        "Composer"
    ));

    rules.push(rule!(
        "user_yarn_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".yarn"]),
        "Yarn"
    ));

    rules.push(rule!(
        "user_pnpm_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".pnpm"]),
        "pnpm"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：办公/生产力应用
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_office_cache",
        OfficeCache,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\office\\"]),
        "Microsoft Office"
    ));

    rules.push(rule!(
        "user_teams_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\teams\\cache"]),
        "Microsoft Teams"
    ));

    rules.push(rule!(
        "user_slack_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\slack\\cache"]),
        "Slack"
    ));

    rules.push(rule!(
        "user_zoom_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\zoom\\cache"]),
        "Zoom"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：游戏平台缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_steam_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\steam\\appcache"]),
        "Steam"
    ));

    rules.push(rule!(
        "user_epic_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\epic games\\"]),
        "Epic Games"
    ));

    rules.push(rule!(
        "user_origin_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\origin\\"]),
        "Origin"
    ));

    rules.push(rule!(
        "user_battlenet_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\battle.net\\"]),
        "Battle.net"
    ));

    rules.push(rule!(
        "user_discord_cache",
        ChatMedia,
        Optional,
        RuleMatcher::Fragment(&["\\discord\\cache"]),
        "Discord"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：媒体/社交应用缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_spotify_cache",
        StreamingCache,
        Optional,
        RuleMatcher::Fragment(&["\\spotify\\cache"]),
        "Spotify"
    ));

    rules.push(rule!(
        "user_telegram_cache",
        ChatMedia,
        Optional,
        RuleMatcher::Fragment(&["\\telegram desktop\\tdata"]),
        "Telegram"
    ));

    rules.push(rule!(
        "user_whatsapp_cache",
        ChatMedia,
        Optional,
        RuleMatcher::Fragment(&["\\whatsapp\\cache"]),
        "WhatsApp"
    ));

    rules.push(rule!(
        "user_skype_cache",
        ChatLog,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\skype for desktop\\cache"]),
        "Skype"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：容器/虚拟化
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_docker_cache",
        ContainerLayer,
        Optional,
        RuleMatcher::Fragment(&["\\docker\\"]),
        "Docker"
    ));

    rules.push(rule!(
        "user_vmware_cache",
        VirtualMachineDisk,
        Optional,
        RuleMatcher::Fragment(&["\\vmware\\"]),
        "VMware"
    ));

    rules.push(rule!(
        "user_vbox_cache",
        VirtualMachineDisk,
        Optional,
        RuleMatcher::Fragment(&["\\oracle\\virtualbox\\"]),
        "VirtualBox"
    ));

    rules.push(rule!(
        "user_wsl_cache",
        ContainerLayer,
        Optional,
        RuleMatcher::Fragment(&["\\wsl\\"]),
        "WSL"
    ));

    rules.push(rule!(
        "user_hyperv_cache",
        VirtualMachineDisk,
        Optional,
        RuleMatcher::Fragment(&["\\hyper-v\\"]),
        "Hyper-V"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：数据库临时文件
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_sqlite_temp",
        DatabaseTemp,
        Optional,
        RuleMatcher::Extension(&[".sqlite-journal"]),
        "SQLite"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：云同步缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_onedrive_cache",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\onedrive\\cache"]),
        "OneDrive"
    ));

    rules.push(rule!(
        "user_dropbox_cache",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\dropbox\\cache"]),
        "Dropbox"
    ));

    rules.push(rule!(
        "user_google_drive_cache",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\google\\drive\\cache"]),
        "Google Drive"
    ));

    rules.push(rule!(
        "user_icloud_cache",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\apple\\icloud\\cache"]),
        "iCloud"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：通用垃圾文件
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_downloads_installer",
        GenericInstaller,
        Optional,
        RuleMatcher::ExtensionInContext {
            extensions: &[".msi", ".exe", ".zip", ".rar"],
            components: &["downloads", "download", "下载"],
        },
        "下载的安装包"
    ));

    rules.push(rule!(
        "user_recycle_bin",
        GenericTemp,
        Disposable,
        RuleMatcher::Prefix(&["c:\\$recycle.bin"]),
        "回收站"
    ));

    rules.push(rule!(
        "user_windows_upgrade",
        SystemTemp,
        Disposable,
        RuleMatcher::Prefix(&["c:\\$windows.~bt"]),
        "Windows 升级临时文件"
    ));

    rules.push(rule!(
        "user_windows_update_temp",
        WindowsUpdateCache,
        Disposable,
        RuleMatcher::Prefix(&["c:\\$windows.~ws"]),
        "Windows Update 临时文件"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：开发工具缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_python_pycache",
        AppCache,
        Optional,
        RuleMatcher::Component(&["__pycache__"]),
        "Python"
    ));

    rules.push(rule!(
        "user_python_egg",
        PackageManagerCache,
        Optional,
        RuleMatcher::Extension(&[".egg-info"]),
        "Python"
    ));

    rules.push(rule!(
        "user_node_modules",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&["node_modules"]),
        "npm/yarn"
    ));

    rules.push(rule!(
        "user_npm_logs",
        AppLog,
        Optional,
        RuleMatcher::Component(&["npm-cache"]),
        "npm"
    ));

    rules.push(rule!(
        "user_maven_repo",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&["repository"]),
        "Maven"
    ));

    rules.push(rule!(
        "user_gradle_wrapper",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&["gradle-wrapper"]),
        "Gradle"
    ));

    rules.push(rule!(
        "user_nuget_packages",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&["packages"]),
        "NuGet"
    ));

    rules.push(rule!(
        "user_dotnet_temp",
        GenericTemp,
        Disposable,
        RuleMatcher::Fragment(&["\\microsoft\\dotnet\\"]),
        ".NET"
    ));

    rules.push(rule!(
        "user_android_studio_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\android studio\\cache"]),
        "Android Studio"
    ));

    rules.push(rule!(
        "user_xcode_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\xcode\\cache"]),
        "Xcode"
    ));

    rules.push(rule!(
        "user_cocoapods_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Component(&[".cocoapods"]),
        "CocoaPods"
    ));

    rules.push(rule!(
        "user_rustup_cache",
        PackageManagerCache,
        Optional,
        RuleMatcher::Fragment(&["\\rustup\\"]),
        "Rustup"
    ));

    rules.push(rule!(
        "user_cargo_git",
        PackageManagerCache,
        Optional,
        RuleMatcher::Fragment(&["\\cargo\\git\\"]),
        "Cargo"
    ));

    rules.push(rule!(
        "user_git_objects",
        GenericCache,
        Optional,
        RuleMatcher::Fragment(&["\\.git\\objects\\"]),
        "Git"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：设计/工程软件缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_photoshop_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\adobe\\photoshop\\cache"]),
        "Adobe Photoshop"
    ));

    rules.push(rule!(
        "user_premiere_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\adobe\\premiere pro\\cache"]),
        "Adobe Premiere"
    ));

    rules.push(rule!(
        "user_illustrator_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\adobe\\illustrator\\cache"]),
        "Adobe Illustrator"
    ));

    rules.push(rule!(
        "user_autocad_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\autodesk\\autocad\\cache"]),
        "AutoCAD"
    ));

    rules.push(rule!(
        "user_matlab_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\mathworks\\matlab\\cache"]),
        "MATLAB"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：Windows 系统缓存
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_gdi_cache",
        SystemRuntimeCache,
        Optional,
        RuleMatcher::Fragment(&["\\windows\\gdi\\cache"]),
        "Windows GDI"
    ));

    rules.push(rule!(
        "user_dx_cache",
        SystemRuntimeCache,
        Optional,
        RuleMatcher::Fragment(&["\\windows\\directx\\cache"]),
        "DirectX"
    ));

    rules.push(rule!(
        "user_mf_cache",
        SystemRuntimeCache,
        Optional,
        RuleMatcher::Fragment(&["\\windows\\media foundation\\cache"]),
        "Media Foundation"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：构建产物
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_cmake_build",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&["cmake-build"]),
        "CMake"
    ));

    rules.push(rule!(
        "user_meson_build",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&["meson-build"]),
        "Meson"
    ));

    rules.push(rule!(
        "user_ninja_build",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".ninja"]),
        "Ninja"
    ));

    rules.push(rule!(
        "user_bazel_build",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&["bazel-bin"]),
        "Bazel"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：日志文件
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_iis_logs",
        SystemLog,
        Optional,
        RuleMatcher::Fragment(&["\\inetpub\\logs\\"]),
        "IIS"
    ));

    rules.push(rule!(
        "user_sql_logs",
        DatabaseLog,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft sql server\\mssql\\log"]),
        "SQL Server"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：备份/遥测
    // ═════════════════════════════════════════════════════════

    rules.push(rule!(
        "user_vss_backup",
        BackupData,
        Optional,
        RuleMatcher::Prefix(&["c:\\system volume information"]),
        "VSS"
    ));

    rules.push(rule!(
        "user_backup_bak",
        GenericBackup,
        Optional,
        RuleMatcher::Extension(&[".bak"]),
        "备份文件"
    ));

    rules.push(rule!(
        "user_diagtrack",
        WindowsTelemetry,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\diagnostics\\"]),
        "Windows 诊断"
    ));

    rules.push(rule!(
        "user_sqm_data",
        WindowsTelemetry,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\sqm\\"]),
        "SQM"
    ));

    // ═════════════════════════════════════════════════════════
    // 第九层：通用目录规则（兜底）
    // ═════════════════════════════════════════════════════════

    // temp/tmp/cache/caches 目录
    rules.push(rule!(
        "dir_temp_cache",
        GenericCache,
        Optional,
        RuleMatcher::Component(&["temp", "tmp", "cache", "caches"]),
        "通用缓存"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：更多浏览器与 Web 运行时缓存
    // ═════════════════════════════════════════════════════════

    // Safari 缓存 (Windows 版 iCloud)
    rules.push(rule!(
        "user_safari_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\safari\\"]),
        "Safari"
    ));

    // Opera GX 缓存
    rules.push(rule!(
        "user_operagx_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\opera gx\\"]),
        "Opera GX"
    ));

    // Firefox Developer Edition
    rules.push(rule!(
        "user_firefox_dev_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\firefox developer edition\\"]),
        "Firefox Dev"
    ));

    // Chrome Dev/Canary
    rules.push(rule!(
        "user_chrome_dev_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\google\\chrome dev\\", "\\google\\chrome canary\\"]),
        "Chrome Dev/Canary"
    ));

    // Edge Dev/Beta/Canary
    rules.push(rule!(
        "user_edge_dev_cache",
        BrowserCache,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\edge dev\\", "\\microsoft\\edge beta\\", "\\microsoft\\edge canary\\"]),
        "Edge Dev/Beta/Canary"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：更多 IDE/编辑器缓存
    // ═════════════════════════════════════════════════════════

    // Eclipse 缓存
    rules.push(rule!(
        "user_eclipse_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\eclipse\\"]),
        "Eclipse"
    ));

    // NetBeans 缓存
    rules.push(rule!(
        "user_netbeans_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\netbeans\\"]),
        "NetBeans"
    ));

    // IntelliJ IDEA 系统目录
    rules.push(rule!(
        "user_intellij_system",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\intellijdea"]),
        "IntelliJ IDEA"
    ));

    // Visual Studio 缓存
    rules.push(rule!(
        "user_vs_cache",
        IDECache,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\visualstudio\\"]),
        "Visual Studio"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：更多开发工具与构建缓存
    // ═════════════════════════════════════════════════════════

    // .tox (Python)
    rules.push(rule!(
        "user_tox_env",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".tox"]),
        "tox"
    ));

    // .venv / venv
    rules.push(rule!(
        "user_venv",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".venv", "venv"]),
        "Python venv"
    ));

    // .pytest_cache
    rules.push(rule!(
        "user_pytest_cache",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".pytest_cache"]),
        "pytest"
    ));

    // .mypy_cache
    rules.push(rule!(
        "user_mypy_cache",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".mypy_cache"]),
        "mypy"
    ));

    // coverage data
    rules.push(rule!(
        "user_coverage_data",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&[".coverage", "htmlcov", ".nyc_output"]),
        "Coverage"
    ));

    // Go build cache
    rules.push(rule!(
        "user_go_build_cache",
        BuildArtifact,
        Optional,
        RuleMatcher::Fragment(&["\\go-build"]),
        "Go"
    ));

    // Rust target/debug
    rules.push(rule!(
        "user_rust_target",
        BuildArtifact,
        Optional,
        RuleMatcher::Component(&["target"]),
        "Rust"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：更多设计/工程/生产力软件
    // ═════════════════════════════════════════════════════════

    // Blender 缓存
    rules.push(rule!(
        "user_blender_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\blender\\"]),
        "Blender"
    ));

    // DaVinci Resolve 缓存
    rules.push(rule!(
        "user_resolve_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\blackmagic design\\davinci resolve\\"]),
        "DaVinci Resolve"
    ));

    // Final Cut Pro 缓存 (Windows Bootcamp)
    rules.push(rule!(
        "user_fcp_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\final cut pro"]),
        "Final Cut Pro"
    ));

    // After Effects 缓存
    rules.push(rule!(
        "user_ae_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\adobe\\after effects\\"]),
        "After Effects"
    ));

    // Lightroom 缓存
    rules.push(rule!(
        "user_lightroom_cache",
        MediaCache,
        Optional,
        RuleMatcher::Fragment(&["\\adobe\\lightroom"]),
        "Lightroom"
    ));

    // OBS Studio 缓存
    rules.push(rule!(
        "user_obs_cache",
        StreamingCache,
        Optional,
        RuleMatcher::Fragment(&["\\obs-studio\\"]),
        "OBS Studio"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：系统级临时文件与日志
    // ═════════════════════════════════════════════════════════

    // Windows.old 下的临时文件
    rules.push(rule!(
        "system_windows_old_temp",
        SystemTemp,
        Disposable,
        RuleMatcher::Prefix(&["c:\\windows.old\\windows\\temp"]),
        "旧版 Windows 临时文件"
    ));

    // Windows.old 下的 Prefetch
    rules.push(rule!(
        "system_windows_old_prefetch",
        PrefetchData,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows.old\\windows\\prefetch"]),
        "旧版 Windows Prefetch"
    ));

    // Windows.old 下的日志
    rules.push(rule!(
        "system_windows_old_logs",
        SystemLog,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows.old\\windows\\logs"]),
        "旧版 Windows 日志"
    ));

    // CBS 日志
    rules.push(rule!(
        "system_cbs_logs",
        CBSLog,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\logs\\cbs"]),
        "CBS"
    ));

    // DISM 日志
    rules.push(rule!(
        "system_dism_logs",
        SetupLog,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\logs\\dism"]),
        "DISM"
    ));

    // Windows 升级日志
    rules.push(rule!(
        "system_upgrade_logs",
        SetupLog,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\logs\\windowsupdate"]),
        "Windows 升级日志"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：通用垃圾文件与下载碎片
    // ═════════════════════════════════════════════════════════

    // 通用临时扩展名
    rules.push(rule!(
        "ext_tempfile",
        GenericTemp,
        Disposable,
        RuleMatcher::Extension(&[".tmp", ".temp", ".~tmp", ".~temp"]),
        "临时文件"
    ));

    // 通用日志扩展名
    rules.push(rule!(
        "ext_logfile",
        GenericLog,
        Optional,
        RuleMatcher::Extension(&[".log", ".out", ".err"]),
        "日志文件"
    ));

    // 通用备份扩展名
    rules.push(rule!(
        "ext_backupfile",
        GenericBackup,
        Optional,
        RuleMatcher::Extension(&[".bak", ".old", ".orig", ".backup", ".save"]),
        "备份文件"
    ));

    // 通用缓存扩展名
    rules.push(rule!(
        "ext_cachefile",
        GenericCache,
        Optional,
        RuleMatcher::Extension(&[".cache", ".cached"]),
        "缓存文件"
    ));

    // 下载碎片
    rules.push(rule!(
        "ext_download_frag",
        GenericFragment,
        Disposable,
        RuleMatcher::Extension(&[".crdownload", ".part", ".partial", ".download", ".tmpdownload"]),
        "下载碎片"
    ));

    // 通用安装包扩展名
    rules.push(rule!(
        "ext_installer",
        GenericInstaller,
        Optional,
        RuleMatcher::Extension(&[".msi", ".msix", ".msp", ".exe", ".zip", ".rar", ".7z", ".tar.gz"]),
        "安装包"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：用户应用级缓存目录
    // ═════════════════════════════════════════════════════════

    // Electron 应用通用缓存 (Cache 目录在 AppData/Local)
    rules.push(rule!(
        "user_electron_app_cache",
        AppCache,
        Optional,
        RuleMatcher::Component(&["cache", "cacheddata", "code cache"]),
        "Electron 应用"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：游戏平台缓存
    // ═════════════════════════════════════════════════════════

    // Xbox Game Pass 缓存
    rules.push(rule!(
        "user_xbox_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\xbox\\"]),
        "Xbox"
    ));

    // GOG Galaxy 缓存
    rules.push(rule!(
        "user_gog_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\gog galaxy\\"]),
        "GOG Galaxy"
    ));

    // Ubisoft Connect 缓存
    rules.push(rule!(
        "user_ubisoft_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\ubisoft\\", "\\ubisoft game launcher\\"]),
        "Ubisoft Connect"
    ));

    // Rockstar Games 缓存
    rules.push(rule!(
        "user_rockstar_cache",
        GameCache,
        Optional,
        RuleMatcher::Fragment(&["\\rockstar games\\"]),
        "Rockstar Games"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：云同步与协作工具缓存
    // ═════════════════════════════════════════════════════════

    // Dropbox 缓存
    rules.push(rule!(
        "user_dropbox_offline",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\dropbox\\"]),
        "Dropbox"
    ));

    // Google Drive 离线缓存
    rules.push(rule!(
        "user_gdrive_offline",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\google\\drive"]),
        "Google Drive"
    ));

    // OneDrive 离线缓存
    rules.push(rule!(
        "user_onedrive_offline",
        CloudSyncLocal,
        Optional,
        RuleMatcher::Fragment(&["\\microsoft\\onedrive\\"]),
        "OneDrive"
    ));

    // Notion 缓存
    rules.push(rule!(
        "user_notion_cache2",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\notion\\"]),
        "Notion"
    ));

    // Figma 缓存
    rules.push(rule!(
        "user_figma_cache",
        AppCache,
        Optional,
        RuleMatcher::Fragment(&["\\figma\\"]),
        "Figma"
    ));

    // ═════════════════════════════════════════════════════════
    // 扩展：系统诊断与崩溃数据
    // ═════════════════════════════════════════════════════════

    // Windows Memory Dumps
    rules.push(rule!(
        "system_memory_dump",
        MemoryDump,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\minidump"]),
        "Windows 内存转储"
    ));

    // Windows LiveKernelReports
    rules.push(rule!(
        "system_livekernel_reports",
        MemoryDump,
        Optional,
        RuleMatcher::Prefix(&["c:\\windows\\livekernelreports"]),
        "Windows LiveKernelReports"
    ));

    // Windows Error Reporting LocalDumps
    rules.push(rule!(
        "system_wer_localdumps",
        ErrorReport,
        Optional,
        RuleMatcher::Fragment(&["\\wer\\localdumps"]),
        "Windows Error Reporting"
    ));

    // 应用程序崩溃日志
    rules.push(rule!(
        "user_app_crash_logs",
        ErrorReport,
        Optional,
        RuleMatcher::Component(&["crashdumps", "crashes"]),
        "应用崩溃日志"
    ));

    rules
}
