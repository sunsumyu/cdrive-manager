use std::{
    cmp::Ordering,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, UNIX_EPOCH},
};

use eframe::egui::{self, RichText};
use rfd::FileDialog;

use crate::{
    ai_analysis::{
        AiAnalysisEvent, AiAnalysisFinished, AiAnalysisHandle, AiAnalysisProgress,
        AiAnalysisReport, AiCleanupRisk, AiProviderConfig, AiReviewFinding, spawn_ai_analysis,
    },
    cleanup::{
        CleanupCandidate, CleanupPreview, CleanupPreviewEvent, CleanupPreviewFinished,
        CleanupPreviewHandle, CleanupPreviewOptions, CleanupPreviewProgress, CleanupRule,
        default_cleanup_rules, spawn_cleanup_preview,
    },
    duplicates::{
        DuplicateFile, DuplicateGroup, DuplicatePreview, DuplicatePreviewEvent,
        DuplicatePreviewFinished, DuplicatePreviewHandle, DuplicatePreviewOptions,
        DuplicatePreviewProgress, spawn_duplicate_preview,
    },
    format,
    model::{
        DirectoryRecord, DirectoryTree, ExtensionRecord, FileRecord, ScanFilterConfig, ScanStats,
        normalize_extension_filter,
    },
    scan_cache::{
        ScanCacheEntry, default_cache_db_path, delete_scan_cache_by_root_key, format_saved_at_time,
        get_cache_db_size, list_scan_cache_entries, load_latest_scan, load_scan_cache_by_root_key,
        save_latest_scan,
    },
    scanner::{ScanEvent, ScanFinished, ScanHandle, ScanOptions, ScanProgress, spawn_scan},
    sunburst::draw_sunburst,
    treemap::{TreemapAction, TreemapItem, draw_treemap},
};

pub struct CDriveManagerApp {
    root_input: String,
    scan_handle: Option<ScanHandle>,
    scan_in_progress: bool,
    cancel_requested: bool,
    progress: Option<ScanProgress>,
    stats: Option<Arc<ScanStats>>,
    cleanup_handle: Option<CleanupPreviewHandle>,
    cleanup_in_progress: bool,
    cleanup_cancel_requested: bool,
    cleanup_progress: Option<CleanupPreviewProgress>,
    cleanup_preview: Option<Arc<CleanupPreview>>,
    duplicate_handle: Option<DuplicatePreviewHandle>,
    duplicate_in_progress: bool,
    duplicate_cancel_requested: bool,
    duplicate_progress: Option<DuplicatePreviewProgress>,
    duplicate_preview: Option<Arc<DuplicatePreview>>,
    ai_analysis_handle: Option<AiAnalysisHandle>,
    ai_analysis_in_progress: bool,
    ai_analysis_cancel_requested: bool,
    ai_analysis_progress: Option<AiAnalysisProgress>,
    ai_analysis_report: Option<Arc<AiAnalysisReport>>,
    ai_provider_config: AiProviderConfig,
    cleanup_rules: Vec<CleanupRuleUiState>,
    status_message: String,
    selected_tab: ResultTab,
    search_query: String,
    directory_sort: SortState<DirectorySortKey>,
    file_sort: SortState<FileSortKey>,
    extension_sort: SortState<ExtensionSortKey>,
    cleanup_sort: SortState<CleanupSortKey>,
    duplicate_sort: SortState<DuplicateSortKey>,
    ai_sort: SortState<AiSortKey>,
    treemap_current_dir: Option<PathBuf>,
    visualization_mode: VisualizationMode,
    // Resizable panel state
    left_panel_ratio: f32,
    right_panel_ratio: f32,
    // Directory tree expansion state
    expanded_dirs: std::collections::HashSet<PathBuf>,
    // Color palette for unified visualization
    color_palette: crate::color_palette::ColorPalette,
    // Extension selection for Treemap highlighting
    selected_extensions: std::collections::HashSet<String>,
    scan_filter_excluded_dirs: String,
    scan_filter_excluded_extensions: String,
    scan_filter_same_file_system: bool,
    duplicate_min_size_bytes: u64,
    cache_manager_open: bool,
    cache_entries: Vec<ScanCacheEntry>,
    cache_db_path: Option<PathBuf>,
    cache_db_size: Option<u64>,
    cache_delete_confirmation: Option<String>,
    // Scan timing and progress tracking
    scan_start_time: Option<std::time::Instant>,
    estimated_total_dirs: Option<u64>,
    estimated_total_files: Option<u64>,
    quick_scan_complete: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    Directories,
    Files,
    Types,
    CleanupPreview,
    DuplicatePreview,
    AiReview,
    Errors,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VisualizationMode {
    Treemap,
    Sunburst,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SortDirection {
    Asc,
    Desc,
}

impl SortDirection {
    fn toggled(self) -> Self {
        match self {
            Self::Asc => Self::Desc,
            Self::Desc => Self::Asc,
        }
    }

    fn arrow(self) -> &'static str {
        match self {
            Self::Asc => "↑",
            Self::Desc => "↓",
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct SortState<K> {
    key: K,
    direction: SortDirection,
}

impl<K> SortState<K>
where
    K: Copy + PartialEq,
{
    fn new(key: K, direction: SortDirection) -> Self {
        Self { key, direction }
    }

    fn select(&mut self, key: K, default_direction: SortDirection) {
        if self.key == key {
            self.direction = self.direction.toggled();
        } else {
            self.key = key;
            self.direction = default_direction;
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DirectorySortKey {
    Name,
    Size,
    Percent,
    Files,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileSortKey {
    Name,
    Size,
    Extension,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExtensionSortKey {
    Extension,
    Size,
    Percent,
    FileCount,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CleanupSortKey {
    Rule,
    Risk,
    Protected,
    Size,
    Path,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DuplicateSortKey {
    Size,
    Count,
    Reclaimable,
    Protected,
    KeepPath,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AiSortKey {
    FinalRecommendation,
    AuditStatus,
    Risk,
    Confidence,
    Category,
    Source,
    Size,
    Protected,
    Path,
}

#[derive(Debug, Clone)]
struct CleanupRuleUiState {
    rule: CleanupRule,
    enabled: bool,
}

impl CleanupRuleUiState {
    fn new(rule: CleanupRule) -> Self {
        Self {
            rule,
            enabled: true,
        }
    }
}

const DEFAULT_DUPLICATE_MIN_SIZE_BYTES: u64 = 1024;

impl CDriveManagerApp {
    pub fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            root_input: default_root(),
            scan_handle: None,
            scan_in_progress: false,
            cancel_requested: false,
            progress: None,
            stats: None,
            cleanup_handle: None,
            cleanup_in_progress: false,
            cleanup_cancel_requested: false,
            cleanup_progress: None,
            cleanup_preview: None,
            duplicate_handle: None,
            duplicate_in_progress: false,
            duplicate_cancel_requested: false,
            duplicate_progress: None,
            duplicate_preview: None,
            ai_analysis_handle: None,
            ai_analysis_in_progress: false,
            ai_analysis_cancel_requested: false,
            ai_analysis_progress: None,
            ai_analysis_report: None,
            ai_provider_config: AiProviderConfig::default(),
            cleanup_rules: default_cleanup_rules()
                .into_iter()
                .map(CleanupRuleUiState::new)
                .collect(),
            status_message: "准备扫描。第一版只分析空间占用，不删除任何文件。".to_owned(),
            selected_tab: ResultTab::Directories,
            search_query: String::new(),
            directory_sort: SortState::new(DirectorySortKey::Size, SortDirection::Desc),
            file_sort: SortState::new(FileSortKey::Size, SortDirection::Desc),
            extension_sort: SortState::new(ExtensionSortKey::Size, SortDirection::Desc),
            cleanup_sort: SortState::new(CleanupSortKey::Size, SortDirection::Desc),
            duplicate_sort: SortState::new(DuplicateSortKey::Reclaimable, SortDirection::Desc),
            ai_sort: SortState::new(AiSortKey::Size, SortDirection::Desc),
            treemap_current_dir: None,
            visualization_mode: VisualizationMode::Treemap,
            left_panel_ratio: 0.25,
            right_panel_ratio: 0.15,
            expanded_dirs: std::collections::HashSet::new(),
            color_palette: crate::color_palette::ColorPalette::new(),
            selected_extensions: std::collections::HashSet::new(),
            scan_filter_excluded_dirs: String::new(),
            scan_filter_excluded_extensions: String::new(),
            scan_filter_same_file_system: false,
            duplicate_min_size_bytes: DEFAULT_DUPLICATE_MIN_SIZE_BYTES,
            cache_manager_open: false,
            cache_entries: Vec::new(),
            cache_db_path: None,
            cache_db_size: None,
            cache_delete_confirmation: None,
            scan_start_time: None,
            estimated_total_dirs: None,
            estimated_total_files: None,
            quick_scan_complete: false,
        }
    }

    fn start_scan(&mut self) {
        let Some(root) = self.validate_root_input() else {
            return;
        };

        let filter_config = self.build_scan_filter_config();
        
        // Start detailed parallel scan immediately so the UI can show partial results
        // instead of waiting for a full QuickCount pass on large drives.
        let options = ScanOptions::new(root.clone(), filter_config)
            .with_mode(crate::scanner::ScanMode::ParallelFullScan);
        
        self.scan_handle = Some(spawn_scan(options));
        self.scan_in_progress = true;
        self.cancel_requested = false;
        self.progress = None;
        self.stats = None;
        self.cleanup_preview = None;
        self.cleanup_progress = None;
        self.duplicate_preview = None;
        self.duplicate_progress = None;
        self.ai_analysis_report = None;
        self.ai_analysis_progress = None;
        self.treemap_current_dir = None;
        self.quick_scan_complete = true;
        self.estimated_total_dirs = None;
        self.estimated_total_files = None;
        self.scan_start_time = Some(std::time::Instant::now());
        self.status_message = format!("多线程扫描中：{}", root.display());
    }
    
    #[cfg(windows)]
    fn start_mft_scan(&mut self) {
        let Some(root) = self.validate_root_input() else {
            return;
        };
        
        // MFT scan only works for drive roots (e.g., C:\)
        let drive_letter = root.to_string_lossy().chars().next().unwrap_or('C');
        
        let filter_config = self.build_scan_filter_config();
        
        let options = ScanOptions::new(root.clone(), filter_config)
            .with_mode(crate::scanner::ScanMode::MftScan)
            .with_drive_letter(drive_letter);
        
        self.scan_handle = Some(spawn_scan(options));
        self.scan_in_progress = true;
        self.cancel_requested = false;
        self.progress = None;
        self.stats = None;
        self.cleanup_preview = None;
        self.cleanup_progress = None;
        self.duplicate_preview = None;
        self.duplicate_progress = None;
        self.ai_analysis_report = None;
        self.ai_analysis_progress = None;
        self.treemap_current_dir = None;
        self.quick_scan_complete = false;
        self.estimated_total_dirs = None;
        self.estimated_total_files = None;
        self.scan_start_time = Some(std::time::Instant::now());
        self.status_message = format!("MFT 高速扫描中：驱动器 {}:...", drive_letter);
    }

    fn build_scan_filter_config(&self) -> ScanFilterConfig {
        let excluded_directories: Vec<String> = self
            .scan_filter_excluded_dirs
            .split([',', ';', '\n'])
            .map(|part| part.trim().to_owned())
            .filter(|part| !part.is_empty())
            .collect();
        let excluded_extensions: Vec<String> = self
            .scan_filter_excluded_extensions
            .split([',', ';', '\n'])
            .filter_map(|part| normalize_extension_filter(part.trim()))
            .collect();
        ScanFilterConfig {
            excluded_directories,
            excluded_extensions,
            same_file_system: self.scan_filter_same_file_system,
        }
    }

    fn cancel_scan(&mut self) {
        if let Some(handle) = &self.scan_handle {
            handle.cancel();
            self.cancel_requested = true;
            self.status_message = "正在取消扫描，已扫描结果会保留……".to_owned();
        }
    }

    fn start_cleanup_preview(&mut self) {
        let Some(stats) = self.stats.as_ref() else {
            self.status_message = "请先完成扫描或打开已保存的扫描结果，再生成清理预览。".to_owned();
            return;
        };

        let rules = self.enabled_cleanup_rules();
        if rules.is_empty() {
            self.status_message = "请至少启用一条清理预览规则。".to_owned();
            return;
        }

        let root = stats.root.clone();
        let rule_count = rules.len();
        self.cleanup_handle = Some(spawn_cleanup_preview(CleanupPreviewOptions {
            root: root.clone(),
            rules,
        }));
        self.cleanup_in_progress = true;
        self.cleanup_cancel_requested = false;
        self.cleanup_progress = None;
        self.cleanup_preview = None;
        self.selected_tab = ResultTab::CleanupPreview;
        self.status_message = format!(
            "正在生成 dry-run 清理预览：{}，启用 {} 条规则。不会删除、移动或修改任何文件。",
            root.display(),
            format::count(rule_count as u64)
        );
    }

    fn cancel_cleanup_preview(&mut self) {
        if let Some(handle) = &self.cleanup_handle {
            handle.cancel();
            self.cleanup_cancel_requested = true;
            self.status_message = "正在取消清理预览，已发现的候选会保留……".to_owned();
        }
    }

    fn start_duplicate_preview(&mut self) {
        let Some(stats) = self.stats.as_ref() else {
            self.status_message = "请先完成扫描或打开已保存的扫描结果，再查找重复文件。".to_owned();
            return;
        };

        let root = stats.root.clone();
        let min_size = self.duplicate_min_size_bytes;
        self.duplicate_handle = Some(spawn_duplicate_preview(DuplicatePreviewOptions {
            root: root.clone(),
            min_size,
        }));
        self.duplicate_in_progress = true;
        self.duplicate_cancel_requested = false;
        self.duplicate_progress = None;
        self.duplicate_preview = None;
        self.selected_tab = ResultTab::DuplicatePreview;
        self.status_message = format!(
            "正在 dry-run 查找重复文件：{}，仅哈希不小于 {} 的同大小候选；不会删除、移动或修改任何文件。",
            root.display(),
            format::bytes(min_size)
        );
    }

    fn cancel_duplicate_preview(&mut self) {
        if let Some(handle) = &self.duplicate_handle {
            handle.cancel();
            self.duplicate_cancel_requested = true;
            self.status_message = "正在取消重复文件检测，已发现的重复组会保留……".to_owned();
        }
    }

    fn start_ai_analysis(&mut self) {
        let cleanup_preview = self.current_cleanup_preview();
        let duplicate_preview = self.current_duplicate_preview();
        if cleanup_preview.is_none() && duplicate_preview.is_none() {
            self.status_message = "请先生成清理预览或重复文件预览，再启动 AI 分析审核。".to_owned();
            return;
        }

        let root = cleanup_preview
            .as_ref()
            .map(|preview| preview.root.clone())
            .or_else(|| duplicate_preview.as_ref().map(|preview| preview.root.clone()))
            .or_else(|| self.stats.as_ref().map(|stats| stats.root.clone()))
            .unwrap_or_else(|| PathBuf::from(self.root_input.trim()));

        let config = self.ai_provider_config.clone();
        self.ai_analysis_handle = Some(spawn_ai_analysis(crate::ai_analysis::AiAnalysisOptions {
            root: root.clone(),
            cleanup_preview,
            duplicate_preview,
            provider_config: config.clone(),
        }));
        self.ai_analysis_in_progress = true;
        self.ai_analysis_cancel_requested = false;
        self.ai_analysis_progress = None;
        self.ai_analysis_report = None;
        self.selected_tab = ResultTab::AiReview;
        self.status_message = format!(
            "正在进行 AI 分析审核：{}，模型 {}。dry-run/report-only，不会删除、移动或修改任何文件。",
            root.display(),
            config.model
        );
    }

    fn cancel_ai_analysis(&mut self) {
        if let Some(handle) = &self.ai_analysis_handle {
            handle.cancel();
            self.ai_analysis_cancel_requested = true;
            self.status_message = "正在取消 AI 分析审核，已生成的报告会保留……".to_owned();
        }
    }

    fn validate_root_input(&mut self) -> Option<PathBuf> {
        let input = self.root_input.trim();
        if input.is_empty() {
            self.status_message = "请输入要扫描的目录。".to_owned();
            return None;
        }

        let root = PathBuf::from(input);
        if !root.exists() {
            self.status_message = format!("路径不存在：{}", root.display());
            return None;
        }

        if !root.is_dir() {
            self.status_message = format!("路径不是目录：{}", root.display());
            return None;
        }

        Some(root)
    }

    fn choose_directory(&mut self) {
        let mut dialog = FileDialog::new().set_title("选择要扫描的目录");
        let current = PathBuf::from(self.root_input.trim());
        if current.is_dir() {
            dialog = dialog.set_directory(current);
        }

        if let Some(path) = dialog.pick_folder() {
            self.root_input = path.display().to_string();
            self.status_message = format!("已选择目录：{}", path.display());
        }
    }

    fn save_scan_result(&mut self) {
        let Some(stats) = self.stats.as_ref().map(Arc::clone) else {
            self.status_message = "没有可保存的扫描结果。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("保存扫描结果")
            .add_filter("扫描结果 JSON", &["json"])
            .set_file_name("cdrive-scan-result.json")
            .save_file()
        else {
            return;
        };

        match save_scan_result_to_path(&path, stats.as_ref()) {
            Ok(()) => {
                self.status_message = format!("已保存扫描结果：{}", path.display());
            }
            Err(error) => {
                self.status_message = format!("保存扫描结果失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn open_scan_result(&mut self) {
        let Some(path) = FileDialog::new()
            .set_title("打开扫描结果")
            .add_filter("扫描结果 JSON", &["json"])
            .pick_file()
        else {
            return;
        };

        match load_scan_result_from_path(&path) {
            Ok(stats) => {
                let metadata = scan_cache_metadata_summary(&stats);
                self.set_loaded_stats(stats);
                self.status_message = format!("已打开扫描结果：{} ({})", path.display(), metadata);
            }
            Err(error) => {
                self.status_message = format!("打开扫描结果失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn open_cached_scan_result(&mut self) {
        let Some(root) = self.validate_root_input() else {
            return;
        };

        match load_latest_scan(&root) {
            Ok(Some(stats)) => {
                let metadata = scan_cache_metadata_summary(&stats);
                self.set_loaded_stats(stats);
                self.status_message = format!(
                    "已打开 SQLite 最新扫描缓存：{} ({})",
                    root.display(),
                    metadata
                );
            }
            Ok(None) => {
                self.status_message =
                    format!("没有找到该目录的 SQLite 扫描缓存：{}", root.display());
            }
            Err(error) => {
                self.status_message =
                    format!("打开 SQLite 扫描缓存失败：{} ({:#})", root.display(), error);
            }
        }
    }

    fn set_loaded_stats(&mut self, stats: ScanStats) {
        self.root_input = stats.root.display().to_string();
        self.stats = Some(Arc::new(stats));
        self.progress = None;
        self.scan_in_progress = false;
        self.cancel_requested = false;
        self.scan_handle = None;
        self.cleanup_preview = None;
        self.cleanup_progress = None;
        self.cleanup_in_progress = false;
        self.cleanup_cancel_requested = false;
        self.cleanup_handle = None;
        self.duplicate_preview = None;
        self.duplicate_progress = None;
        self.duplicate_in_progress = false;
        self.duplicate_cancel_requested = false;
        self.duplicate_handle = None;
        self.ai_analysis_report = None;
        self.ai_analysis_progress = None;
        self.ai_analysis_in_progress = false;
        self.ai_analysis_cancel_requested = false;
        self.ai_analysis_handle = None;
        self.treemap_current_dir = None;
    }

    fn export_csv_report(&mut self) {
        let Some(stats) = self.stats.as_ref().map(Arc::clone) else {
            self.status_message = "没有可导出的扫描结果。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("导出 CSV 报告")
            .add_filter("CSV 报告", &["csv"])
            .set_file_name("cdrive-scan-report.csv")
            .save_file()
        else {
            return;
        };

        match export_csv_report_to_path(&path, stats.as_ref()) {
            Ok(summary) => {
                self.status_message = format!("已导出 CSV 报告：{} ({})", path.display(), summary);
            }
            Err(error) => {
                self.status_message = format!("导出 CSV 失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn export_cleanup_preview_csv(&mut self) {
        let Some(preview) = self.current_cleanup_preview() else {
            self.status_message = "没有可导出的清理预览。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("导出清理预览 CSV")
            .add_filter("CSV 报告", &["csv"])
            .set_file_name("cdrive-cleanup-preview.csv")
            .save_file()
        else {
            return;
        };

        match export_cleanup_preview_to_path(&path, preview.as_ref()) {
            Ok(summary) => {
                self.status_message =
                    format!("已导出清理预览 CSV：{} ({})", path.display(), summary);
            }
            Err(error) => {
                self.status_message = format!("导出清理预览失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn export_duplicate_preview_csv(&mut self) {
        let Some(preview) = self.current_duplicate_preview() else {
            self.status_message = "没有可导出的重复文件预览。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("导出重复文件预览 CSV")
            .add_filter("CSV 报告", &["csv"])
            .set_file_name("cdrive-duplicate-preview.csv")
            .save_file()
        else {
            return;
        };

        match export_duplicate_preview_to_path(&path, preview.as_ref()) {
            Ok(summary) => {
                self.status_message =
                    format!("已导出重复文件预览 CSV：{} ({})", path.display(), summary);
            }
            Err(error) => {
                self.status_message =
                    format!("导出重复文件预览失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn export_ai_analysis_report_csv(&mut self) {
        let Some(report) = self.current_ai_analysis_report() else {
            self.status_message = "没有可导出的 AI 审核报告。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("导出 AI 审核报告 CSV")
            .add_filter("CSV 报告", &["csv"])
            .set_file_name("cdrive-ai-review-report.csv")
            .save_file()
        else {
            return;
        };

        match export_ai_analysis_report_to_path(&path, report.as_ref()) {
            Ok(summary) => {
                self.status_message = format!("已导出 AI 审核报告：{} ({})", path.display(), summary);
            }
            Err(error) => {
                self.status_message = format!("导出 AI 审核报告失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn export_ai_delete_list_csv(&mut self) {
        let Some(report) = self.current_ai_analysis_report() else {
            self.status_message = "没有可导出的 AI 待删清单。".to_owned();
            return;
        };

        let Some(path) = FileDialog::new()
            .set_title("导出 AI 待删清单 CSV")
            .add_filter("CSV 报告", &["csv"])
            .set_file_name("cdrive-ai-delete-candidates.csv")
            .save_file()
        else {
            return;
        };

        match export_ai_delete_list_to_path(&path, report.as_ref()) {
            Ok(summary) => {
                self.status_message = format!("已导出 AI 待删清单：{} ({})", path.display(), summary);
            }
            Err(error) => {
                self.status_message = format!("导出 AI 待删清单失败：{} ({:#})", path.display(), error);
            }
        }
    }

    fn poll_scan_events(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self
            .scan_handle
            .as_ref()
            .map(|handle| handle.receiver.clone())
        else {
            return;
        };

        let mut latest_progress = None;
        let mut finished = None;

        while let Ok(event) = receiver.try_recv() {
            match event {
                ScanEvent::Progress(progress) => latest_progress = Some(progress),
                ScanEvent::Finished(result) => finished = Some(result),
            }
        }

        if let Some(progress) = latest_progress {
            self.apply_progress(progress);
        }

        if let Some(result) = finished {
            self.apply_finished(result);
        }

        if self.scan_in_progress {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn poll_cleanup_preview_events(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self
            .cleanup_handle
            .as_ref()
            .map(|handle| handle.receiver.clone())
        else {
            return;
        };

        let mut latest_progress = None;
        let mut finished = None;

        while let Ok(event) = receiver.try_recv() {
            match event {
                CleanupPreviewEvent::Progress(progress) => latest_progress = Some(progress),
                CleanupPreviewEvent::Finished(result) => finished = Some(result),
            }
        }

        if let Some(progress) = latest_progress {
            self.apply_cleanup_preview_progress(progress);
        }

        if let Some(result) = finished {
            self.apply_cleanup_preview_finished(result);
        }

        if self.cleanup_in_progress {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn poll_duplicate_preview_events(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self
            .duplicate_handle
            .as_ref()
            .map(|handle| handle.receiver.clone())
        else {
            return;
        };

        let mut latest_progress = None;
        let mut finished = None;

        while let Ok(event) = receiver.try_recv() {
            match event {
                DuplicatePreviewEvent::Progress(progress) => latest_progress = Some(progress),
                DuplicatePreviewEvent::Finished(result) => finished = Some(result),
            }
        }

        if let Some(progress) = latest_progress {
            self.apply_duplicate_preview_progress(progress);
        }

        if let Some(result) = finished {
            self.apply_duplicate_preview_finished(result);
        }

        if self.duplicate_in_progress {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn poll_ai_analysis_events(&mut self, ctx: &egui::Context) {
        let Some(receiver) = self
            .ai_analysis_handle
            .as_ref()
            .map(|handle| handle.receiver.clone())
        else {
            return;
        };

        let mut latest_progress = None;
        let mut finished = None;

        while let Ok(event) = receiver.try_recv() {
            match event {
                AiAnalysisEvent::Progress(progress) => latest_progress = Some(progress),
                AiAnalysisEvent::Finished(result) => finished = Some(result),
            }
        }

        if let Some(progress) = latest_progress {
            self.apply_ai_analysis_progress(progress);
        }

        if let Some(result) = finished {
            self.apply_ai_analysis_finished(result);
        }

        if self.ai_analysis_in_progress {
            ctx.request_repaint_after(Duration::from_millis(120));
        }
    }

    fn apply_progress(&mut self, progress: ScanProgress) {
        use crate::scanner::ScanMode;

        self.status_message = if progress.cancelled {
            "扫描已取消，当前显示的是部分结果。".to_owned()
        } else if progress.finished {
            "扫描完成。".to_owned()
        } else if let Some(path) = &progress.current_path {
            match &progress.scan_mode {
                ScanMode::QuickCount => format!("快速统计：{}", path.display()),
                ScanMode::FullScan | ScanMode::ParallelFullScan | ScanMode::MftScan => {
                    let mode_label = match &progress.scan_mode {
                        ScanMode::ParallelFullScan => {
                            if let Some(threads) = progress.active_threads {
                                format!("多线程 ({}线程)", threads)
                            } else {
                                "多线程".to_string()
                            }
                        }
                        ScanMode::MftScan => "MFT 高速".to_string(),
                        _ => "单线程".to_string(),
                    };
                    
                    // Show progress with ETA if we have estimates
                    if let (Some(estimated), Some(start_time)) = (self.estimated_total_dirs, self.scan_start_time) {
                        let current = progress.stats.dir_count;
                        let ratio = current as f64 / estimated as f64;
                        let pct = (ratio * 100.0).min(99.9);
                        let elapsed = start_time.elapsed();
                        
                        let eta_str = if let Some(remaining) = self.estimated_remaining(ratio) {
                            format!(", 剩余 {}", format::duration(remaining))
                        } else {
                            String::new()
                        };
                        
                        format!(
                            "{}扫描中：{:.1}% ({}秒{}) {}",
                            mode_label,
                            pct,
                            elapsed.as_secs(),
                            eta_str,
                            path.display()
                        )
                    } else {
                        format!("{}扫描：{}", mode_label, path.display())
                    }
                }
            }
        } else {
            match &progress.scan_mode {
                ScanMode::QuickCount => "快速统计中...".to_owned(),
                ScanMode::FullScan => "单线程详细扫描中...".to_owned(),
                ScanMode::ParallelFullScan => {
                    if let Some(threads) = progress.active_threads {
                        format!("多线程详细扫描中 ({} 线程)...", threads)
                    } else {
                        "多线程详细扫描中...".to_owned()
                    }
                }
                ScanMode::MftScan => "MFT 高速扫描中...".to_owned(),
            }
        };
        
        // Store estimated totals from progress
        if let Some(total_dirs) = progress.estimated_total_dirs {
            self.estimated_total_dirs = Some(total_dirs);
        }
        if let Some(total_files) = progress.estimated_total_files {
            self.estimated_total_files = Some(total_files);
        }
        
        self.progress = Some(progress);
    }

    fn estimated_remaining(&self, progress_ratio: f64) -> Option<Duration> {
        let start_time = self.scan_start_time?;
        let elapsed = start_time.elapsed();
        if progress_ratio <= 0.0 || progress_ratio >= 1.0 {
            return None;
        }
        let total_estimated_secs = elapsed.as_secs_f64() / progress_ratio;
        let total_estimated = Duration::from_secs_f64(total_estimated_secs);
        total_estimated.checked_sub(elapsed)
    }

    fn apply_finished(&mut self, result: ScanFinished) {
        use crate::scanner::ScanMode;

        match &result.scan_mode {
            ScanMode::QuickCount => {
                // First pass complete - store estimates and start full scan
                self.estimated_total_dirs = Some(result.total_dirs);
                self.estimated_total_files = Some(result.total_files);
                self.quick_scan_complete = true;
                
                if result.cancelled {
                    self.scan_in_progress = false;
                    self.scan_handle = None;
                    self.status_message = format!("快速统计已取消");
                    return;
                }

                // Start second pass (parallel full scan)
                let root = PathBuf::from(&self.root_input);
                let filter_config = self.build_scan_filter_config();
                let options = ScanOptions::new(root.clone(), filter_config)
                    .with_mode(ScanMode::ParallelFullScan);
                
                self.scan_handle = Some(spawn_scan(options));
                self.status_message = format!(
                    "快速统计完成：{} 个目录，{} 个文件。开始多线程详细扫描 ({} 线程)...",
                    format::count(result.total_dirs),
                    format::count(result.total_files),
                    rayon::current_num_threads()
                );
            }
            ScanMode::FullScan | ScanMode::ParallelFullScan | ScanMode::MftScan => {
                // Second pass complete - show final results
                self.stats = Some(Arc::clone(&result.stats));
                self.scan_in_progress = false;
                self.cancel_requested = false;
                self.scan_handle = None;

                let mode_name = match &result.scan_mode {
                    ScanMode::ParallelFullScan => "多线程",
                    ScanMode::MftScan => "MFT 高速",
                    _ => "单线程",
                };

                let mut message = if result.cancelled {
                    format!(
                        "{}扫描已取消：已统计 {} 个文件，{} 个目录，部分结果共 {}。",
                        mode_name,
                        format::count(result.stats.file_count),
                        format::count(result.stats.dir_count),
                        format::bytes(result.stats.total_size)
                    )
                } else {
                    format!(
                        "{}扫描完成：{} 个文件，{} 个目录，总计 {}。",
                        mode_name,
                        format::count(result.stats.file_count),
                        format::count(result.stats.dir_count),
                        format::bytes(result.stats.total_size)
                    )
                };

                message.push_str(&format!(" 耗时：{}", format::duration(result.elapsed_time)));

                if !result.cancelled {
                    match save_latest_scan(result.stats.as_ref()) {
                        Ok(path) => {
                            message.push_str(&format!(" 已写入 SQLite 最新缓存：{}。", path.display()));
                        }
                        Err(error) => {
                            message.push_str(&format!(" SQLite 最新缓存写入失败：{:#}。", error));
                        }
                    }
                }

                self.status_message = message;
            }
        }
    }

    fn apply_cleanup_preview_progress(&mut self, progress: CleanupPreviewProgress) {
        self.status_message = if progress.cancelled {
            "清理预览已取消，当前显示的是部分 dry-run 结果。".to_owned()
        } else if progress.finished {
            "清理预览完成。".to_owned()
        } else if let Some(path) = &progress.current_path {
            format!("正在预览：{}", path.display())
        } else {
            "正在生成清理预览……".to_owned()
        };
        self.cleanup_progress = Some(progress);
    }

    fn apply_cleanup_preview_finished(&mut self, result: CleanupPreviewFinished) {
        self.status_message = if result.cancelled {
            format!(
                "清理预览已取消：{} 下发现 {} 个候选，dry-run 预计可清理 {}，受保护 {}，错误 {} 条。",
                result.preview.root.display(),
                format::count(result.preview.candidate_count),
                format::bytes(result.preview.reclaimable_size),
                format::bytes(result.preview.protected_size),
                format::count(result.preview.error_count)
            )
        } else {
            format!(
                "清理预览完成：{} 下发现 {} 个候选，dry-run 预计可清理 {}，受保护 {}，错误 {} 条。当前版本不会执行清理。",
                result.preview.root.display(),
                format::count(result.preview.candidate_count),
                format::bytes(result.preview.reclaimable_size),
                format::bytes(result.preview.protected_size),
                format::count(result.preview.error_count)
            )
        };
        self.cleanup_preview = Some(result.preview);
        self.cleanup_in_progress = false;
        self.cleanup_cancel_requested = false;
        self.cleanup_handle = None;
    }

    fn apply_duplicate_preview_progress(&mut self, progress: DuplicatePreviewProgress) {
        self.status_message = if progress.cancelled {
            "重复文件检测已取消，当前显示的是部分 dry-run 结果。".to_owned()
        } else if progress.finished {
            "重复文件检测完成。".to_owned()
        } else if let Some(path) = &progress.current_path {
            format!("正在{}：{}", progress.phase.label(), path.display())
        } else {
            format!("正在{}……", progress.phase.label())
        };
        self.duplicate_progress = Some(progress);
    }

    fn apply_duplicate_preview_finished(&mut self, result: DuplicatePreviewFinished) {
        self.status_message = if result.cancelled {
            format!(
                "重复文件检测已取消：{} 下发现 {} 个重复组，dry-run 预计可回收 {}，受保护 {}，错误 {} 条。",
                result.preview.root.display(),
                format::count(result.preview.duplicate_group_count),
                format::bytes(result.preview.reclaimable_size),
                format::bytes(result.preview.protected_size),
                format::count(result.preview.error_count)
            )
        } else {
            format!(
                "重复文件检测完成：{} 下发现 {} 个重复组、{} 个重复副本，dry-run 预计可回收 {}，受保护 {}，错误 {} 条。当前版本不会执行删除。",
                result.preview.root.display(),
                format::count(result.preview.duplicate_group_count),
                format::count(result.preview.duplicate_file_count),
                format::bytes(result.preview.reclaimable_size),
                format::bytes(result.preview.protected_size),
                format::count(result.preview.error_count)
            )
        };
        self.duplicate_preview = Some(result.preview);
        self.duplicate_in_progress = false;
        self.duplicate_cancel_requested = false;
        self.duplicate_handle = None;
    }

    fn apply_ai_analysis_progress(&mut self, progress: AiAnalysisProgress) {
        self.status_message = if progress.cancelled {
            "AI 分析审核已取消，当前显示的是部分报告。".to_owned()
        } else if progress.finished {
            "AI 分析审核完成。".to_owned()
        } else if let Some(item) = &progress.current_item {
            format!("正在{}：{}", progress.phase.label(), item)
        } else {
            format!("正在{}……", progress.phase.label())
        };
        self.ai_analysis_progress = Some(progress);
    }

    fn apply_ai_analysis_finished(&mut self, result: AiAnalysisFinished) {
        self.status_message = if result.cancelled {
            format!(
                "AI 分析审核已取消：已分析 {} 个候选，可导出待删候选 {} 个，需人工复核 {} 个。不会删除任何文件。",
                format::count(result.report.candidate_count),
                format::count(result.report.delete_candidate_count),
                format::count(result.report.needs_review_count)
            )
        } else {
            format!(
                "AI 分析审核完成：{} 个候选，可导出待删候选 {} 个，需人工复核 {} 个，拒绝/保留 {} 个，错误 {} 条。请人工确认后自行处理；本程序不会删除文件。",
                format::count(result.report.candidate_count),
                format::count(result.report.delete_candidate_count),
                format::count(result.report.needs_review_count),
                format::count(result.report.rejected_count),
                format::count(result.report.error_count)
            )
        };
        self.ai_analysis_report = Some(result.report);
        self.ai_analysis_in_progress = false;
        self.ai_analysis_cancel_requested = false;
        self.ai_analysis_handle = None;
    }

    fn current_stats(&self) -> Option<Arc<ScanStats>> {
        self.stats.as_ref().map(Arc::clone).or_else(|| {
            self.progress
                .as_ref()
                .map(|progress| Arc::clone(&progress.stats))
        })
    }

    fn current_cleanup_preview(&self) -> Option<Arc<CleanupPreview>> {
        self.cleanup_preview.as_ref().map(Arc::clone).or_else(|| {
            self.cleanup_progress
                .as_ref()
                .map(|progress| Arc::clone(&progress.preview))
        })
    }

    fn current_duplicate_preview(&self) -> Option<Arc<DuplicatePreview>> {
        self.duplicate_preview.as_ref().map(Arc::clone).or_else(|| {
            self.duplicate_progress
                .as_ref()
                .map(|progress| Arc::clone(&progress.preview))
        })
    }

    fn current_ai_analysis_report(&self) -> Option<Arc<AiAnalysisReport>> {
        self.ai_analysis_report.as_ref().map(Arc::clone).or_else(|| {
            self.ai_analysis_progress
                .as_ref()
                .map(|progress| Arc::clone(&progress.report))
        })
    }

    fn enabled_cleanup_rules(&self) -> Vec<CleanupRule> {
        self.cleanup_rules
            .iter()
            .filter(|state| state.enabled)
            .map(|state| state.rule.clone())
            .collect()
    }

    fn enabled_cleanup_rule_count(&self) -> usize {
        self.cleanup_rules
            .iter()
            .filter(|state| state.enabled)
            .count()
    }

    fn set_all_cleanup_rules_enabled(&mut self, enabled: bool) {
        for state in &mut self.cleanup_rules {
            state.enabled = enabled;
        }
        self.invalidate_cleanup_preview_after_rule_change();
    }

    fn reset_cleanup_rules(&mut self) {
        self.cleanup_rules = default_cleanup_rules()
            .into_iter()
            .map(CleanupRuleUiState::new)
            .collect();
        self.invalidate_cleanup_preview_after_rule_change();
    }

    fn invalidate_cleanup_preview_after_rule_change(&mut self) {
        if !self.cleanup_in_progress
            && (self.cleanup_preview.is_some() || self.cleanup_progress.is_some())
        {
            self.cleanup_preview = None;
            self.cleanup_progress = None;
            self.status_message = "清理规则已变化，请重新生成 dry-run 预览。".to_owned();
        }
    }

    fn invalidate_duplicate_preview_after_config_change(&mut self) {
        if !self.duplicate_in_progress
            && (self.duplicate_preview.is_some() || self.duplicate_progress.is_some())
        {
            self.duplicate_preview = None;
            self.duplicate_progress = None;
            self.status_message = "重复检测配置已变化，请重新查找重复文件。".to_owned();
        }
    }

    fn draw_scan_filter_config(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("扫描过滤配置", |ui| {
            ui.label(
                RichText::new("配置扫描时跳过的目录和文件类型，可减少扫描时间。")
                    .small()
                    .weak(),
            );

            let busy = self.scan_in_progress
                || self.cleanup_in_progress
                || self.duplicate_in_progress
                || self.ai_analysis_in_progress;

            ui.label(RichText::new("排除目录名称").strong());
            ui.add_enabled(
                !busy,
                egui::TextEdit::multiline(&mut self.scan_filter_excluded_dirs)
                    .hint_text("node_modules, target, .git")
                    .desired_width(ui.available_width()),
            );
            ui.label(
                RichText::new("目录名大小写不敏感，逗号/分号/换行分隔")
                    .small()
                    .weak(),
            );

            ui.add_space(6.0);
            ui.label(RichText::new("排除文件扩展名").strong());
            ui.add_enabled(
                !busy,
                egui::TextEdit::multiline(&mut self.scan_filter_excluded_extensions)
                    .hint_text(".tmp, .log, [无扩展名]")
                    .desired_width(ui.available_width()),
            );
            ui.label(
                RichText::new("扩展名大小写不敏感，可带或不带点，逗号/分号/换行分隔")
                    .small()
                    .weak(),
            );

            ui.add_space(6.0);
            ui.checkbox(&mut self.scan_filter_same_file_system, "限制在同一文件系统");
            ui.label(
                RichText::new("启用后不跨越挂载点扫描（如 C 盘不扫描 D 盘挂载目录）")
                    .small()
                    .weak(),
            );
        });
    }

    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                let busy =
                    self.scan_in_progress || self.cleanup_in_progress || self.duplicate_in_progress;
                ui.heading("C 盘空间管理器");
                ui.separator();
                ui.label("扫描目录：");
                let input = ui.text_edit_singleline(&mut self.root_input);
                if input.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    && !busy
                {
                    self.start_scan();
                }

                if ui
                    .add_enabled(!busy, egui::Button::new("选择目录"))
                    .clicked()
                {
                    self.choose_directory();
                }

                if ui
                    .add_enabled(!busy, egui::Button::new("开始扫描"))
                    .clicked()
                {
                    self.start_scan();
                }
                
                // MFT 高速扫描按钮 (仅 Windows)
                #[cfg(windows)]
                {
                    let mft_tooltip = "直接读取 NTFS MFT，速度极快\n需要管理员权限";
                    if ui
                        .add_enabled(!busy, egui::Button::new("MFT 高速扫描"))
                        .on_hover_text(mft_tooltip)
                        .clicked()
                    {
                        self.start_mft_scan();
                    }
                }

                if ui
                    .add_enabled(
                        self.scan_in_progress && !self.cancel_requested,
                        egui::Button::new("取消扫描"),
                    )
                    .clicked()
                {
                    self.cancel_scan();
                }

                ui.separator();

                if ui
                    .add_enabled(!busy, egui::Button::new("打开结果"))
                    .clicked()
                {
                    self.open_scan_result();
                }

                if ui
                    .add_enabled(!busy, egui::Button::new("打开缓存"))
                    .clicked()
                {
                    self.open_cached_scan_result();
                }

                let has_final_stats = self.stats.is_some();
                if ui
                    .add_enabled(!busy && has_final_stats, egui::Button::new("保存结果"))
                    .clicked()
                {
                    self.save_scan_result();
                }

                if ui
                    .add_enabled(!busy && has_final_stats, egui::Button::new("导出 CSV"))
                    .clicked()
                {
                    self.export_csv_report();
                }

                ui.separator();

                if ui
                    .add_enabled(
                        !busy && has_final_stats && self.enabled_cleanup_rule_count() > 0,
                        egui::Button::new("生成清理预览"),
                    )
                    .clicked()
                {
                    self.start_cleanup_preview();
                }

                if ui
                    .add_enabled(
                        self.cleanup_in_progress && !self.cleanup_cancel_requested,
                        egui::Button::new("取消预览"),
                    )
                    .clicked()
                {
                    self.cancel_cleanup_preview();
                }

                if ui
                    .add_enabled(
                        !busy && self.current_cleanup_preview().is_some(),
                        egui::Button::new("导出预览 CSV"),
                    )
                    .clicked()
                {
                    self.export_cleanup_preview_csv();
                }

                ui.separator();

                if ui
                    .add_enabled(!busy && has_final_stats, egui::Button::new("查找重复文件"))
                    .clicked()
                {
                    self.start_duplicate_preview();
                }

                if ui
                    .add_enabled(
                        self.duplicate_in_progress && !self.duplicate_cancel_requested,
                        egui::Button::new("取消重复检测"),
                    )
                    .clicked()
                {
                    self.cancel_duplicate_preview();
                }

                if ui
                    .add_enabled(
                        !busy && self.current_duplicate_preview().is_some(),
                        egui::Button::new("导出重复 CSV"),
                    )
                    .clicked()
                {
                    self.export_duplicate_preview_csv();
                }

                ui.separator();

                let has_ai_source =
                    self.current_cleanup_preview().is_some() || self.current_duplicate_preview().is_some();
                if ui
                    .add_enabled(!busy && has_ai_source, egui::Button::new("AI 分析审核"))
                    .on_hover_text("基于清理预览和/或重复文件预览调用 OpenAI 兼容 API。只生成报告，不删除文件。")
                    .clicked()
                {
                    self.start_ai_analysis();
                }

                if ui
                    .add_enabled(
                        self.ai_analysis_in_progress && !self.ai_analysis_cancel_requested,
                        egui::Button::new("取消 AI 分析"),
                    )
                    .clicked()
                {
                    self.cancel_ai_analysis();
                }

                if ui
                    .add_enabled(
                        !busy && self.current_ai_analysis_report().is_some(),
                        egui::Button::new("导出 AI 报告 CSV"),
                    )
                    .clicked()
                {
                    self.export_ai_analysis_report_csv();
                }

                if ui
                    .add_enabled(
                        !busy && self.current_ai_analysis_report().is_some(),
                        egui::Button::new("导出待删清单 CSV"),
                    )
                    .clicked()
                {
                    self.export_ai_delete_list_csv();
                }

                ui.separator();

                if ui
                    .add_enabled(!busy, egui::Button::new("缓存管理"))
                    .clicked()
                {
                    self.open_cache_manager();
                }

                if busy {
                    ui.spinner();
                    let text = if self.scan_in_progress {
                        if self.cancel_requested {
                            "取消扫描中"
                        } else {
                            "扫描中"
                        }
                    } else if self.cleanup_in_progress {
                        if self.cleanup_cancel_requested {
                            "取消预览中"
                        } else {
                            "预览中"
                        }
                    } else if self.duplicate_in_progress {
                        if self.duplicate_cancel_requested {
                            "取消重复检测中"
                        } else {
                            "重复检测中"
                        }
                    } else if self.ai_analysis_cancel_requested {
                        "取消 AI 分析中"
                    } else {
                        "AI 分析中"
                    };
                    ui.label(RichText::new(text).strong());
                    
                    // Add progress bar for scanning
                    if self.scan_in_progress {
                        self.draw_progress_bar(ui);
                    }
                }
            });
            ui.label(RichText::new(&self.status_message).small());
        });
    }

    fn draw_progress_bar(&self, ui: &mut egui::Ui) {
        use crate::scanner::ScanMode;

        let Some(progress) = &self.progress else {
            return;
        };

        let Some(start_time) = self.scan_start_time else {
            return;
        };

        // Only show progress bar for FullScan mode
        if let ScanMode::FullScan = &progress.scan_mode {
            if let Some(estimated) = self.estimated_total_dirs {
                let current = progress.stats.dir_count;
                let ratio = (current as f64 / estimated as f64).clamp(0.0, 1.0);
                let pct = ratio * 100.0;
                
                ui.add_space(4.0);
                
                // Progress bar
                let progress_bar = egui::ProgressBar::new(ratio as f32)
                    .text(format!("进度: {:.1}%", pct))
                    .desired_width(200.0)
                    .desired_height(12.0);
                ui.add(progress_bar);
                
                // Time info
                let elapsed = start_time.elapsed();
                let time_text = if let Some(remaining) = self.estimated_remaining(ratio) {
                    format!(
                        "已用: {} | 剩余: {} | {}/{} 目录",
                        format::duration(elapsed),
                        format::duration(remaining),
                        format::count(current),
                        format::count(estimated)
                    )
                } else {
                    format!(
                        "已用: {} | {}/{} 目录",
                        format::duration(elapsed),
                        format::count(current),
                        format::count(estimated)
                    )
                };
                ui.label(RichText::new(time_text).small());
            }
        } else if let ScanMode::QuickCount = &progress.scan_mode {
            // Quick count phase
            ui.add_space(4.0);
            ui.label(RichText::new("快速统计目录和文件数量...").small().weak());
        }
    }

    /// WizTree-style three-column layout with resizable panels:
    /// - Left: Directory tree (resizable)
    /// - Center: Treemap/Sunburst visualization
    /// - Right: File type distribution (resizable)
    fn draw_wiztree_layout(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        let available_height = ui.available_height();
        let available_width = ui.available_width();
        
        // Use 55% height for visualization area, rest for tabs
        let viz_height = (available_height * 0.55).max(280.0).min(520.0);
        let splitter_width = 4.0;

        // Calculate panel widths from ratios with constraints
        let left_width = (available_width * self.left_panel_ratio).clamp(180.0, 320.0);
        let right_width = (available_width * self.right_panel_ratio).clamp(150.0, 280.0);
        let center_width = (available_width - left_width - right_width - splitter_width * 2.0).max(260.0);

        // Use a single horizontal layout with proper cursor advancement
        ui.allocate_ui_with_layout(
            egui::vec2(available_width, viz_height),
            egui::Layout::left_to_right(egui::Align::Min),
            |ui| {
                // === Left panel: Directory tree ===
                let left_rect_start = ui.cursor().min;
                
                ui.allocate_ui_with_layout(
                    egui::vec2(left_width, viz_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_min_height(viz_height - 8.0);
                            self.draw_directory_tree_panel(ui, stats);
                        });
                    },
                );
                
                // Get the actual allocated rect for left panel
                let left_allocated_rect = ui.min_rect();
                
                // === Left splitter ===
                let left_splitter_rect = egui::Rect::from_min_size(
                    egui::pos2(left_allocated_rect.max.x, left_rect_start.y),
                    egui::vec2(splitter_width, viz_height),
                );
                
                // Allocate splitter space
                ui.allocate_space(egui::vec2(splitter_width, viz_height));
                
                let left_splitter_id = ui.make_persistent_id("viz_left_splitter");
                let left_splitter_response = ui.interact(left_splitter_rect, left_splitter_id, egui::Sense::drag());
                
                if left_splitter_response.dragged() {
                    if let Some(pos) = ui.ctx().pointer_interact_pos() {
                        let new_ratio = (pos.x / available_width).clamp(0.15, 0.40);
                        self.left_panel_ratio = new_ratio;
                    }
                }
                
                // Draw left splitter
                let left_splitter_color = if left_splitter_response.dragged() {
                    egui::Color32::from_rgb(100, 150, 200)
                } else if left_splitter_response.hovered() {
                    egui::Color32::from_rgb(80, 120, 180)
                } else {
                    egui::Color32::from_rgb(50, 55, 65)
                };
                ui.painter().rect_filled(left_splitter_rect, 0.0, left_splitter_color);

                // === Center panel: Treemap/Sunburst ===
                ui.allocate_ui_with_layout(
                    egui::vec2(center_width, viz_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_min_height(viz_height - 8.0);
                            self.draw_treemap_panel(ui, stats);
                        });
                    },
                );
                
                let center_allocated_rect = ui.min_rect();
                
                // === Right splitter ===
                let right_splitter_rect = egui::Rect::from_min_size(
                    egui::pos2(center_allocated_rect.max.x, left_rect_start.y),
                    egui::vec2(splitter_width, viz_height),
                );
                
                // Allocate splitter space
                ui.allocate_space(egui::vec2(splitter_width, viz_height));
                
                let right_splitter_id = ui.make_persistent_id("viz_right_splitter");
                let right_splitter_response = ui.interact(right_splitter_rect, right_splitter_id, egui::Sense::drag());
                
                if right_splitter_response.dragged() {
                    if let Some(pos) = ui.ctx().pointer_interact_pos() {
                        let new_ratio = ((available_width - pos.x) / available_width).clamp(0.10, 0.30);
                        self.right_panel_ratio = new_ratio;
                    }
                }
                
                // Draw right splitter
                let right_splitter_color = if right_splitter_response.dragged() {
                    egui::Color32::from_rgb(100, 150, 200)
                } else if right_splitter_response.hovered() {
                    egui::Color32::from_rgb(80, 120, 180)
                } else {
                    egui::Color32::from_rgb(50, 55, 65)
                };
                ui.painter().rect_filled(right_splitter_rect, 0.0, right_splitter_color);

                // === Right panel: Extension/File type distribution ===
                ui.allocate_ui_with_layout(
                    egui::vec2(right_width, viz_height),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        egui::Frame::group(ui.style()).show(ui, |ui| {
                            ui.set_min_height(viz_height - 8.0);
                            self.draw_extension_panel(ui, stats);
                        });
                    },
                );
            },
        );

        // Bottom: Tab bar with detailed data tables
        ui.add_space(6.0);
        ui.separator();
        self.draw_tabs(ui, stats);
    }

    /// Left panel: Directory tree view
    fn draw_directory_tree_panel(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        ui.heading("目录树");
        ui.add_space(4.0);
        
        if let Some(tree) = &stats.directory_tree {
            self.draw_tree_recursive(ui, tree, tree.root_index, 0, stats);
        } else if self.scan_in_progress {
            ui.label("扫描中，先显示已发现的顶层目录...");
            ui.label(format!("目录: {}", format::count(stats.dir_count)));
            ui.label(format!("文件: {}", format::count(stats.file_count)));
            ui.add_space(6.0);

            if stats.top_level_dirs.is_empty() {
                ui.label("正在发现顶层目录...");
            } else {
                egui::ScrollArea::vertical()
                    .id_salt("incremental_tree_scroll")
                    .max_height(260.0)
                    .show(ui, |ui| {
                        for dir in stats.top_level_dirs.iter().take(80) {
                            let name = dir
                                .path
                                .file_name()
                                .and_then(|name| name.to_str())
                                .unwrap_or_else(|| dir.path.to_str().unwrap_or("<unknown>"));
                            let label = if dir.total_size > 0 {
                                format!("{} ({})", name, format::bytes(dir.total_size))
                            } else {
                                name.to_owned()
                            };

                            if ui.small_button(label).clicked() {
                                self.treemap_current_dir = Some(dir.path.clone());
                            }
                        }
                    });
            }
        } else {
            ui.label("无数据");
        }
    }

    /// Recursive tree drawing with percentage bars, icons, and collapsible nodes
    fn draw_tree_recursive(&mut self, ui: &mut egui::Ui, tree: &DirectoryTree, node_index: usize, depth: usize, stats: &ScanStats) {
        if depth > 5 {
            return; // Limit depth for performance
        }

        let node = &tree.nodes[node_index];
        let has_children = !node.children.is_empty();
        let is_expanded = self.expanded_dirs.contains(&node.record.path);
        let indent = depth * 16;
        
        ui.horizontal(|ui| {
            ui.add_space(indent as f32);
            
            // Expand/collapse button for directories with children
            if has_children {
                let expand_icon = if is_expanded { "▼" } else { "▶" };
                let expand_button = egui::Button::new(egui::RichText::new(expand_icon).size(10.0))
                    .fill(egui::Color32::TRANSPARENT)
                    .small();
                if ui.add(expand_button).clicked() {
                    if is_expanded {
                        self.expanded_dirs.remove(&node.record.path);
                    } else {
                        self.expanded_dirs.insert(node.record.path.clone());
                    }
                }
            } else {
                ui.add_space(12.0); // Align with expand button width
            }
            
            // Add folder icon
            let icon = if has_children && is_expanded { "📂" } else { "📁" };
            ui.label(egui::RichText::new(icon).size(14.0));
            
            // Directory name
            let name = &node.record.name();
            let size_text = format::bytes(node.record.total_size);
            
            // Calculate percentage relative to parent or root
            let percentage = if stats.total_size > 0 {
                (node.record.total_size as f64 / stats.total_size as f64) * 100.0
            } else {
                0.0
            };
            
            // Use unified color for this directory type
            let path_str = node.record.path.to_string_lossy();
            let ext_hint = path_str.rsplit('.').next().unwrap_or("");
            let color = self.color_palette.color_for_extension(ext_hint);
            
            // Draw percentage progress bar
            ui.add(
                egui::ProgressBar::new((percentage / 100.0) as f32)
                    .desired_width(50.0)
                    .desired_height(8.0)
                    .fill(color),
            );
            
            // Show name and size - click to navigate to directory in treemap
            let text = format!("{} ({})", name, size_text);
            if ui.small_button(text).clicked() {
                self.treemap_current_dir = Some(node.record.path.clone());
            }
            
            // Show percentage label
            ui.label(egui::RichText::new(format!("{:.1}%", percentage)).small().weak());
        });

        // Only draw children if expanded (or at root level, always show first level)
        if has_children && (is_expanded || depth == 0) {
            for &child_index in &node.children {
                self.draw_tree_recursive(ui, tree, child_index, depth + 1, stats);
            }
        }
    }

    /// Right panel: File type distribution with unified color palette
    fn draw_extension_panel(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        ui.heading("文件类型");
        ui.add_space(4.0);
        
        if stats.extensions.is_empty() {
            if self.scan_in_progress {
                ui.label("统计中...");
            } else {
                ui.label("无数据");
            }
            return;
        }

        // Show clear filter button if extensions are selected
        if !self.selected_extensions.is_empty() {
            ui.horizontal(|ui| {
                if ui.small_button("✕ 清除筛选").clicked() {
                    self.selected_extensions.clear();
                }
                ui.label(format!("已选 {} 种类型", self.selected_extensions.len()));
            });
            ui.add_space(4.0);
        }

        // Show top extensions with unified color palette
        let total = stats.total_size as f64;
        for ext in stats.extensions.iter().take(15) {
            let percentage = if total > 0.0 {
                (ext.total_size as f64 / total) * 100.0
            } else {
                0.0
            };

            // Use unified color palette
            let ext_name = ext.extension.trim_start_matches('.').to_string();
            let color = self.color_palette.color_for_extension(&ext_name);
            let category = self.color_palette.category_for_extension(&ext_name);
            let is_selected = self.selected_extensions.contains(&ext.extension);

            ui.horizontal(|ui| {
                // Color indicator (clickable for selection)
                let color_label = if is_selected {
                    format!("✓ {}", category.icon())
                } else {
                    category.icon().to_string()
                };
                
                let color_response = ui.add(
                    egui::Label::new(egui::RichText::new(color_label).color(color))
                        .selectable(false)
                        .sense(egui::Sense::click())
                );
                
                // Click to select/highlight this extension type
                if color_response.clicked() {
                    if is_selected {
                        self.selected_extensions.remove(&ext.extension);
                    } else {
                        self.selected_extensions.insert(ext.extension.clone());
                    }
                }

                // Progress bar
                ui.add(
                    egui::ProgressBar::new((percentage / 100.0) as f32)
                        .desired_width(60.0)
                        .desired_height(8.0)
                        .fill(color),
                );
                
                // Extension name (highlighted if selected)
                let ext_text = if is_selected {
                    egui::RichText::new(&ext.extension).strong().color(color)
                } else {
                    egui::RichText::new(&ext.extension)
                };
                ui.label(ext_text);
                
                // Size and percentage
                ui.label(format!("{} ({:.1}%)", format::bytes(ext.total_size), percentage));
            });

            // Show category label on hover
            ui.add_space(1.0);
        }

        ui.add_space(8.0);
        ui.label(format!("共 {} 种类型", format::count(stats.extensions.len() as u64)));
        
        // Show legend for categories
        ui.add_space(4.0);
        ui.label(egui::RichText::new("颜色图例").small().weak());
        egui::Grid::new("category_legend")
            .num_columns(4)
            .spacing([8.0, 4.0])
            .show(ui, |ui| {
                for category in [
                    crate::color_palette::FileCategory::Executable,
                    crate::color_palette::FileCategory::Document,
                    crate::color_palette::FileCategory::Media,
                    crate::color_palette::FileCategory::Code,
                    crate::color_palette::FileCategory::System,
                    crate::color_palette::FileCategory::Temporary,
                    crate::color_palette::FileCategory::Archive,
                    crate::color_palette::FileCategory::Data,
                ] {
                    let color = category.default_color();
                    ui.colored_label(color, category.icon());
                    ui.label(egui::RichText::new(category.label()).small());
                }
            });
    }

    fn draw_summary(&self, ui: &mut egui::Ui, stats: &ScanStats) {
        ui.heading("概览");
        ui.add_space(4.0);
        egui::Grid::new("summary_grid")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                label_value(ui, "扫描根目录", stats.root.display().to_string());
                label_value(ui, "累计大小", format::bytes(stats.total_size));
                label_value(ui, "文件数量", format::count(stats.file_count));
                label_value(ui, "目录数量", format::count(stats.dir_count));
                label_value(ui, "访问错误", format::count(stats.error_count));

                if self.scan_in_progress {
                    let state = if self.cancel_requested {
                        "正在取消"
                    } else {
                        "正在扫描"
                    };
                    label_value(ui, "扫描状态", state.to_owned());

                    if let Some(path) = self
                        .progress
                        .as_ref()
                        .and_then(|progress| progress.current_path.as_ref())
                    {
                        label_value(
                            ui,
                            "当前路径",
                            compact_text(&path.display().to_string(), 48),
                        );
                    }
                }

                if let Some(preview) = self.current_cleanup_preview() {
                    label_value(ui, "预览候选", format::count(preview.candidate_count));
                    label_value(ui, "预计可清理", format::bytes(preview.reclaimable_size));
                    label_value(ui, "受保护候选", format::bytes(preview.protected_size));
                    if self.cleanup_in_progress {
                        let state = if self.cleanup_cancel_requested {
                            "正在取消预览"
                        } else {
                            "正在预览"
                        };
                        label_value(ui, "预览状态", state.to_owned());
                    }
                }

                if let Some(preview) = self.current_duplicate_preview() {
                    label_value(ui, "重复组", format::count(preview.duplicate_group_count));
                    label_value(ui, "重复副本", format::count(preview.duplicate_file_count));
                    label_value(ui, "预计可回收", format::bytes(preview.reclaimable_size));
                    if self.duplicate_in_progress {
                        let state = if self.duplicate_cancel_requested {
                            "正在取消重复检测"
                        } else if let Some(progress) = &self.duplicate_progress {
                            progress.phase.label()
                        } else {
                            "正在检测重复文件"
                        };
                        label_value(ui, "重复检测", state.to_owned());
                    }
                }

                if stats.filter_config.is_active() {
                    ui.label(RichText::new("扫描过滤").strong());
                    ui.label("已启用");
                    ui.end_row();

                    if !stats.filter_config.excluded_directories.is_empty() {
                        label_value(
                            ui,
                            "排除目录",
                            stats.filter_config.excluded_directories.join(", "),
                        );
                    }
                    if !stats.filter_config.excluded_extensions.is_empty() {
                        label_value(
                            ui,
                            "排除扩展名",
                            stats.filter_config.excluded_extensions.join(", "),
                        );
                    }
                    if stats.filter_config.same_file_system {
                        label_value(ui, "同文件系统", "是".to_owned());
                    }
                }
            });
        ui.add_space(8.0);
        ui.label(
            RichText::new(
                "安全提示：当前版本只分析空间占用，并且清理功能仅 dry-run 预览，不提供删除功能。",
            )
            .small(),
        );
        if self.scan_in_progress {
            ui.label(RichText::new("进度提示：程序不预扫描总文件数，因此不显示百分比。列表会持续刷新已扫描到的部分结果。").small());
        }
        if self.cleanup_in_progress {
            ui.label(RichText::new("清理预览提示：预览只读取文件元数据，不删除、移动或修改任何文件。受保护候选不计入预计可清理空间。")
                .small());
        }
        if self.duplicate_in_progress {
            ui.label(RichText::new("重复检测提示：仅对同大小候选读取文件内容并计算 BLAKE3 哈希；当前版本只提供 dry-run 预览。")
                .small());
        }
    }

    fn open_cache_manager(&mut self) {
        self.cache_manager_open = true;
        self.refresh_cache_entries();
    }

    fn refresh_cache_entries(&mut self) {
        match default_cache_db_path() {
            Ok(db_path) => {
                self.cache_db_path = Some(db_path.clone());
                self.cache_entries = list_scan_cache_entries(&db_path).unwrap_or_default();
                self.cache_db_size = get_cache_db_size(&db_path).ok().flatten();
            }
            Err(e) => {
                self.status_message = format!("无法获取缓存数据库路径: {}", e);
                self.cache_db_path = None;
                self.cache_entries = Vec::new();
                self.cache_db_size = None;
            }
        }
    }

    fn draw_cache_manager_window(&mut self, ctx: &egui::Context) {
        if !self.cache_manager_open {
            return;
        }

        // Collect data needed for the UI
        let db_path = self.cache_db_path.clone();
        let db_size = self.cache_db_size;
        let entries = self.cache_entries.clone();
        let delete_confirmation = self.cache_delete_confirmation.clone();

        // Actions to perform after the UI pass
        let mut load_entry_key: Option<String> = None;
        let mut delete_entry_key: Option<String> = None;
        let mut confirm_delete: bool = false;
        let mut cancel_delete: bool = false;
        let mut refresh_requested = false;

        egui::Window::new("缓存管理")
            .open(&mut self.cache_manager_open)
            .default_size([500.0, 400.0])
            .show(ctx, |ui| {
                // Database info
                ui.horizontal(|ui| {
                    ui.label(RichText::new("缓存数据库:").strong());
                    if let Some(ref path) = db_path {
                        ui.label(path.display().to_string());
                    }
                });

                ui.horizontal(|ui| {
                    ui.label(RichText::new("数据库大小:").strong());
                    if let Some(size) = db_size {
                        ui.label(format::bytes(size));
                    } else {
                        ui.label("未知");
                    }
                });

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Cache entries list
                ui.heading("已保存的扫描结果");
                ui.add_space(4.0);

                if entries.is_empty() {
                    ui.label("暂无已保存的扫描结果。");
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(250.0)
                        .show(ui, |ui| {
                            for entry in &entries {
                                ui.group(|ui| {
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new(&entry.root_display).strong());
                                    });
                                    ui.horizontal_wrapped(|ui| {
                                        ui.label(format!(
                                            "保存时间: {}",
                                            format_saved_at_time(entry.saved_at_unix_secs)
                                        ));
                                        ui.label(format!(
                                            "大小: {}",
                                            format::bytes(entry.total_size)
                                        ));
                                        ui.label(format!(
                                            "文件: {}",
                                            format::count(entry.file_count)
                                        ));
                                    });
                                    ui.horizontal(|ui| {
                                        if ui.small_button("加载").clicked() {
                                            load_entry_key = Some(entry.root_key.clone());
                                        }
                                        if ui.small_button("删除").clicked() {
                                            delete_entry_key = Some(entry.root_key.clone());
                                        }
                                    });
                                });
                                ui.add_space(4.0);
                            }
                        });
                }

                // Delete confirmation dialog
                if let Some(ref root_key) = delete_confirmation {
                    ui.add_space(8.0);
                    ui.separator();
                    ui.add_space(4.0);
                    egui::Frame::NONE
                        .fill(egui::Color32::from_rgba_unmultiplied(255, 200, 200, 50))
                        .inner_margin(8.0)
                        .show(ui, |ui| {
                            ui.horizontal(|ui| {
                                ui.label(format!("确定要删除此缓存记录吗？路径: {}", root_key));
                            });
                            ui.horizontal(|ui| {
                                if ui.small_button("确认删除").clicked() {
                                    confirm_delete = true;
                                }
                                if ui.small_button("取消").clicked() {
                                    cancel_delete = true;
                                }
                            });
                        });
                }

                ui.add_space(8.0);
                ui.separator();
                ui.add_space(4.0);

                // Refresh button
                if ui.button("刷新列表").clicked() {
                    refresh_requested = true;
                }
            });

        // Handle actions after the UI pass
        if let Some(key) = load_entry_key {
            if let Some(ref db_path) = self.cache_db_path {
                match load_scan_cache_by_root_key(db_path, &key) {
                    Ok(Some(stats)) => {
                        self.stats = Some(Arc::new(stats));
                        self.treemap_current_dir = None;
                        self.status_message = format!("已从缓存加载：{}", key);
                        self.cache_manager_open = false;
                    }
                    Ok(None) => {
                        self.status_message = "缓存记录不存在".to_owned();
                    }
                    Err(e) => {
                        self.status_message = format!("加载缓存失败: {}", e);
                    }
                }
            }
        }

        if let Some(key) = delete_entry_key {
            self.cache_delete_confirmation = Some(key);
        }

        if confirm_delete {
            if let Some(ref root_key) = self.cache_delete_confirmation {
                if let Some(ref db_path) = self.cache_db_path {
                    match delete_scan_cache_by_root_key(db_path, root_key) {
                        Ok(true) => {
                            self.status_message = format!("已删除缓存: {}", root_key);
                            self.refresh_cache_entries();
                        }
                        Ok(false) => {
                            self.status_message = "缓存记录不存在".to_owned();
                        }
                        Err(e) => {
                            self.status_message = format!("删除缓存失败: {}", e);
                        }
                    }
                }
            }
            self.cache_delete_confirmation = None;
        }

        if cancel_delete {
            self.cache_delete_confirmation = None;
        }

        if refresh_requested {
            self.refresh_cache_entries();
        }
    }

    fn draw_tabs(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        ui.horizontal(|ui| {
            tab_button(
                ui,
                &mut self.selected_tab,
                ResultTab::Directories,
                "最大目录",
            );
            tab_button(ui, &mut self.selected_tab, ResultTab::Files, "最大文件");
            tab_button(ui, &mut self.selected_tab, ResultTab::Types, "文件类型");
            tab_button(
                ui,
                &mut self.selected_tab,
                ResultTab::CleanupPreview,
                "清理预览",
            );
            tab_button(
                ui,
                &mut self.selected_tab,
                ResultTab::DuplicatePreview,
                "重复文件",
            );
            tab_button(ui, &mut self.selected_tab, ResultTab::AiReview, "AI 审核报告");
            tab_button(ui, &mut self.selected_tab, ResultTab::Errors, "错误");
        });
        ui.separator();
        self.draw_search_bar(ui);
        ui.add_space(4.0);

        match self.selected_tab {
            ResultTab::Directories => {
                if let Some(path) = directory_table(
                    ui,
                    stats,
                    &self.search_query,
                    &mut self.directory_sort,
                    &mut self.status_message,
                ) {
                    self.treemap_current_dir = if path == stats.root {
                        None
                    } else {
                        Some(path.clone())
                    };
                    self.status_message = format!("Treemap 已定位目录：{}", path.display());
                }
            }
            ResultTab::Files => file_table(
                ui,
                stats,
                &self.search_query,
                &mut self.file_sort,
                &mut self.status_message,
            ),
            ResultTab::Types => {
                extension_table(ui, stats, &self.search_query, &mut self.extension_sort)
            }
            ResultTab::CleanupPreview => self.draw_cleanup_preview_tab(ui),
            ResultTab::DuplicatePreview => self.draw_duplicate_preview_tab(ui),
            ResultTab::AiReview => self.draw_ai_review_tab(ui),
            ResultTab::Errors => error_list(ui, stats, &self.search_query),
        }
    }

    fn draw_search_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("搜索当前结果：");
            ui.add(
                egui::TextEdit::singleline(&mut self.search_query)
                    .hint_text("输入名称、路径、类型或错误文本")
                    .desired_width(320.0),
            );
            if ui
                .add_enabled(!self.search_query.is_empty(), egui::Button::new("清空"))
                .clicked()
            {
                self.search_query.clear();
            }
        });
        ui.label(
            RichText::new("目录标签输入关键词后会搜索完整目录树；文件标签在新扫描/新缓存中可搜索完整文件索引。类型、错误、清理预览和重复文件搜索当前保留结果。")
                .small()
                .weak(),
        );
    }

    fn draw_cleanup_preview_tab(&mut self, ui: &mut egui::Ui) {
        self.draw_cleanup_rules_panel(ui);
        ui.separator();
        cleanup_preview_table(
            ui,
            self.current_cleanup_preview().as_deref(),
            &self.search_query,
            &mut self.cleanup_sort,
            &mut self.status_message,
        );
    }

    fn draw_duplicate_preview_tab(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("最小候选大小:");
            let mut min_size_text = self.duplicate_min_size_bytes.to_string();
            let response = ui.add(
                egui::TextEdit::singleline(&mut min_size_text)
                    .hint_text("字节")
                    .desired_width(80.0),
            );
            if response.changed() {
                if let Ok(value) = min_size_text.parse::<u64>() {
                    self.duplicate_min_size_bytes = value.max(1);
                    self.invalidate_duplicate_preview_after_config_change();
                }
            }
            ui.label(format!(
                "(当前: {})",
                format::bytes(self.duplicate_min_size_bytes)
            ));
        });
        ui.label(
            RichText::new("仅对大小 >= 最小候选的文件进行哈希比对。配置变更会清空已有预览。")
                .small()
                .weak(),
        );
        ui.separator();
        duplicate_preview_table(
            ui,
            self.current_duplicate_preview().as_deref(),
            &self.search_query,
            &mut self.duplicate_sort,
            &mut self.status_message,
        );
    }

    fn draw_ai_review_tab(&mut self, ui: &mut egui::Ui) {
        self.draw_ai_config_panel(ui);
        ui.separator();
        ai_review_table(
            ui,
            self.current_ai_analysis_report().as_deref(),
            &self.search_query,
            &mut self.ai_sort,
            &mut self.status_message,
        );
    }

    fn draw_ai_config_panel(&mut self, ui: &mut egui::Ui) {
        ui.collapsing("AI 分析配置（OpenAI 兼容 API）", |ui| {
            ui.label(
                RichText::new("API Key 只从环境变量读取，不会保存、显示、日志记录或导出。默认不会向云 API 发送完整路径/文件名。")
                    .small()
                    .weak(),
            );
            let busy = self.ai_analysis_in_progress;
            egui::Grid::new("ai_config_grid")
                .num_columns(2)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    ui.label("Base URL");
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut self.ai_provider_config.base_url)
                            .desired_width(280.0),
                    );
                    ui.end_row();

                    ui.label("模型");
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut self.ai_provider_config.model)
                            .desired_width(180.0),
                    );
                    ui.end_row();

                    ui.label("API Key 环境变量");
                    ui.add_enabled(
                        !busy,
                        egui::TextEdit::singleline(&mut self.ai_provider_config.api_key_env)
                            .desired_width(180.0),
                    );
                    ui.end_row();

                    ui.label("最多候选数");
                    let mut max_candidates_text = self.ai_provider_config.max_candidates.to_string();
                    if ui
                        .add_enabled(
                            !busy,
                            egui::TextEdit::singleline(&mut max_candidates_text)
                                .desired_width(80.0),
                        )
                        .changed()
                    {
                        if let Ok(value) = max_candidates_text.parse::<usize>() {
                            self.ai_provider_config.max_candidates = value.clamp(1, 1000);
                        }
                    }
                    ui.end_row();

                    ui.label("超时秒数");
                    let mut timeout_text = self.ai_provider_config.timeout_secs.to_string();
                    if ui
                        .add_enabled(
                            !busy,
                            egui::TextEdit::singleline(&mut timeout_text).desired_width(80.0),
                        )
                        .changed()
                    {
                        if let Ok(value) = timeout_text.parse::<u64>() {
                            self.ai_provider_config.timeout_secs = value.clamp(5, 600);
                        }
                    }
                    ui.end_row();
                });

            ui.add_enabled(
                !busy,
                egui::Checkbox::new(
                    &mut self.ai_provider_config.send_full_paths,
                    "允许向云 API 发送完整路径/文件名",
                ),
            );
            ui.label(
                RichText::new("关闭时云端只接收脱敏路径、大小、扩展名、来源、风险提示和规则原因；本地导出的报告仍包含真实路径，方便用户人工确认。")
                    .small()
                    .weak(),
            );
        });
    }

    fn draw_cleanup_rules_panel(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("清理规则").strong());
            ui.label(format!(
                "已启用 {} / {} 条",
                format::count(self.enabled_cleanup_rule_count() as u64),
                format::count(self.cleanup_rules.len() as u64)
            ));
            if ui
                .add_enabled(!self.cleanup_in_progress, egui::Button::new("全选"))
                .clicked()
            {
                self.set_all_cleanup_rules_enabled(true);
            }
            if ui
                .add_enabled(!self.cleanup_in_progress, egui::Button::new("全不选"))
                .clicked()
            {
                self.set_all_cleanup_rules_enabled(false);
            }
            if ui
                .add_enabled(!self.cleanup_in_progress, egui::Button::new("恢复默认"))
                .clicked()
            {
                self.reset_cleanup_rules();
            }
        });
        ui.label(
            RichText::new("规则变更会清空已有预览；请重新点击\"生成清理预览\"。")
                .small()
                .weak(),
        );

        let mut changed = false;
        egui::Grid::new("cleanup_rules_grid")
            .striped(true)
            .num_columns(4)
            .spacing([12.0, 4.0])
            .show(ui, |ui| {
                plain_header(ui, "启用");
                plain_header(ui, "规则");
                plain_header(ui, "风险");
                plain_header(ui, "说明");
                ui.end_row();

                for state in &mut self.cleanup_rules {
                    let response = ui.add_enabled(
                        !self.cleanup_in_progress,
                        egui::Checkbox::new(&mut state.enabled, ""),
                    );
                    changed |= response.changed();
                    ui.label(state.rule.label);
                    ui.label(state.rule.risk.label());
                    ui.label(state.rule.description);
                    ui.end_row();
                }
            });

        if changed {
            self.invalidate_cleanup_preview_after_rule_change();
        }
    }

    fn draw_treemap_panel(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        // During scanning, use top_level_dirs for incremental display (faster, hierarchical)
        // After completion, use full directory tree
        let (treemap_items, current_size, is_scanning) = if let Some(tree) = stats.directory_tree.as_ref() {
            // Full tree available - use detailed visualization
            let current_dir = self.treemap_current_dir(stats);
            let current_index = tree
                .node_index_for_path(&current_dir)
                .unwrap_or(tree.root_index);
            let current_node = &tree.nodes[current_index];
            let current_size = current_node.record.total_size;
            let mut treemap_items = treemap_items_for_node(tree, current_index);
            let display_limit = 36;
            treemap_items = treemap_items_with_other(treemap_items, current_dir.clone(), display_limit);
            (treemap_items, current_size, false)
        } else {
            // Scanning in progress - use top_level_dirs for hierarchical incremental display
            // This shows direct children of root, which is more meaningful than global largest_dirs
            let items: Vec<TreemapItem> = if stats.top_level_dirs.iter().any(|dir| dir.total_size > 0) {
                stats
                    .top_level_dirs
                    .iter()
                    .map(|dir| TreemapItem::directory(
                        dir.path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| dir.path.display().to_string()),
                        dir.path.clone(),
                        dir.total_size,
                    ))
                    .collect()
            } else {
                stats
                    .largest_dirs
                    .iter()
                    .take(36)
                    .filter(|dir| dir.total_size > 0)
                    .map(|dir| TreemapItem::directory(
                        dir.path
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(|s| s.to_string())
                            .unwrap_or_else(|| dir.path.display().to_string()),
                        dir.path.clone(),
                        dir.total_size,
                    ))
                    .collect()
            };
            let display_limit = 36;
            let items = treemap_items_with_other(items, stats.root.clone(), display_limit);
            (items, stats.total_size, true)
        };

        // Draw header and visualization mode selector
        ui.horizontal_wrapped(|ui| {
            ui.heading("空间占用图");
            ui.separator();
            ui.selectable_value(
                &mut self.visualization_mode,
                VisualizationMode::Treemap,
                "Treemap",
            );
            ui.selectable_value(
                &mut self.visualization_mode,
                VisualizationMode::Sunburst,
                "旭日图",
            );
        });

        // Show appropriate message based on scan state
        if is_scanning {
            ui.label(
                RichText::new("扫描中实时预览：基于当前已发现的最大目录显示。扫描完成后可进入子目录查看详情。")
                    .small()
                    .weak(),
            );
        } else if let Some(tree) = stats.directory_tree.as_ref() {
            let current_dir = self.treemap_current_dir(stats);
            let current_index = tree
                .node_index_for_path(&current_dir)
                .unwrap_or(tree.root_index);
            let current_node = &tree.nodes[current_index];
            let child_count = current_node.children.len();
            
            self.draw_breadcrumb_navigation(ui, stats, &current_dir);
            
            ui.label(
                RichText::new(format!(
                    "完整目录树：{} 个直接子目录，直属文件 {} 个 / {}，当前图表显示 {} 个块。",
                    format::count(child_count as u64),
                    format::count(current_node.record.direct_file_count),
                    format::bytes(current_node.record.direct_file_size),
                    format::count(treemap_items.len() as u64),
                ))
                .small()
                .weak(),
            );
        }

        let empty_message = if is_scanning && stats.file_count == 0 {
            "正在读取文件大小，稍候显示空间占用图..."
        } else if is_scanning {
            "正在扫描中..."
        } else if stats.total_size == 0 {
            "扫描后显示目录空间占用图"
        } else {
            "当前目录没有可显示的子目录"
        };

        // Draw visualization (only Treemap during scanning, full options after completion)
        if is_scanning {
            draw_treemap(ui, &treemap_items, current_size, empty_message, &self.color_palette, None);
        } else if let Some(tree) = stats.directory_tree.as_ref() {
            let current_dir = self.treemap_current_dir(stats);
            let current_index = tree
                .node_index_for_path(&current_dir)
                .unwrap_or(tree.root_index);
            
            // Create selected extensions reference after getting current_dir to avoid borrow conflict
            let selected_ext_ref = Some(&self.selected_extensions);
            
            let action = match self.visualization_mode {
                VisualizationMode::Treemap => {
                    draw_treemap(ui, &treemap_items, current_size, empty_message, &self.color_palette, selected_ext_ref)
                }
                VisualizationMode::Sunburst => {
                    draw_sunburst(ui, tree, current_index, current_size, empty_message, &self.color_palette)
                }
            };

            if let Some(action) = action {
                self.handle_treemap_action(action, ui.ctx());
            }
        }
    }

    fn treemap_current_dir(&mut self, stats: &ScanStats) -> PathBuf {
        let Some(tree) = stats.directory_tree.as_ref() else {
            self.treemap_current_dir = None;
            return stats.root.clone();
        };

        let Some(current) = &self.treemap_current_dir else {
            return stats.root.clone();
        };

        if current == &stats.root {
            return stats.root.clone();
        }

        let exists =
            current.starts_with(&stats.root) && tree.node_index_for_path(current).is_some();
        if exists {
            current.clone()
        } else {
            self.treemap_current_dir = None;
            stats.root.clone()
        }
    }

    fn draw_breadcrumb_navigation(
        &mut self,
        ui: &mut egui::Ui,
        stats: &ScanStats,
        current_dir: &Path,
    ) {
        let root_display = stats.root.display().to_string();
        let path_parts: Vec<&str> = if current_dir == &stats.root {
            vec![]
        } else {
            current_dir
                .iter()
                .map(|p| p.to_str().unwrap_or(""))
                .collect()
        };

        ui.horizontal_wrapped(|ui| {
            // Show parent directory button if not at root
            if current_dir != &stats.root {
                if ui.small_button("返回上级").clicked() {
                    self.treemap_current_dir = Some(treemap_parent_dir(stats, current_dir));
                }
                if ui.small_button("返回根目录").clicked() {
                    self.treemap_current_dir = None;
                }
                ui.separator();
            }

            ui.label(RichText::new(&root_display).strong());

            // Show breadcrumb path segments
            for part in &path_parts {
                if !part.is_empty() {
                    ui.label(" > ");
                    ui.label(format!("{}", part));
                }
            }
        });
    }

    fn handle_treemap_action(&mut self, action: TreemapAction, ctx: &egui::Context) {
        match action {
            TreemapAction::Enter(path) => {
                self.status_message = format!("Treemap 已进入目录：{}", path.display());
                self.treemap_current_dir = Some(path);
            }
            TreemapAction::CopyPath(path) => {
                let text = path.display().to_string();
                ctx.copy_text(text.clone());
                self.status_message = format!("已复制路径：{}", text);
            }
            TreemapAction::OpenLocation(path) => {
                open_path(&path, &mut self.status_message);
            }
        }
    }
}

impl eframe::App for CDriveManagerApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.poll_scan_events(ctx);
        self.poll_cleanup_preview_events(ctx);
        self.poll_duplicate_preview_events(ctx);
        self.poll_ai_analysis_events(ctx);
        self.draw_top_bar(ctx);
        self.draw_cache_manager_window(ctx);

        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(stats) = self.current_stats() else {
                ui.vertical_centered(|ui| {
                    ui.add_space(80.0);
                    ui.heading("参考 WinDirStat / QDirStat 的 Rust 桌面空间分析工具");
                    ui.label("输入目录并开始扫描，程序会在后台统计空间占用。  ");
                    ui.label("第一版不会删除任何文件。  ");
                });
                return;
            };

            // WizTree-style three-column layout
            self.draw_wiztree_layout(ui, stats.as_ref());
        });
    }
}

fn default_root() -> String {
    if cfg!(windows) {
        "C:\\".to_owned()
    } else {
        "/".to_owned()
    }
}

fn label_value(ui: &mut egui::Ui, label: &str, value: String) {
    ui.label(RichText::new(label).strong());
    ui.label(value);
    ui.end_row();
}

fn tab_button(ui: &mut egui::Ui, selected: &mut ResultTab, value: ResultTab, label: &str) {
    if ui.selectable_label(*selected == value, label).clicked() {
        *selected = value;
    }
}

fn treemap_items_for_node(tree: &DirectoryTree, node_index: usize) -> Vec<TreemapItem> {
    let node = &tree.nodes[node_index];
    let mut items: Vec<_> = node
        .children
        .iter()
        .filter_map(|child_index| tree.nodes.get(*child_index))
        .map(|child| {
            TreemapItem::directory(
                child.record.name(),
                child.record.path.clone(),
                child.record.total_size,
            )
        })
        .collect();

    if node.record.direct_file_size > 0 {
        items.push(TreemapItem::direct_files(
            node.record.path.clone(),
            node.record.direct_file_count,
            node.record.direct_file_size,
        ));
    }

    items.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.label.cmp(&right.label))
    });
    items
}

fn treemap_items_with_other(
    mut items: Vec<TreemapItem>,
    dir: PathBuf,
    limit: usize,
) -> Vec<TreemapItem> {
    if items.len() <= limit || limit < 2 {
        return items;
    }

    let hidden_items = items.split_off(limit - 1);
    let hidden_count = hidden_items.len();
    let hidden_size = hidden_items
        .iter()
        .fold(0_u64, |total, item| total.saturating_add(item.size));

    if hidden_size > 0 {
        items.push(TreemapItem::other(dir, hidden_count, hidden_size));
    }

    items
}

fn treemap_parent_dir(stats: &ScanStats, current_dir: &Path) -> PathBuf {
    if current_dir == stats.root {
        return stats.root.clone();
    }

    current_dir
        .parent()
        .filter(|parent| parent.starts_with(&stats.root))
        .map(Path::to_path_buf)
        .unwrap_or_else(|| stats.root.clone())
}

fn directory_table(
    ui: &mut egui::Ui,
    stats: &ScanStats,
    search_query: &str,
    sort: &mut SortState<DirectorySortKey>,
    status_message: &mut String,
) -> Option<PathBuf> {
    let query = normalized_query(search_query);
    let use_full_tree_search = !query.is_empty() && stats.directory_tree.is_some();
    let mut directories: Vec<_> = if use_full_tree_search {
        stats
            .directory_tree
            .as_ref()
            .map(|tree| tree.nodes.iter().map(|node| &node.record).collect())
            .unwrap_or_default()
    } else {
        stats.largest_dirs.iter().collect()
    };
    directories.retain(|dir| directory_matches(dir, &query));
    directories.sort_by(|left, right| compare_directories(left, right, *sort));

    result_count_label(
        ui,
        directories.len().min(120),
        directories.len(),
        if use_full_tree_search {
            stats
                .directory_tree
                .as_ref()
                .map(|tree| tree.nodes.len())
                .unwrap_or(stats.largest_dirs.len())
        } else {
            stats.largest_dirs.len()
        },
        if use_full_tree_search {
            "完整目录"
        } else {
            "目录"
        },
    );
    if use_full_tree_search {
        ui.label(
            RichText::new("当前目录搜索基于完整目录树；清空搜索后回到最大目录列表。")
                .small()
                .weak(),
        );
    }

    if directories.is_empty() {
        ui.label("没有匹配的目录。");
        return None;
    }

    let mut jump_to = None;
    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            egui::Grid::new("directory_table")
                .striped(true)
                .num_columns(6)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(ui, "目录", DirectorySortKey::Name, SortDirection::Asc, sort);
                    sortable_header(
                        ui,
                        "大小",
                        DirectorySortKey::Size,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "占比",
                        DirectorySortKey::Percent,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "文件",
                        DirectorySortKey::Files,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(ui, "路径", DirectorySortKey::Path, SortDirection::Asc, sort);
                    plain_header(ui, "操作");
                    ui.end_row();
                    for dir in directories.into_iter().take(120) {
                        if jump_to.is_none() {
                            jump_to = directory_row(ui, dir, stats.total_size, status_message);
                        } else {
                            let _ = directory_row(ui, dir, stats.total_size, status_message);
                        }
                    }
                });
        });

    jump_to
}

fn directory_matches(dir: &DirectoryRecord, query: &str) -> bool {
    query.is_empty()
        || text_matches(&dir.name(), query)
        || text_matches(&dir.path.display().to_string(), query)
}

fn directory_row(
    ui: &mut egui::Ui,
    dir: &DirectoryRecord,
    total_size: u64,
    status_message: &mut String,
) -> Option<PathBuf> {
    let name = dir.name();
    let name_response = ui
        .label(compact_text(&name, 32))
        .on_hover_text(name.clone());
    path_context_menu(name_response, &dir.path, &dir.path, &name, status_message);

    ui.label(format::bytes(dir.total_size));
    ui.label(format::percent(dir.total_size, total_size));
    ui.label(format::count(dir.descendant_file_count));

    let path_text = dir.path.display().to_string();
    let path_response = ui
        .label(compact_text(&path_text, 72))
        .on_hover_text(path_text.clone());
    path_context_menu(path_response, &dir.path, &dir.path, &name, status_message);

    let mut jump_to = None;
    ui.horizontal(|ui| {
        if ui.small_button("定位图中").clicked() {
            jump_to = Some(dir.path.clone());
        }
        path_actions(ui, &dir.path, &dir.path, status_message);
    });
    ui.end_row();
    jump_to
}

fn file_table(
    ui: &mut egui::Ui,
    stats: &ScanStats,
    search_query: &str,
    sort: &mut SortState<FileSortKey>,
    status_message: &mut String,
) {
    let query = normalized_query(search_query);
    let use_full_file_search = !query.is_empty() && !stats.all_files.is_empty();
    let mut files: Vec<_> = if use_full_file_search {
        stats.all_files.iter().collect()
    } else {
        stats.largest_files.iter().collect()
    };
    files.retain(|file| file_matches(file, &query));
    files.sort_by(|left, right| compare_files(left, right, *sort));

    result_count_label(
        ui,
        files.len().min(120),
        files.len(),
        if use_full_file_search {
            stats.all_files.len()
        } else {
            stats.largest_files.len()
        },
        if use_full_file_search {
            "完整文件"
        } else {
            "文件"
        },
    );
    if use_full_file_search {
        ui.label(
            RichText::new("当前文件搜索基于完整文件索引；清空搜索后回到最大文件列表。")
                .small()
                .weak(),
        );
    } else if !query.is_empty()
        && stats.all_files.is_empty()
        && stats.file_count > stats.largest_files.len() as u64
    {
        ui.label(
            RichText::new("当前结果没有完整文件索引（可能来自旧缓存或扫描中快照），只能搜索已保留的最大文件。")
                .small()
                .weak(),
        );
    }

    if files.is_empty() {
        ui.label("没有匹配的文件。");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            egui::Grid::new("file_table")
                .striped(true)
                .num_columns(5)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(ui, "文件", FileSortKey::Name, SortDirection::Asc, sort);
                    sortable_header(ui, "大小", FileSortKey::Size, SortDirection::Desc, sort);
                    sortable_header(ui, "类型", FileSortKey::Extension, SortDirection::Asc, sort);
                    sortable_header(ui, "路径", FileSortKey::Path, SortDirection::Asc, sort);
                    plain_header(ui, "操作");
                    ui.end_row();
                    for file in files.into_iter().take(120) {
                        file_row(ui, file, status_message);
                    }
                });
        });
}

fn file_matches(file: &FileRecord, query: &str) -> bool {
    query.is_empty()
        || text_matches(&file_name(&file.path), query)
        || text_matches(&file.path.display().to_string(), query)
        || text_matches(&file.extension, query)
}

fn file_row(ui: &mut egui::Ui, file: &FileRecord, status_message: &mut String) {
    let name = file_name(&file.path);
    let open_target = file.path.parent().unwrap_or(file.path.as_path());

    let name_response = ui
        .label(compact_text(&name, 36))
        .on_hover_text(name.clone());
    path_context_menu(
        name_response,
        &file.path,
        open_target,
        &name,
        status_message,
    );

    ui.label(format::bytes(file.size));
    ui.label(&file.extension);

    let path_text = file.path.display().to_string();
    let path_response = ui
        .label(compact_text(&path_text, 72))
        .on_hover_text(path_text.clone());
    path_context_menu(
        path_response,
        &file.path,
        open_target,
        &name,
        status_message,
    );

    path_actions(ui, &file.path, open_target, status_message);
    ui.end_row();
}

fn path_actions(
    ui: &mut egui::Ui,
    copy_path: &Path,
    open_target: &Path,
    status_message: &mut String,
) {
    ui.horizontal(|ui| {
        if ui.small_button("复制路径").clicked() {
            copy_path_to_clipboard(ui, copy_path, status_message);
        }

        if ui.small_button("打开位置").clicked() {
            open_path(open_target, status_message);
        }
    });
}

fn path_context_menu(
    response: egui::Response,
    copy_path: &Path,
    open_target: &Path,
    name: &str,
    status_message: &mut String,
) {
    response.context_menu(|ui| {
        if ui.button("复制路径").clicked() {
            copy_path_to_clipboard(ui, copy_path, status_message);
            ui.close();
        }

        if ui.button("复制名称").clicked() {
            copy_text_to_clipboard(ui, name.to_owned(), "已复制名称", status_message);
            ui.close();
        }

        ui.separator();

        if ui.button("打开位置").clicked() {
            open_path(open_target, status_message);
            ui.close();
        }
    });
}

fn copy_path_to_clipboard(ui: &egui::Ui, path: &Path, status_message: &mut String) {
    let text = path.display().to_string();
    copy_text_to_clipboard(ui, text, "已复制路径", status_message);
}

fn copy_text_to_clipboard(
    ui: &egui::Ui,
    text: String,
    success_prefix: &str,
    status_message: &mut String,
) {
    ui.ctx().copy_text(text.clone());
    *status_message = format!("{}：{}", success_prefix, text);
}

fn open_path(open_target: &Path, status_message: &mut String) {
    match open::that_detached(open_target) {
        Ok(()) => {
            *status_message = format!("已请求打开：{}", open_target.display());
        }
        Err(error) => {
            *status_message = format!("打开位置失败：{} ({})", open_target.display(), error);
        }
    }
}

fn save_scan_result_to_path(path: &Path, stats: &ScanStats) -> anyhow::Result<()> {
    let file = File::create(path)?;
    let writer = BufWriter::new(file);
    let mut stats = stats.clone();
    stats.prepare_for_save();
    serde_json::to_writer_pretty(writer, &stats)?;
    Ok(())
}

fn load_scan_result_from_path(path: &Path) -> anyhow::Result<ScanStats> {
    let file = File::open(path)?;
    let mut stats: ScanStats = serde_json::from_reader(file)?;
    stats.normalize_cache_metadata_after_load();
    stats.rebuild_indexes();
    Ok(stats)
}

fn scan_cache_metadata_summary(stats: &ScanStats) -> String {
    let saved_at = stats
        .saved_at_unix_secs
        .map(|saved_at| format!("saved_at={}", saved_at))
        .unwrap_or_else(|| "旧缓存，无保存时间".to_owned());
    let app_version = stats.app_version.as_deref().unwrap_or("未知版本");
    format!(
        "schema v{}，{}，app {}",
        stats.schema_version, saved_at, app_version
    )
}

fn export_csv_report_to_path(path: &Path, stats: &ScanStats) -> anyhow::Result<String> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    write_csv_row(
        &mut writer,
        [
            "kind",
            "name",
            "path",
            "size_bytes",
            "size_display",
            "file_count",
            "extension",
            "modified_unix_secs",
            "message",
        ],
    )?;

    write_summary_csv_rows(&mut writer, stats)?;

    let directory_count = if let Some(tree) = &stats.directory_tree {
        let mut directories: Vec<_> = tree.nodes.iter().map(|node| &node.record).collect();
        directories.sort_by(|left, right| compare_directories_for_export(left, right));
        for directory in &directories {
            write_directory_csv_row(&mut writer, directory)?;
        }
        directories.len()
    } else {
        for directory in &stats.largest_dirs {
            write_directory_csv_row(&mut writer, directory)?;
        }
        stats.largest_dirs.len()
    };

    let exported_files = if stats.all_files.is_empty() {
        &stats.largest_files
    } else {
        &stats.all_files
    };
    for file in exported_files {
        write_file_csv_row(&mut writer, file)?;
    }

    for extension in &stats.extensions {
        write_extension_csv_row(&mut writer, extension)?;
    }

    for error in &stats.errors {
        write_error_csv_row(&mut writer, error)?;
    }

    writer.flush()?;

    let directory_scope = if stats.directory_tree.is_some() {
        "完整目录"
    } else {
        "Top 目录"
    };
    let file_scope = if stats.all_files.is_empty() {
        "最大文件"
    } else {
        "完整文件"
    };
    Ok(format!(
        "{} {}，{} 个{}，{} 个类型，{} 条错误",
        format::count(directory_count as u64),
        directory_scope,
        format::count(exported_files.len() as u64),
        file_scope,
        format::count(stats.extensions.len() as u64),
        format::count(stats.errors.len() as u64)
    ))
}

fn write_summary_csv_rows(writer: &mut impl Write, stats: &ScanStats) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "summary".to_owned(),
            "root".to_owned(),
            path_text(&stats.root),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ],
    )?;
    write_csv_row(
        writer,
        [
            "summary".to_owned(),
            "total_size_bytes".to_owned(),
            String::new(),
            stats.total_size.to_string(),
            format::bytes(stats.total_size),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
        ],
    )?;
    write_csv_row(
        writer,
        [
            "summary".to_owned(),
            "file_count".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            stats.file_count.to_string(),
            String::new(),
            String::new(),
            String::new(),
        ],
    )?;
    write_csv_row(
        writer,
        [
            "summary".to_owned(),
            "dir_count".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            stats.dir_count.to_string(),
            String::new(),
            String::new(),
            String::new(),
        ],
    )?;
    write_csv_row(
        writer,
        [
            "summary".to_owned(),
            "error_count".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            stats.error_count.to_string(),
            String::new(),
            String::new(),
            String::new(),
        ],
    )?;
    Ok(())
}

fn write_directory_csv_row(
    writer: &mut impl Write,
    directory: &DirectoryRecord,
) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "directory".to_owned(),
            directory.name(),
            path_text(&directory.path),
            directory.total_size.to_string(),
            format::bytes(directory.total_size),
            directory.descendant_file_count.to_string(),
            String::new(),
            String::new(),
            format!(
                "direct_file_size={}; direct_file_count={}",
                directory.direct_file_size, directory.direct_file_count
            ),
        ],
    )
}

fn write_file_csv_row(writer: &mut impl Write, file: &FileRecord) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "file".to_owned(),
            file_name(&file.path),
            path_text(&file.path),
            file.size.to_string(),
            format::bytes(file.size),
            String::new(),
            file.extension.clone(),
            modified_unix_secs(file),
            String::new(),
        ],
    )
}

fn write_extension_csv_row(
    writer: &mut impl Write,
    extension: &ExtensionRecord,
) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "extension".to_owned(),
            extension.extension.clone(),
            String::new(),
            extension.total_size.to_string(),
            format::bytes(extension.total_size),
            extension.file_count.to_string(),
            extension.extension.clone(),
            String::new(),
            String::new(),
        ],
    )
}

fn write_error_csv_row(writer: &mut impl Write, error: &str) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "error".to_owned(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            String::new(),
            error.to_owned(),
        ],
    )
}

fn write_csv_row<S>(
    writer: &mut impl Write,
    cells: impl IntoIterator<Item = S>,
) -> anyhow::Result<()>
where
    S: AsRef<str>,
{
    let mut first = true;
    for cell in cells {
        if first {
            first = false;
        } else {
            writer.write_all(b",")?;
        }
        writer.write_all(csv_cell(cell.as_ref()).as_bytes())?;
    }
    writer.write_all(b"\n")?;
    Ok(())
}

fn csv_cell(value: &str) -> String {
    if value.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", value.replace('"', "\"\""))
    } else {
        value.to_owned()
    }
}

fn compare_directories_for_export(left: &DirectoryRecord, right: &DirectoryRecord) -> Ordering {
    right
        .total_size
        .cmp(&left.total_size)
        .then_with(|| left.path.cmp(&right.path))
}

fn modified_unix_secs(file: &FileRecord) -> String {
    system_time_unix_secs(file.modified)
}

fn system_time_unix_secs(time: Option<std::time::SystemTime>) -> String {
    time.and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_default()
}

fn path_text(path: &Path) -> String {
    path.display().to_string()
}

fn export_cleanup_preview_to_path(path: &Path, preview: &CleanupPreview) -> anyhow::Result<String> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    write_csv_row(
        &mut writer,
        [
            "mode",
            "action_taken",
            "rule_id",
            "rule_label",
            "risk",
            "protected",
            "candidate_kind",
            "path",
            "size_bytes",
            "size_display",
            "modified_unix_secs",
            "reason",
        ],
    )?;

    for candidate in &preview.candidates {
        write_cleanup_candidate_csv_row(&mut writer, candidate)?;
    }

    writer.flush()?;

    Ok(format!(
        "dry-run，保留 {} / 总计 {} 个候选，候选总大小 {}，预计可清理 {}，受保护 {} 个 / {}",
        format::count(preview.candidates.len() as u64),
        format::count(preview.candidate_count),
        format::bytes(preview.total_candidate_size),
        format::bytes(preview.reclaimable_size),
        format::count(preview.protected_count),
        format::bytes(preview.protected_size)
    ))
}

fn write_cleanup_candidate_csv_row(
    writer: &mut impl Write,
    candidate: &CleanupCandidate,
) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "dry-run".to_owned(),
            "none".to_owned(),
            candidate.rule_id.to_owned(),
            candidate.rule_label.to_owned(),
            candidate.risk.as_str().to_owned(),
            candidate.protected.to_string(),
            candidate.kind.as_str().to_owned(),
            path_text(&candidate.path),
            candidate.size.to_string(),
            format::bytes(candidate.size),
            system_time_unix_secs(candidate.modified),
            candidate.reason.clone(),
        ],
    )
}

fn export_duplicate_preview_to_path(
    path: &Path,
    preview: &DuplicatePreview,
) -> anyhow::Result<String> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    write_csv_row(
        &mut writer,
        [
            "mode",
            "action_taken",
            "group_hash",
            "group_size_bytes",
            "group_size_display",
            "duplicate_count",
            "reclaimable_bytes",
            "protected_bytes",
            "file_role",
            "protected",
            "path",
            "modified_unix_secs",
        ],
    )?;

    for group in &preview.groups {
        for file in &group.files {
            write_duplicate_file_csv_row(&mut writer, group, file)?;
        }
    }

    writer.flush()?;

    Ok(format!(
        "dry-run，保留 {} / 总计 {} 个重复组，重复副本 {} 个，预计可回收 {}，受保护 {}",
        format::count(preview.groups.len() as u64),
        format::count(preview.duplicate_group_count),
        format::count(preview.duplicate_file_count),
        format::bytes(preview.reclaimable_size),
        format::bytes(preview.protected_size)
    ))
}

fn write_duplicate_file_csv_row(
    writer: &mut impl Write,
    group: &DuplicateGroup,
    file: &DuplicateFile,
) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "dry-run".to_owned(),
            "none".to_owned(),
            group.hash.clone(),
            group.size.to_string(),
            format::bytes(group.size),
            group.duplicate_count.to_string(),
            group.reclaimable_size.to_string(),
            group.protected_size.to_string(),
            if file.keep { "keep" } else { "duplicate" }.to_owned(),
            file.protected.to_string(),
            path_text(&file.path),
            system_time_unix_secs(file.modified),
        ],
    )
}

fn export_ai_analysis_report_to_path(
    path: &Path,
    report: &AiAnalysisReport,
) -> anyhow::Result<String> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    write_csv_row(
        &mut writer,
        [
            "mode",
            "action_taken",
            "source",
            "category",
            "risk",
            "confidence",
            "protected",
            "analysis_recommendation",
            "analysis_reason",
            "audit_status",
            "audit_reason",
            "final_recommendation",
            "delete_list_candidate",
            "path",
            "size_bytes",
            "size_display",
            "provider",
            "model",
        ],
    )?;

    for finding in &report.findings {
        write_ai_finding_csv_row(&mut writer, report, finding)?;
    }

    if !report.errors.is_empty() {
        write_csv_row(&mut writer, ["errors"])?;
        for error in &report.errors {
            write_csv_row(&mut writer, [error.as_str()])?;
        }
    }

    writer.flush()?;

    Ok(format!(
        "ai-review/report-only，候选 {} 个，待删清单 {} 个，需人工复核 {} 个，受保护 {} 个，错误 {} 条",
        format::count(report.candidate_count),
        format::count(report.delete_candidate_count),
        format::count(report.needs_review_count),
        format::count(report.protected_count),
        format::count(report.error_count)
    ))
}

fn export_ai_delete_list_to_path(
    path: &Path,
    report: &AiAnalysisReport,
) -> anyhow::Result<String> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);

    write_csv_row(
        &mut writer,
        [
            "mode",
            "action_taken",
            "source",
            "category",
            "risk",
            "confidence",
            "audit_status",
            "final_recommendation",
            "path",
            "size_bytes",
            "size_display",
            "audit_reason",
        ],
    )?;

    let mut exported = 0_u64;
    let mut exported_size = 0_u64;
    for finding in report
        .findings
        .iter()
        .filter(|finding| finding.is_delete_list_candidate())
    {
        write_csv_row(
            &mut writer,
            [
                "ai-review".to_owned(),
                "none".to_owned(),
                finding.source.as_str().to_owned(),
                finding.category.as_str().to_owned(),
                finding.risk.as_str().to_owned(),
                format!("{:.2}", finding.confidence),
                finding.audit_status.as_str().to_owned(),
                finding.final_recommendation.as_str().to_owned(),
                path_text(&finding.path),
                finding.size.to_string(),
                format::bytes(finding.size),
                finding.audit_reason.clone(),
            ],
        )?;
        exported += 1;
        exported_size = exported_size.saturating_add(finding.size);
    }

    writer.flush()?;

    Ok(format!(
        "ai-review/report-only，导出待删候选 {} 个，合计 {}；action_taken=none，请人工确认后自行处理",
        format::count(exported),
        format::bytes(exported_size)
    ))
}

fn write_ai_finding_csv_row(
    writer: &mut impl Write,
    report: &AiAnalysisReport,
    finding: &AiReviewFinding,
) -> anyhow::Result<()> {
    write_csv_row(
        writer,
        [
            "ai-review".to_owned(),
            "none".to_owned(),
            finding.source.as_str().to_owned(),
            finding.category.as_str().to_owned(),
            finding.risk.as_str().to_owned(),
            format!("{:.2}", finding.confidence),
            finding.protected.to_string(),
            finding.analysis_recommendation.as_str().to_owned(),
            finding.analysis_reason.clone(),
            finding.audit_status.as_str().to_owned(),
            finding.audit_reason.clone(),
            finding.final_recommendation.as_str().to_owned(),
            finding.is_delete_list_candidate().to_string(),
            path_text(&finding.path),
            finding.size.to_string(),
            format::bytes(finding.size),
            report.provider_label.clone(),
            report.model.clone(),
        ],
    )
}

fn cleanup_preview_table(
    ui: &mut egui::Ui,
    preview: Option<&CleanupPreview>,
    search_query: &str,
    sort: &mut SortState<CleanupSortKey>,
    status_message: &mut String,
) {
    let Some(preview) = preview else {
        ui.label("点击\"生成清理预览\"后，这里会显示 dry-run 候选。当前版本不会删除任何文件。");
        return;
    };

    ui.label(
        RichText::new(format!(
            "Dry-run 预览：根目录 {}，候选总大小 {}，预计可清理 {}，受保护 {} 个 / {}。总候选 {} 个，访问错误 {} 个。当前版本不提供执行清理功能。",
            preview.root.display(),
            format::bytes(preview.total_candidate_size),
            format::bytes(preview.reclaimable_size),
            format::count(preview.protected_count),
            format::bytes(preview.protected_size),
            format::count(preview.candidate_count),
            format::count(preview.error_count)
        ))
        .small()
        .strong(),
    );
    ui.label(
        RichText::new("受保护候选会显示在列表中，但不会计入预计可清理空间。")
            .small()
            .weak(),
    );
    if !preview.errors.is_empty() {
        ui.label(
            RichText::new(format!(
                "预览过程中记录了 {} 条访问错误，当前保留 {} 条；可在导出报告前先确认扫描权限。",
                format::count(preview.error_count),
                format::count(preview.errors.len() as u64)
            ))
            .small()
            .weak(),
        );
    }

    let query = normalized_query(search_query);
    let mut candidates: Vec<_> = preview
        .candidates
        .iter()
        .filter(|candidate| cleanup_candidate_matches(candidate, &query))
        .collect();
    candidates.sort_by(|left, right| compare_cleanup_candidates(left, right, *sort));

    result_count_label(
        ui,
        candidates.len().min(120),
        candidates.len(),
        preview.candidates.len(),
        "候选",
    );

    if candidates.is_empty() {
        ui.label("没有匹配的清理预览候选。 ");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            egui::Grid::new("cleanup_preview_table")
                .striped(true)
                .num_columns(8)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(ui, "规则", CleanupSortKey::Rule, SortDirection::Asc, sort);
                    sortable_header(ui, "风险", CleanupSortKey::Risk, SortDirection::Desc, sort);
                    sortable_header(
                        ui,
                        "保护",
                        CleanupSortKey::Protected,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(ui, "大小", CleanupSortKey::Size, SortDirection::Desc, sort);
                    plain_header(ui, "类型");
                    sortable_header(ui, "路径", CleanupSortKey::Path, SortDirection::Asc, sort);
                    plain_header(ui, "原因");
                    plain_header(ui, "操作");
                    ui.end_row();

                    for candidate in candidates.into_iter().take(120) {
                        cleanup_candidate_row(ui, candidate, status_message);
                    }
                });
        });
}

fn cleanup_candidate_matches(candidate: &CleanupCandidate, query: &str) -> bool {
    query.is_empty()
        || text_matches(candidate.rule_label, query)
        || text_matches(candidate.risk.label(), query)
        || text_matches(candidate.kind.label(), query)
        || text_matches(&candidate.path.display().to_string(), query)
        || text_matches(&candidate.reason, query)
}

fn cleanup_candidate_row(
    ui: &mut egui::Ui,
    candidate: &CleanupCandidate,
    status_message: &mut String,
) {
    ui.label(candidate.rule_label);
    ui.label(candidate.risk.label());
    ui.label(if candidate.protected {
        RichText::new("受保护").strong()
    } else {
        RichText::new("可估算")
    });
    ui.label(format::bytes(candidate.size));
    ui.label(candidate.kind.label());

    let path_text = candidate.path.display().to_string();
    let open_target = candidate.path.parent().unwrap_or(candidate.path.as_path());
    let path_response = ui
        .label(compact_text(&path_text, 72))
        .on_hover_text(path_text.clone());
    path_context_menu(
        path_response,
        &candidate.path,
        open_target,
        &file_name(&candidate.path),
        status_message,
    );

    ui.label(compact_text(&candidate.reason, 42))
        .on_hover_text(candidate.reason.clone());
    path_actions(ui, &candidate.path, open_target, status_message);
    ui.end_row();
}

fn duplicate_preview_table(
    ui: &mut egui::Ui,
    preview: Option<&DuplicatePreview>,
    search_query: &str,
    sort: &mut SortState<DuplicateSortKey>,
    status_message: &mut String,
) {
    let Some(preview) = preview else {
        ui.label(
            "点击\"查找重复文件\"后，这里会显示 dry-run 重复文件组。当前版本不会删除任何文件。",
        );
        return;
    };

    ui.label(
        RichText::new(format!(
            "Dry-run 重复文件预览：根目录 {}，最小候选 {}，已扫描 {} 个文件，已哈希 {} 个候选。发现 {} 个重复组、{} 个重复副本，重复副本总大小 {}；预计可回收 {}，受保护 {}，错误 {} 条。",
            preview.root.display(),
            format::bytes(preview.min_size),
            format::count(preview.scanned_file_count),
            format::count(preview.hashed_file_count),
            format::count(preview.duplicate_group_count),
            format::count(preview.duplicate_file_count),
            format::bytes(preview.duplicate_size),
            format::bytes(preview.reclaimable_size),
            format::bytes(preview.protected_size),
            format::count(preview.error_count)
        ))
        .small()
        .strong(),
    );
    ui.label(
        RichText::new("每组默认保留修改时间最早、路径排序最靠前的一个文件；其它文件仅作为 dry-run 重复副本显示。受保护重复副本不计入预计可回收空间。")
            .small()
            .weak(),
    );
    if !preview.errors.is_empty() {
        ui.label(
            RichText::new(format!(
                "重复检测过程中记录了 {} 条访问错误，当前保留 {} 条。",
                format::count(preview.error_count),
                format::count(preview.errors.len() as u64)
            ))
            .small()
            .weak(),
        );
    }

    let query = normalized_query(search_query);
    let mut groups: Vec<_> = preview
        .groups
        .iter()
        .filter(|group| duplicate_group_matches(group, &query))
        .collect();
    groups.sort_by(|left, right| compare_duplicate_groups(left, right, *sort));

    result_count_label(
        ui,
        groups.len().min(120),
        groups.len(),
        preview.groups.len(),
        "重复组",
    );

    if groups.is_empty() {
        ui.label("没有匹配的重复文件组。 ");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            egui::Grid::new("duplicate_preview_table")
                .striped(true)
                .num_columns(8)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(
                        ui,
                        "大小",
                        DuplicateSortKey::Size,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "副本",
                        DuplicateSortKey::Count,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "可回收",
                        DuplicateSortKey::Reclaimable,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "保护",
                        DuplicateSortKey::Protected,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "保留路径",
                        DuplicateSortKey::KeepPath,
                        SortDirection::Asc,
                        sort,
                    );
                    plain_header(ui, "哈希");
                    plain_header(ui, "组内文件");
                    plain_header(ui, "操作");
                    ui.end_row();

                    for group in groups.into_iter().take(120) {
                        duplicate_group_row(ui, group, status_message);
                    }
                });
        });
}

fn duplicate_group_matches(group: &DuplicateGroup, query: &str) -> bool {
    query.is_empty()
        || text_matches(&group.hash, query)
        || text_matches(&group.keep_path.display().to_string(), query)
        || group
            .files
            .iter()
            .any(|file| text_matches(&file.path.display().to_string(), query))
}

fn duplicate_group_row(ui: &mut egui::Ui, group: &DuplicateGroup, status_message: &mut String) {
    ui.label(format::bytes(group.size));
    ui.label(format::count(group.duplicate_count));
    ui.label(format::bytes(group.reclaimable_size));
    ui.label(if group.protected_size > 0 {
        RichText::new(format::bytes(group.protected_size)).strong()
    } else {
        RichText::new("-")
    });

    let keep_path_text = group.keep_path.display().to_string();
    let keep_open_target = group
        .keep_path
        .parent()
        .unwrap_or(group.keep_path.as_path());
    let keep_response = ui
        .label(compact_text(&keep_path_text, 58))
        .on_hover_text(keep_path_text.clone());
    path_context_menu(
        keep_response,
        &group.keep_path,
        keep_open_target,
        &file_name(&group.keep_path),
        status_message,
    );

    ui.label(compact_text(&group.hash, 12))
        .on_hover_text(group.hash.clone());

    ui.vertical(|ui| {
        for file in &group.files {
            duplicate_file_line(ui, file, status_message);
        }
    });

    path_actions(ui, &group.keep_path, keep_open_target, status_message);
    ui.end_row();
}

fn duplicate_file_line(ui: &mut egui::Ui, file: &DuplicateFile, status_message: &mut String) {
    let role = if file.keep { "保留" } else { "重复" };
    let protection = if file.protected { " / 受保护" } else { "" };
    let text = format!("{}{} · {}", role, protection, file.path.display());
    let open_target = file.path.parent().unwrap_or(file.path.as_path());
    let response = ui.label(compact_text(&text, 84)).on_hover_text(format!(
        "{}\n{}",
        text,
        format::bytes(file.size)
    ));
    path_context_menu(
        response,
        &file.path,
        open_target,
        &file_name(&file.path),
        status_message,
    );
}

fn ai_review_table(
    ui: &mut egui::Ui,
    report: Option<&AiAnalysisReport>,
    search_query: &str,
    sort: &mut SortState<AiSortKey>,
    status_message: &mut String,
) {
    let Some(report) = report else {
        ui.label("生成清理预览或重复文件预览后，点击\"AI 分析审核\"。当前版本只生成报告和待删清单，不执行删除。 ");
        return;
    };

    ui.label(
        RichText::new(format!(
            "AI 审核报告：根目录 {}，Provider {}，模型 {}。候选 {} 个，待删清单候选 {} 个，需人工复核 {} 个，拒绝/保留 {} 个，受保护 {} 个，错误 {} 条。",
            report.root.display(),
            report.provider_label,
            report.model,
            format::count(report.candidate_count),
            format::count(report.delete_candidate_count),
            format::count(report.needs_review_count),
            format::count(report.rejected_count),
            format::count(report.protected_count),
            format::count(report.error_count)
        ))
        .small()
        .strong(),
    );
    ui.label(
        RichText::new("待删清单只包含：审核通过/纠正为可删、非受保护、风险不是高/未知、且不需要人工复核的项目；导出仍为 action_taken=none。")
            .small()
            .weak(),
    );

    if !report.errors.is_empty() {
        ui.label(
            RichText::new(format!(
                "AI 流程记录了 {} 条错误；失败或缺失结果会保守标记为需人工复核。",
                format::count(report.error_count)
            ))
            .small()
            .weak(),
        );
    }

    let query = normalized_query(search_query);
    let mut findings: Vec<_> = report
        .findings
        .iter()
        .filter(|finding| ai_finding_matches(finding, &query))
        .collect();
    findings.sort_by(|left, right| compare_ai_findings(left, right, *sort));

    result_count_label(
        ui,
        findings.len().min(120),
        findings.len(),
        report.findings.len(),
        "AI 条目",
    );

    if findings.is_empty() {
        ui.label("没有匹配的 AI 审核条目。 ");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(360.0)
        .show(ui, |ui| {
            egui::Grid::new("ai_review_table")
                .striped(true)
                .num_columns(10)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(
                        ui,
                        "最终建议",
                        AiSortKey::FinalRecommendation,
                        SortDirection::Asc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "审核",
                        AiSortKey::AuditStatus,
                        SortDirection::Asc,
                        sort,
                    );
                    sortable_header(ui, "风险", AiSortKey::Risk, SortDirection::Desc, sort);
                    sortable_header(
                        ui,
                        "置信度",
                        AiSortKey::Confidence,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(ui, "分类", AiSortKey::Category, SortDirection::Asc, sort);
                    sortable_header(ui, "来源", AiSortKey::Source, SortDirection::Asc, sort);
                    sortable_header(ui, "大小", AiSortKey::Size, SortDirection::Desc, sort);
                    sortable_header(
                        ui,
                        "保护",
                        AiSortKey::Protected,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(ui, "路径", AiSortKey::Path, SortDirection::Asc, sort);
                    plain_header(ui, "理由/操作");
                    ui.end_row();

                    for finding in findings.into_iter().take(120) {
                        ai_finding_row(ui, finding, status_message);
                    }
                });
        });
}

fn ai_finding_matches(finding: &AiReviewFinding, query: &str) -> bool {
    query.is_empty()
        || text_matches(finding.final_recommendation.label(), query)
        || text_matches(finding.audit_status.label(), query)
        || text_matches(finding.risk.label(), query)
        || text_matches(finding.category.label(), query)
        || text_matches(finding.source.label(), query)
        || text_matches(&finding.display_path, query)
        || text_matches(&finding.analysis_reason, query)
        || text_matches(&finding.audit_reason, query)
}

fn ai_finding_row(ui: &mut egui::Ui, finding: &AiReviewFinding, status_message: &mut String) {
    let recommendation = if finding.is_delete_list_candidate() {
        RichText::new(finding.final_recommendation.label()).strong()
    } else {
        RichText::new(finding.final_recommendation.label())
    };
    ui.label(recommendation);
    ui.label(finding.audit_status.label());
    ui.label(match finding.risk {
        AiCleanupRisk::High | AiCleanupRisk::Unknown => RichText::new(finding.risk.label()).strong(),
        _ => RichText::new(finding.risk.label()),
    });
    ui.label(format!("{:.0}%", finding.confidence * 100.0));
    ui.label(finding.category.label());
    ui.label(finding.source.label());
    ui.label(format::bytes(finding.size));
    ui.label(if finding.protected {
        RichText::new("受保护").strong()
    } else {
        RichText::new("-")
    });

    let path_text = finding.path.display().to_string();
    let open_target = finding.path.parent().unwrap_or(finding.path.as_path());
    let path_response = ui
        .label(compact_text(&path_text, 64))
        .on_hover_text(path_text.clone());
    path_context_menu(
        path_response,
        &finding.path,
        open_target,
        &file_name(&finding.path),
        status_message,
    );

    ui.vertical(|ui| {
        ui.label(compact_text(&finding.audit_reason, 48))
            .on_hover_text(format!(
                "审核理由：{}\n分析建议：{}\n分析理由：{}",
                finding.audit_reason,
                finding.analysis_recommendation.label(),
                finding.analysis_reason
            ));
        path_actions(ui, &finding.path, open_target, status_message);
    });
    ui.end_row();
}

fn extension_table(
    ui: &mut egui::Ui,
    stats: &ScanStats,
    search_query: &str,
    sort: &mut SortState<ExtensionSortKey>,
) {
    let query = normalized_query(search_query);
    let mut extensions: Vec<_> = stats
        .extensions
        .iter()
        .filter(|extension| extension_matches(extension, &query))
        .collect();
    extensions.sort_by(|left, right| compare_extensions(left, right, *sort));

    result_count_label(
        ui,
        extensions.len().min(120),
        extensions.len(),
        stats.extensions.len(),
        "类型",
    );

    if extensions.is_empty() {
        ui.label("没有匹配的文件类型。");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            egui::Grid::new("extension_table")
                .striped(true)
                .num_columns(4)
                .spacing([12.0, 4.0])
                .show(ui, |ui| {
                    sortable_header(
                        ui,
                        "类型",
                        ExtensionSortKey::Extension,
                        SortDirection::Asc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "大小",
                        ExtensionSortKey::Size,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "占比",
                        ExtensionSortKey::Percent,
                        SortDirection::Desc,
                        sort,
                    );
                    sortable_header(
                        ui,
                        "文件数",
                        ExtensionSortKey::FileCount,
                        SortDirection::Desc,
                        sort,
                    );
                    ui.end_row();
                    for extension in extensions.into_iter().take(120) {
                        extension_row(ui, extension, stats.total_size);
                    }
                });
        });
}

fn extension_matches(extension: &ExtensionRecord, query: &str) -> bool {
    query.is_empty() || text_matches(&extension.extension, query)
}

fn extension_row(ui: &mut egui::Ui, extension: &ExtensionRecord, total_size: u64) {
    ui.label(&extension.extension);
    ui.label(format::bytes(extension.total_size));
    ui.label(format::percent(extension.total_size, total_size));
    ui.label(format::count(extension.file_count));
    ui.end_row();
}

fn error_list(ui: &mut egui::Ui, stats: &ScanStats, search_query: &str) {
    if stats.errors.is_empty() {
        ui.label("没有记录到访问错误。");
        return;
    }

    let query = normalized_query(search_query);
    let errors: Vec<_> = stats
        .errors
        .iter()
        .filter(|error| query.is_empty() || text_matches(error, &query))
        .collect();

    ui.label(format!(
        "共 {} 个访问错误，当前保留 {} 条，匹配 {} 条。",
        format::count(stats.error_count),
        format::count(stats.errors.len() as u64),
        format::count(errors.len() as u64),
    ));

    if errors.is_empty() {
        ui.label("没有匹配的错误记录。");
        return;
    }

    egui::ScrollArea::vertical()
        .max_height(320.0)
        .show(ui, |ui| {
            for error in errors {
                ui.label(error);
            }
        });
}

fn result_count_label(
    ui: &mut egui::Ui,
    visible: usize,
    matched: usize,
    retained: usize,
    unit: &str,
) {
    ui.label(
        RichText::new(format!(
            "显示 {} / 匹配 {} / 当前保留 {} 个{}。",
            format::count(visible as u64),
            format::count(matched as u64),
            format::count(retained as u64),
            unit
        ))
        .small()
        .weak(),
    );
}

fn sortable_header<K>(
    ui: &mut egui::Ui,
    label: &str,
    key: K,
    default_direction: SortDirection,
    state: &mut SortState<K>,
) where
    K: Copy + PartialEq,
{
    let selected = state.key == key;
    let text = if selected {
        format!("{} {}", label, state.direction.arrow())
    } else {
        label.to_owned()
    };

    if ui.button(RichText::new(text).strong()).clicked() {
        state.select(key, default_direction);
    }
}

fn plain_header(ui: &mut egui::Ui, label: &str) {
    ui.label(RichText::new(label).strong());
}

fn compare_directories(
    left: &DirectoryRecord,
    right: &DirectoryRecord,
    sort: SortState<DirectorySortKey>,
) -> Ordering {
    let primary = match sort.key {
        DirectorySortKey::Name => compare_text(&left.name(), &right.name()),
        DirectorySortKey::Size | DirectorySortKey::Percent => {
            left.total_size.cmp(&right.total_size)
        }
        DirectorySortKey::Files => left.descendant_file_count.cmp(&right.descendant_file_count),
        DirectorySortKey::Path => left.path.cmp(&right.path),
    };

    apply_direction(primary, sort.direction).then_with(|| left.path.cmp(&right.path))
}

fn compare_files(left: &FileRecord, right: &FileRecord, sort: SortState<FileSortKey>) -> Ordering {
    let primary = match sort.key {
        FileSortKey::Name => compare_text(&file_name(&left.path), &file_name(&right.path)),
        FileSortKey::Size => left.size.cmp(&right.size),
        FileSortKey::Extension => compare_text(&left.extension, &right.extension),
        FileSortKey::Path => left.path.cmp(&right.path),
    };

    apply_direction(primary, sort.direction).then_with(|| left.path.cmp(&right.path))
}

fn compare_extensions(
    left: &ExtensionRecord,
    right: &ExtensionRecord,
    sort: SortState<ExtensionSortKey>,
) -> Ordering {
    let primary = match sort.key {
        ExtensionSortKey::Extension => compare_text(&left.extension, &right.extension),
        ExtensionSortKey::Size | ExtensionSortKey::Percent => {
            left.total_size.cmp(&right.total_size)
        }
        ExtensionSortKey::FileCount => left.file_count.cmp(&right.file_count),
    };

    apply_direction(primary, sort.direction).then_with(|| left.extension.cmp(&right.extension))
}

fn compare_cleanup_candidates(
    left: &CleanupCandidate,
    right: &CleanupCandidate,
    sort: SortState<CleanupSortKey>,
) -> Ordering {
    let primary = match sort.key {
        CleanupSortKey::Rule => compare_text(left.rule_label, right.rule_label),
        CleanupSortKey::Risk => left.risk.rank().cmp(&right.risk.rank()),
        CleanupSortKey::Protected => left.protected.cmp(&right.protected),
        CleanupSortKey::Size => left.size.cmp(&right.size),
        CleanupSortKey::Path => left.path.cmp(&right.path),
    };

    apply_direction(primary, sort.direction).then_with(|| left.path.cmp(&right.path))
}

fn compare_duplicate_groups(
    left: &DuplicateGroup,
    right: &DuplicateGroup,
    sort: SortState<DuplicateSortKey>,
) -> Ordering {
    let primary = match sort.key {
        DuplicateSortKey::Size => left.size.cmp(&right.size),
        DuplicateSortKey::Count => left.duplicate_count.cmp(&right.duplicate_count),
        DuplicateSortKey::Reclaimable => left.reclaimable_size.cmp(&right.reclaimable_size),
        DuplicateSortKey::Protected => left.protected_size.cmp(&right.protected_size),
        DuplicateSortKey::KeepPath => left.keep_path.cmp(&right.keep_path),
    };

    apply_direction(primary, sort.direction).then_with(|| left.keep_path.cmp(&right.keep_path))
}

fn compare_ai_findings(
    left: &AiReviewFinding,
    right: &AiReviewFinding,
    sort: SortState<AiSortKey>,
) -> Ordering {
    let primary = match sort.key {
        AiSortKey::FinalRecommendation => left
            .final_recommendation
            .rank()
            .cmp(&right.final_recommendation.rank()),
        AiSortKey::AuditStatus => left.audit_status.rank().cmp(&right.audit_status.rank()),
        AiSortKey::Risk => left.risk.rank().cmp(&right.risk.rank()),
        AiSortKey::Confidence => left
            .confidence
            .partial_cmp(&right.confidence)
            .unwrap_or(Ordering::Equal),
        AiSortKey::Category => left.category.rank().cmp(&right.category.rank()),
        AiSortKey::Source => compare_text(left.source.label(), right.source.label()),
        AiSortKey::Size => left.size.cmp(&right.size),
        AiSortKey::Protected => left.protected.cmp(&right.protected),
        AiSortKey::Path => left.path.cmp(&right.path),
    };

    apply_direction(primary, sort.direction).then_with(|| left.path.cmp(&right.path))
}

fn apply_direction(ordering: Ordering, direction: SortDirection) -> Ordering {
    match direction {
        SortDirection::Asc => ordering,
        SortDirection::Desc => ordering.reverse(),
    }
}

fn normalized_query(query: &str) -> String {
    query.trim().to_ascii_lowercase()
}

fn text_matches(text: &str, query: &str) -> bool {
    text.to_ascii_lowercase().contains(query)
}

fn compare_text(left: &str, right: &str) -> Ordering {
    left.to_ascii_lowercase()
        .cmp(&right.to_ascii_lowercase())
        .then_with(|| left.cmp(right))
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("[未知文件名]")
        .to_owned()
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let chars: Vec<_> = text.chars().collect();
    if chars.len() <= max_chars {
        return text.to_owned();
    }

    if max_chars <= 1 {
        return "…".to_owned();
    }

    let keep_start = (max_chars / 3).max(1);
    let keep_end = max_chars.saturating_sub(keep_start + 1).max(1);
    let start: String = chars.iter().take(keep_start).collect();
    let end: String = chars
        .iter()
        .skip(chars.len().saturating_sub(keep_end))
        .collect();
    format!("{}…{}", start, end)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn treemap_items_with_other_aggregates_hidden_items() {
        let dir = PathBuf::from("C:\\root");
        let mut items = Vec::new();
        for index in 0..5 {
            items.push(TreemapItem::directory(
                format!("dir{}", index),
                dir.join(format!("dir{}", index)),
                10 - index as u64,
            ));
        }

        let aggregated = treemap_items_with_other(items, dir, 3);
        assert_eq!(aggregated.len(), 3);
        assert_eq!(aggregated[0].label, "dir0");
        assert_eq!(aggregated[1].label, "dir1");
        assert_eq!(aggregated[2].size, 8 + 7 + 6);
        assert!(matches!(
            aggregated[2].kind,
            crate::treemap::TreemapItemKind::Other { item_count: 3, .. }
        ));
    }
}
