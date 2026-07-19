//! 文件分类体系
//!
//! 定义文件的系统分层、细分类别和重要性等级。

use std::path::PathBuf;

// ============================================================
// 系统分层（6 层模型）
// ============================================================

/// Windows 系统中文件的所属层级，从内核到用户空间。
///
/// 层级越低，越靠近系统核心，删除风险越高。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum SystemLayer {
    /// 内核与核心组件，删除会导致系统崩溃或无法启动。
    /// e.g. C:\Windows\System32\kernel32.dll, WinSxS
    Kernel,
    // 系统服务与驱动，删除会导致特定服务崩溃或硬件无法工作。
    /// e.g. 驱动文件、服务可执行文件
    SystemService,
    // 运行时缓存与临时数据，删除影响性能但通常不破坏功能。
    /// e.g. Prefetch、Windows Update 下载缓存
    SystemRuntime,
    // 系统配置与组件存储，删除会影响系统功能或组件管理。
    /// e.g. MSI 安装包缓存、WinSxS manifest
    SystemConfig,
    // 已安装应用本体及其数据，删除会导致应用无法运行。
    /// e.g. Chrome 可执行文件、VS Code 配置
    Application,
    // 用户文档、媒体、下载等用户创建的数据。
    /// e.g. 文档、图片、下载的安装包
    UserSpace,
}

impl SystemLayer {
    /// 层级排名，越低越核心（数值越小越核心）。
    pub fn rank(self) -> u8 {
        match self {
            Self::Kernel => 0,
            Self::SystemService => 1,
            Self::SystemConfig => 2,
            Self::SystemRuntime => 3,
            Self::Application => 4,
            Self::UserSpace => 5,
        }
    }

    /// 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Kernel => "系统内核",
            Self::SystemService => "系统服务",
            Self::SystemConfig => "系统配置",
            Self::SystemRuntime => "系统运行时",
            Self::Application => "应用层",
            Self::UserSpace => "用户空间",
        }
    }
}

// ============================================================
// 文件分类（200+ 种细分类别）
// ============================================================

/// 细粒度的文件分类，覆盖 Windows 10/11 各类文件。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FileCategory {
    // ─── 系统内核层（20 种）───
    SystemBinary,        // 系统核心二进制（kernel32.dll, ntdll.dll）
    SystemDriver,        // 内核驱动文件（.sys）
    ComponentStore,      // WinSxS 组件存储
    InstallerCache,      // Windows Installer 缓存（C:\Windows\Installer）
    BootLoader,          // 启动引导文件（BCD、bootmgr）
    BootConfiguration,   // 启动配置文件（BCD store）
    UefiFirmware,        // UEFI 固件文件
    RegistryHive,        // 注册表配置单元（.reg, .hiv）
    SystemModule,        // 系统模块（.dll, .exe）
    SystemManifest,      // 组件清单（.manifest）
    DllCache,            // DLL 缓存
    FontCache,           // 字体缓存
    BootFile,            // 其他启动文件
    SystemResource,      // 系统资源（.mui, .res）
    SystemMetadata,      // 系统元数据
    TrustedInstallerFile,// TrustedInstaller 管理的文件
    WinSxSBinary,        // WinSxS 中的二进制
    WinSxSManifest,      // WinSxS 中的清单
    CoreSystemFile,      // 其他核心系统文件
    KernelModule,        // 内核模块

    // ─── 系统服务层（15 种）───
    ServiceExecutable,   // 服务可执行文件
    ServiceData,         // 服务数据文件
    DriverStore,         // 驱动商店（DriverStore）
    PrintSpooler,        // 打印假脱机数据
    AntivirusDefinition, // 杀毒软件定义文件
    WindowsService,      // Windows 服务相关文件
    SystemServiceConfig, // 服务配置文件
    FirewallRule,        // 防火墙规则
    PolicyFile,          // 策略文件
    SystemServiceDll,    // 服务 DLL
    ServiceLog,          // 服务日志
    SystemServiceRegistry,// 服务注册表相关
    EventLogData,        // 事件日志数据
    ServicePipe,         // 命名管道
    SystemServiceCache,  // 服务缓存

    // ─── 系统运行时（25 种）───
    SystemTemp,          // 系统临时文件（C:\Windows\Temp）
    PrefetchData,        // Prefetch 预读数据
    SuperfetchData,      // Superfetch 数据
    WindowsUpdateCache,  // Windows Update 下载缓存
    DeliveryOptimization,// 传递优化缓存
    ErrorReport,         // Windows Error Reporting 报告
    SystemLog,           // 系统日志
    EventTrace,          // ETW 事件追踪（.etl）
    MemoryDump,          // 内存转储（.dmp）
    PageFile,            // 页面文件（pagefile.sys）
    HibernationFile,     // 休眠文件（hiberfil.sys）
    SystemRestorePoint,  // 系统还原点
    ThumbnailCache,      // 缩略图缓存
    IconCache,           // 图标缓存
    SearchIndex,         // Windows Search 索引
    WindowsLog,          // Windows 日志目录
    CBSLog,              // CBS 组件服务日志
    SetupLog,            // 安装日志
    CrashDump,           // 崩溃转储
    WindowsErrorLog,     // Windows 错误日志
    DiagnosticsLog,      // 诊断日志
    SystemPerformanceLog,// 性能日志
    WindowsTelemetry,    // 遥测数据
    WindowsDiagnostic,   // 诊断数据
    SystemRuntimeCache,  // 系统运行时缓存（通用）
    SystemRuntimeData,   // 系统运行时数据（通用）

    // ─── 系统配置（15 种）───
    WinSxSManifestFile,  // WinSxS 清单文件
    MsiManifest,         // MSI 清单
    SymlinkTarget,       // 符号链接目标
    JunctionTarget,      // 联接点目标
    GroupPolicy,         // 组策略文件
    SystemPolicy,        // 系统策略文件
    SecurityPolicy,      // 安全策略文件
    RegistryBackup,      // 注册表备份
    SystemConfigData,    // 系统配置数据
    WindowsActivation,   // Windows 激活数据
    LicensingData,       // 授权数据
    ComponentConfig,     // 组件配置
    FeatureStore,        // 功能存储
    SystemSetting,       // 系统设置
    SystemConfigCache,   // 系统配置缓存

    // ─── 应用本体（10 种）───
    ApplicationBinary,   // 应用可执行文件
    AppModule,           // 应用模块（DLL）
    AppPlugin,           // 应用插件
    AppExtension,        // 应用扩展
    AppResource,         // 应用资源
    AppData,             // 应用数据（通用）
    AppLibrary,          // 应用库文件
    AppFramework,        // 应用框架
    AppLauncher,         // 应用启动器
    AppInstaller,        // 应用安装包

    // ─── 应用数据（30 种）───
    AppConfig,           // 应用配置文件
    AppDatabase,         // 应用数据库
    AppLog,              // 应用日志
    AppCache,            // 应用缓存
    AppTemp,             // 应用临时文件
    BrowserCache,        // 浏览器缓存
    BrowserCookie,       // 浏览器 Cookie
    BrowserHistory,       // 浏览器历史记录
    BrowserExtensionData,// 浏览器扩展数据
    PackageManagerCache, // 包管理器缓存
    BuildArtifact,       // 构建产物
    DevToolCache,        // 开发工具缓存
    MediaCache,          // 媒体缓存
    IDEWorkspace,        // IDE 工作区
    IDEIndex,            // IDE 索引
    IDECache,            // IDE 缓存
    ContainerLayer,      // 容器镜像层
    VirtualMachineDisk,  // 虚拟机磁盘
    DatabaseData,        // 数据库数据文件
    DatabaseLog,         // 数据库日志
    DatabaseTemp,        // 数据库临时文件
    GameCache,           // 游戏缓存
    GameSave,            // 游戏存档
    ChatLog,             // 聊天日志
    ChatMedia,           // 聊天媒体文件
    CloudSyncLocal,      // 云同步本地副本
    OfficeCache,         // Office 缓存
    OfficeTemp,          // Office 临时文件
    StreamingCache,      // 流媒体缓存
    DownloadManagerData, // 下载管理器数据

    // ─── 用户数据（20 种）───
    UserDocument,        // 用户文档
    UserMedia,           // 用户媒体（图片/视频/音乐）
    UserDownload,        // 用户下载
    UserTemp,            // 用户临时文件
    UserProfileData,     // 用户配置文件
    CloudSyncData,       // 云同步数据
    BackupData,          // 备份数据
    UserTemplate,        // 用户模板文件
    UserCertificate,     // 用户证书
    UserCredential,      // 用户凭据
    UserBookmark,        // 用户书签
    UserHistory,         // 用户历史记录
    UserPreference,      // 用户偏好设置
    UserTheme,           // 用户主题
    UserFont,            // 用户字体
    UserMacro,           // 用户宏/脚本
    UserShortcut,        // 用户快捷方式
    UserEmail,           // 用户邮件
    UserContact,         // 用户联系人
    UserCalendar,        // 用户日历

    // ─── 通用（10 种）───
    Unknown,             // 未知类型
    GenericTemp,         // 通用临时文件
    GenericLog,          // 通用日志文件
    GenericCache,        // 通用缓存文件
    GenericBackup,       // 通用备份文件
    GenericInstaller,    // 通用安装包
    GenericDownload,     // 通用下载文件
    GenericFragment,     // 通用文件碎片
    GenericArtifact,     // 通用构建产物
    Other,               // 其他
}

impl FileCategory {
    /// 将分类映射到所属系统层。
    pub fn layer(self) -> SystemLayer {
        match self {
            // 内核层
            Self::SystemBinary | Self::SystemDriver | Self::ComponentStore
            | Self::InstallerCache | Self::BootLoader | Self::BootConfiguration
            | Self::UefiFirmware | Self::RegistryHive | Self::SystemModule
            | Self::SystemManifest | Self::DllCache | Self::FontCache
            | Self::BootFile | Self::SystemResource | Self::SystemMetadata
            | Self::TrustedInstallerFile | Self::WinSxSBinary | Self::WinSxSManifest
            | Self::CoreSystemFile | Self::KernelModule => SystemLayer::Kernel,

            // 服务层
            Self::ServiceExecutable | Self::ServiceData | Self::DriverStore
            | Self::PrintSpooler | Self::AntivirusDefinition | Self::WindowsService
            | Self::SystemServiceConfig | Self::FirewallRule | Self::PolicyFile
            | Self::SystemServiceDll | Self::ServiceLog | Self::SystemServiceRegistry
            | Self::EventLogData | Self::ServicePipe | Self::SystemServiceCache => {
                SystemLayer::SystemService
            }

            // 配置层
            Self::WinSxSManifestFile | Self::MsiManifest | Self::SymlinkTarget
            | Self::JunctionTarget | Self::GroupPolicy | Self::SystemPolicy
            | Self::SecurityPolicy | Self::RegistryBackup | Self::SystemConfigData
            | Self::WindowsActivation | Self::LicensingData | Self::ComponentConfig
            | Self::FeatureStore | Self::SystemSetting | Self::SystemConfigCache => {
                SystemLayer::SystemConfig
            }

            // 运行时层
            Self::SystemTemp | Self::PrefetchData | Self::SuperfetchData
            | Self::WindowsUpdateCache | Self::DeliveryOptimization | Self::ErrorReport
            | Self::SystemLog | Self::EventTrace | Self::MemoryDump | Self::PageFile
            | Self::HibernationFile | Self::SystemRestorePoint | Self::ThumbnailCache
            | Self::IconCache | Self::SearchIndex | Self::WindowsLog | Self::CBSLog
            | Self::SetupLog | Self::CrashDump | Self::WindowsErrorLog
            | Self::DiagnosticsLog | Self::SystemPerformanceLog | Self::WindowsTelemetry
            | Self::WindowsDiagnostic | Self::SystemRuntimeCache | Self::SystemRuntimeData => {
                SystemLayer::SystemRuntime
            }

            // 应用本体层
            Self::ApplicationBinary | Self::AppModule | Self::AppPlugin
            | Self::AppExtension | Self::AppResource | Self::AppData
            | Self::AppLibrary | Self::AppFramework | Self::AppLauncher
            | Self::AppInstaller => SystemLayer::Application,

            // 应用数据层
            Self::AppConfig | Self::AppDatabase | Self::AppLog | Self::AppCache
            | Self::AppTemp | Self::BrowserCache | Self::BrowserCookie
            | Self::BrowserHistory | Self::BrowserExtensionData | Self::PackageManagerCache
            | Self::BuildArtifact | Self::DevToolCache | Self::MediaCache
            | Self::IDEWorkspace | Self::IDEIndex | Self::IDECache | Self::ContainerLayer
            | Self::VirtualMachineDisk | Self::DatabaseData | Self::DatabaseLog
            | Self::DatabaseTemp | Self::GameCache | Self::GameSave | Self::ChatLog
            | Self::ChatMedia | Self::CloudSyncLocal | Self::OfficeCache
            | Self::OfficeTemp | Self::StreamingCache | Self::DownloadManagerData => {
                SystemLayer::Application
            }

            // 用户空间层
            Self::UserDocument | Self::UserMedia | Self::UserDownload | Self::UserTemp
            | Self::UserProfileData | Self::CloudSyncData | Self::BackupData
            | Self::UserTemplate | Self::UserCertificate | Self::UserCredential
            | Self::UserBookmark | Self::UserHistory | Self::UserPreference
            | Self::UserTheme | Self::UserFont | Self::UserMacro | Self::UserShortcut
            | Self::UserEmail | Self::UserContact | Self::UserCalendar => {
                SystemLayer::UserSpace
            }

            // 通用 → 根据上下文判断，默认用户空间
            _ => SystemLayer::UserSpace,
        }
    }

    /// 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            // 内核层
            Self::SystemBinary => "系统二进制",
            Self::SystemDriver => "驱动程序",
            Self::ComponentStore => "组件存储",
            Self::InstallerCache => "安装包缓存",
            Self::BootLoader => "启动引导",
            Self::BootConfiguration => "启动配置",
            Self::UefiFirmware => "UEFI 固件",
            Self::RegistryHive => "注册表配置单元",
            Self::SystemModule => "系统模块",
            Self::SystemManifest => "系统清单",
            Self::DllCache => "DLL 缓存",
            Self::FontCache => "字体缓存",
            Self::BootFile => "启动文件",
            Self::SystemResource => "系统资源",
            Self::SystemMetadata => "系统元数据",
            Self::TrustedInstallerFile => "TrustedInstaller 文件",
            Self::WinSxSBinary => "WinSxS 二进制",
            Self::WinSxSManifest => "WinSxS 清单",
            Self::CoreSystemFile => "核心系统文件",
            Self::KernelModule => "内核模块",

            // 服务层
            Self::ServiceExecutable => "服务可执行文件",
            Self::ServiceData => "服务数据",
            Self::DriverStore => "驱动商店",
            Self::PrintSpooler => "打印假脱机",
            Self::AntivirusDefinition => "杀毒定义",
            Self::WindowsService => "Windows 服务",
            Self::SystemServiceConfig => "服务配置",
            Self::FirewallRule => "防火墙规则",
            Self::PolicyFile => "策略文件",
            Self::SystemServiceDll => "服务 DLL",
            Self::ServiceLog => "服务日志",
            Self::SystemServiceRegistry => "服务注册表",
            Self::EventLogData => "事件日志数据",
            Self::ServicePipe => "服务管道",
            Self::SystemServiceCache => "服务缓存",

            // 运行时层
            Self::SystemTemp => "系统临时文件",
            Self::PrefetchData => "Prefetch 数据",
            Self::SuperfetchData => "Superfetch 数据",
            Self::WindowsUpdateCache => "Windows Update 缓存",
            Self::DeliveryOptimization => "传递优化缓存",
            Self::ErrorReport => "错误报告",
            Self::SystemLog => "系统日志",
            Self::EventTrace => "事件追踪",
            Self::MemoryDump => "内存转储",
            Self::PageFile => "页面文件",
            Self::HibernationFile => "休眠文件",
            Self::SystemRestorePoint => "系统还原点",
            Self::ThumbnailCache => "缩略图缓存",
            Self::IconCache => "图标缓存",
            Self::SearchIndex => "搜索索引",
            Self::WindowsLog => "Windows 日志",
            Self::CBSLog => "CBS 日志",
            Self::SetupLog => "安装日志",
            Self::CrashDump => "崩溃转储",
            Self::WindowsErrorLog => "Windows 错误日志",
            Self::DiagnosticsLog => "诊断日志",
            Self::SystemPerformanceLog => "性能日志",
            Self::WindowsTelemetry => "遥测数据",
            Self::WindowsDiagnostic => "诊断数据",
            Self::SystemRuntimeCache => "运行时缓存",
            Self::SystemRuntimeData => "运行时数据",

            // 配置层
            Self::WinSxSManifestFile => "WinSxS 清单文件",
            Self::MsiManifest => "MSI 清单",
            Self::SymlinkTarget => "符号链接目标",
            Self::JunctionTarget => "联接点目标",
            Self::GroupPolicy => "组策略",
            Self::SystemPolicy => "系统策略",
            Self::SecurityPolicy => "安全策略",
            Self::RegistryBackup => "注册表备份",
            Self::SystemConfigData => "系统配置数据",
            Self::WindowsActivation => "Windows 激活",
            Self::LicensingData => "授权数据",
            Self::ComponentConfig => "组件配置",
            Self::FeatureStore => "功能存储",
            Self::SystemSetting => "系统设置",
            Self::SystemConfigCache => "配置缓存",

            // 应用本体层
            Self::ApplicationBinary => "应用可执行文件",
            Self::AppModule => "应用模块",
            Self::AppPlugin => "应用插件",
            Self::AppExtension => "应用扩展",
            Self::AppResource => "应用资源",
            Self::AppData => "应用数据",
            Self::AppLibrary => "应用库",
            Self::AppFramework => "应用框架",
            Self::AppLauncher => "应用启动器",
            Self::AppInstaller => "应用安装包",

            // 应用数据层
            Self::AppConfig => "应用配置",
            Self::AppDatabase => "应用数据库",
            Self::AppLog => "应用日志",
            Self::AppCache => "应用缓存",
            Self::AppTemp => "应用临时文件",
            Self::BrowserCache => "浏览器缓存",
            Self::BrowserCookie => "浏览器 Cookie",
            Self::BrowserHistory => "浏览器历史",
            Self::BrowserExtensionData => "浏览器扩展数据",
            Self::PackageManagerCache => "包管理器缓存",
            Self::BuildArtifact => "构建产物",
            Self::DevToolCache => "开发工具缓存",
            Self::MediaCache => "媒体缓存",
            Self::IDEWorkspace => "IDE 工作区",
            Self::IDEIndex => "IDE 索引",
            Self::IDECache => "IDE 缓存",
            Self::ContainerLayer => "容器镜像层",
            Self::VirtualMachineDisk => "虚拟机磁盘",
            Self::DatabaseData => "数据库数据",
            Self::DatabaseLog => "数据库日志",
            Self::DatabaseTemp => "数据库临时文件",
            Self::GameCache => "游戏缓存",
            Self::GameSave => "游戏存档",
            Self::ChatLog => "聊天日志",
            Self::ChatMedia => "聊天媒体",
            Self::CloudSyncLocal => "云同步本地副本",
            Self::OfficeCache => "Office 缓存",
            Self::OfficeTemp => "Office 临时文件",
            Self::StreamingCache => "流媒体缓存",
            Self::DownloadManagerData => "下载管理器数据",

            // 用户空间层
            Self::UserDocument => "用户文档",
            Self::UserMedia => "用户媒体",
            Self::UserDownload => "用户下载",
            Self::UserTemp => "用户临时文件",
            Self::UserProfileData => "用户配置",
            Self::CloudSyncData => "云同步数据",
            Self::BackupData => "备份数据",
            Self::UserTemplate => "用户模板",
            Self::UserCertificate => "用户证书",
            Self::UserCredential => "用户凭据",
            Self::UserBookmark => "用户书签",
            Self::UserHistory => "用户历史",
            Self::UserPreference => "用户偏好",
            Self::UserTheme => "用户主题",
            Self::UserFont => "用户字体",
            Self::UserMacro => "用户宏",
            Self::UserShortcut => "用户快捷方式",
            Self::UserEmail => "用户邮件",
            Self::UserContact => "用户联系人",
            Self::UserCalendar => "用户日历",

            // 通用层
            Self::Unknown => "未知",
            Self::GenericTemp => "通用临时文件",
            Self::GenericLog => "通用日志",
            Self::GenericCache => "通用缓存",
            Self::GenericBackup => "通用备份",
            Self::GenericInstaller => "通用安装包",
            Self::GenericDownload => "通用下载",
            Self::GenericFragment => "文件碎片",
            Self::GenericArtifact => "构建产物",
            Self::Other => "其他",
        }
    }
}

// ============================================================
// 文件重要性等级
// ============================================================

/// 文件对系统或应用的重要性，删除后的影响程度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum FileImportance {
    /// 删除导致系统无法启动/蓝屏/崩溃。
    /// e.g. kernel32.dll、WinSxS 核心组件
    Critical,
    // 删除导致应用无法运行/功能损坏/数据丢失。
    /// e.g. Chrome 配置文件、应用数据库
    Essential,
    // 删除导致性能下降，但功能正常。
    /// e.g. Prefetch、浏览器缓存
    Performance,
    // 删除后自动重建，无持久影响。
    /// e.g. 缩略图缓存、临时文件
    Optional,
    // 安全删除，无任何影响。
    /// e.g. Windows Update 下载缓存、下载碎片
    Disposable,
}

impl FileImportance {
    /// 基础风险分数（0-100）。
    pub fn base_score(self) -> u8 {
        match self {
            Self::Critical => 100,
            Self::Essential => 70,
            Self::Performance => 40,
            Self::Optional => 20,
            Self::Disposable => 5,
        }
    }

    /// 中文标签。
    pub fn label(self) -> &'static str {
        match self {
            Self::Critical => "关键",
            Self::Essential => "重要",
            Self::Performance => "性能",
            Self::Optional => "可选",
            Self::Disposable => "可丢弃",
        }
    }

    /// 删除影响描述。
    pub fn impact_description(self) -> &'static str {
        match self {
            Self::Critical => "删除会导致系统无法启动或崩溃",
            Self::Essential => "删除会导致应用无法运行或数据丢失",
            Self::Performance => "删除后性能可能下降，但功能正常",
            Self::Optional => "删除后自动重建，无持久影响",
            Self::Disposable => "安全删除，无任何影响",
        }
    }
}

// ============================================================
// 文件分类结果
// ============================================================

/// 单个文件/目录的完整分类信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FileClassification {
    pub path: PathBuf,
    pub layer: SystemLayer,
    pub category: FileCategory,
    /// 所属应用/组件名。
    pub owner: Option<String>,
    /// 文件在应用中的作用描述。
    pub role_in_app: Option<String>,
    pub importance: FileImportance,
}

impl FileClassification {
    pub fn new(path: impl Into<PathBuf>, category: FileCategory, importance: FileImportance) -> Self {
        let path = path.into();
        let layer = category.layer();
        Self {
            path,
            layer,
            category,
            owner: None,
            role_in_app: None,
            importance,
        }
    }

    /// 设置所属应用。
    pub fn with_owner(mut self, owner: impl Into<String>) -> Self {
        self.owner = Some(owner.into());
        self
    }

    /// 设置文件在应用中的作用。
    pub fn with_role(mut self, role: impl Into<String>) -> Self {
        self.role_in_app = Some(role.into());
        self
    }
}
