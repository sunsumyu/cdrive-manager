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
    cleanup::{
        CleanupCandidate, CleanupPreview, CleanupPreviewEvent, CleanupPreviewFinished,
        CleanupPreviewHandle, CleanupPreviewOptions, CleanupPreviewProgress, spawn_cleanup_preview,
    },
    format,
    model::{DirectoryRecord, DirectoryTree, ExtensionRecord, FileRecord, ScanStats},
    scanner::{ScanEvent, ScanFinished, ScanHandle, ScanOptions, ScanProgress, spawn_scan},
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
    status_message: String,
    selected_tab: ResultTab,
    search_query: String,
    directory_sort: SortState<DirectorySortKey>,
    file_sort: SortState<FileSortKey>,
    extension_sort: SortState<ExtensionSortKey>,
    cleanup_sort: SortState<CleanupSortKey>,
    treemap_current_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResultTab {
    Directories,
    Files,
    Types,
    CleanupPreview,
    Errors,
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
            status_message: "准备扫描。第一版只分析空间占用，不删除任何文件。".to_owned(),
            selected_tab: ResultTab::Directories,
            search_query: String::new(),
            directory_sort: SortState::new(DirectorySortKey::Size, SortDirection::Desc),
            file_sort: SortState::new(FileSortKey::Size, SortDirection::Desc),
            extension_sort: SortState::new(ExtensionSortKey::Size, SortDirection::Desc),
            cleanup_sort: SortState::new(CleanupSortKey::Size, SortDirection::Desc),
            treemap_current_dir: None,
        }
    }

    fn start_scan(&mut self) {
        let Some(root) = self.validate_root_input() else {
            return;
        };

        self.scan_handle = Some(spawn_scan(ScanOptions { root: root.clone() }));
        self.scan_in_progress = true;
        self.cancel_requested = false;
        self.progress = None;
        self.stats = None;
        self.cleanup_preview = None;
        self.cleanup_progress = None;
        self.treemap_current_dir = None;
        self.status_message = format!("正在后台扫描：{}", root.display());
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

        let root = stats.root.clone();
        self.cleanup_handle = Some(spawn_cleanup_preview(CleanupPreviewOptions {
            root: root.clone(),
        }));
        self.cleanup_in_progress = true;
        self.cleanup_cancel_requested = false;
        self.cleanup_progress = None;
        self.cleanup_preview = None;
        self.selected_tab = ResultTab::CleanupPreview;
        self.status_message = format!(
            "正在生成 dry-run 清理预览：{}。不会删除、移动或修改任何文件。",
            root.display()
        );
    }

    fn cancel_cleanup_preview(&mut self) {
        if let Some(handle) = &self.cleanup_handle {
            handle.cancel();
            self.cleanup_cancel_requested = true;
            self.status_message = "正在取消清理预览，已发现的候选会保留……".to_owned();
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
                self.treemap_current_dir = None;
                self.status_message = format!("已打开扫描结果：{}", path.display());
            }
            Err(error) => {
                self.status_message = format!("打开扫描结果失败：{} ({:#})", path.display(), error);
            }
        }
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

    fn apply_progress(&mut self, progress: ScanProgress) {
        self.status_message = if progress.cancelled {
            "扫描已取消，当前显示的是部分结果。".to_owned()
        } else if progress.finished {
            "扫描完成。".to_owned()
        } else if let Some(path) = &progress.current_path {
            format!("正在扫描：{}", path.display())
        } else {
            "正在扫描……".to_owned()
        };
        self.progress = Some(progress);
    }

    fn apply_finished(&mut self, result: ScanFinished) {
        self.status_message = if result.cancelled {
            format!(
                "扫描已取消：已统计 {} 个文件，{} 个目录，部分结果共 {}。",
                format::count(result.stats.file_count),
                format::count(result.stats.dir_count),
                format::bytes(result.stats.total_size)
            )
        } else {
            format!(
                "扫描完成：{} 个文件，{} 个目录，总计 {}。",
                format::count(result.stats.file_count),
                format::count(result.stats.dir_count),
                format::bytes(result.stats.total_size)
            )
        };
        self.stats = Some(result.stats);
        self.scan_in_progress = false;
        self.cancel_requested = false;
        self.scan_handle = None;
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

    fn draw_top_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("top_bar").show(ctx, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.heading("C 盘空间管理器");
                ui.separator();
                ui.label("扫描目录：");
                let input = ui.text_edit_singleline(&mut self.root_input);
                if input.lost_focus()
                    && ui.input(|input| input.key_pressed(egui::Key::Enter))
                    && !self.scan_in_progress
                    && !self.cleanup_in_progress
                {
                    self.start_scan();
                }

                if ui
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress,
                        egui::Button::new("选择目录"),
                    )
                    .clicked()
                {
                    self.choose_directory();
                }

                if ui
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress,
                        egui::Button::new("开始扫描"),
                    )
                    .clicked()
                {
                    self.start_scan();
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
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress,
                        egui::Button::new("打开结果"),
                    )
                    .clicked()
                {
                    self.open_scan_result();
                }

                let has_final_stats = self.stats.is_some();
                if ui
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress && has_final_stats,
                        egui::Button::new("保存结果"),
                    )
                    .clicked()
                {
                    self.save_scan_result();
                }

                if ui
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress && has_final_stats,
                        egui::Button::new("导出 CSV"),
                    )
                    .clicked()
                {
                    self.export_csv_report();
                }

                ui.separator();

                if ui
                    .add_enabled(
                        !self.scan_in_progress && !self.cleanup_in_progress && has_final_stats,
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
                        !self.scan_in_progress
                            && !self.cleanup_in_progress
                            && self.current_cleanup_preview().is_some(),
                        egui::Button::new("导出预览 CSV"),
                    )
                    .clicked()
                {
                    self.export_cleanup_preview_csv();
                }

                if self.scan_in_progress || self.cleanup_in_progress {
                    ui.spinner();
                    let text = if self.scan_in_progress {
                        if self.cancel_requested {
                            "取消扫描中"
                        } else {
                            "扫描中"
                        }
                    } else if self.cleanup_cancel_requested {
                        "取消预览中"
                    } else {
                        "预览中"
                    };
                    ui.label(RichText::new(text).strong());
                }
            });
            ui.label(RichText::new(&self.status_message).small());
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
            tab_button(ui, &mut self.selected_tab, ResultTab::Errors, "错误");
        });
        ui.separator();
        self.draw_search_bar(ui);
        ui.add_space(4.0);

        match self.selected_tab {
            ResultTab::Directories => directory_table(
                ui,
                stats,
                &self.search_query,
                &mut self.directory_sort,
                &mut self.status_message,
            ),
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
            ResultTab::CleanupPreview => cleanup_preview_table(
                ui,
                self.current_cleanup_preview().as_deref(),
                &self.search_query,
                &mut self.cleanup_sort,
                &mut self.status_message,
            ),
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
            RichText::new("搜索和排序只作用于当前保留的 Top 结果，不是全盘全文搜索。")
                .small()
                .weak(),
        );
    }

    fn draw_treemap_panel(&mut self, ui: &mut egui::Ui, stats: &ScanStats) {
        let Some(tree) = stats.directory_tree.as_ref() else {
            ui.heading("空间占用图");
            ui.label(
                RichText::new(
                    "完整目录树会在扫描完成或取消后生成。扫描中仍可查看下方 Top 结果列表。",
                )
                .small()
                .weak(),
            );
            draw_treemap(
                ui,
                &[],
                stats.total_size,
                "完整目录树会在扫描完成或取消后显示",
            );
            return;
        };

        let current_dir = self.treemap_current_dir(stats);
        let current_index = tree
            .node_index_for_path(&current_dir)
            .unwrap_or(tree.root_index);
        let current_node = &tree.nodes[current_index];
        let current_size = current_node.record.total_size;
        let child_count = current_node.children.len();
        let mut treemap_items = treemap_items_for_node(tree, current_index);
        let item_count = treemap_items.len();
        treemap_items.truncate(36);

        ui.horizontal_wrapped(|ui| {
            ui.heading("空间占用图");
            ui.separator();
            if ui
                .add_enabled(current_dir != stats.root, egui::Button::new("返回上级"))
                .clicked()
            {
                self.treemap_current_dir = Some(treemap_parent_dir(stats, &current_dir));
            }
            if ui
                .add_enabled(current_dir != stats.root, egui::Button::new("返回根目录"))
                .clicked()
            {
                self.treemap_current_dir = None;
            }
        });

        ui.label(
            RichText::new(format!("当前目录：{}", current_dir.display()))
                .small()
                .strong(),
        );
        ui.label(
            RichText::new(format!(
                "完整目录树：{} 个直接子目录，直属文件 {} 个 / {}，Treemap 显示前 {} / {} 个块。",
                format::count(child_count as u64),
                format::count(current_node.record.direct_file_count),
                format::bytes(current_node.record.direct_file_size),
                format::count(treemap_items.len() as u64),
                format::count(item_count as u64),
            ))
            .small()
            .weak(),
        );
        ui.label(
            RichText::new("提示：Treemap 基于完整目录树；为保持可读性，矩形图最多显示当前目录下最大的 36 个块。")
                .small()
                .weak(),
        );

        let empty_message = if stats.total_size == 0 {
            "扫描后显示目录空间占用图"
        } else {
            "当前目录没有可显示的子目录或直属文件"
        };

        if let Some(action) = draw_treemap(ui, &treemap_items, current_size, empty_message) {
            self.handle_treemap_action(action, ui.ctx());
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
        self.draw_top_bar(ctx);

        egui::SidePanel::left("summary_panel")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                if let Some(stats) = self.current_stats() {
                    self.draw_summary(ui, stats.as_ref());
                } else {
                    ui.heading("概览");
                    ui.label("点击“开始扫描”后，这里会显示统计信息。");
                    ui.add_space(8.0);
                    ui.label("建议首次扫描小目录测试，再扫描整个 C 盘。  ");
                }
            });

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

            self.draw_treemap_panel(ui, stats.as_ref());

            ui.separator();
            self.draw_tabs(ui, stats.as_ref());
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
) {
    let query = normalized_query(search_query);
    let mut directories: Vec<_> = stats
        .largest_dirs
        .iter()
        .filter(|dir| directory_matches(dir, &query))
        .collect();
    directories.sort_by(|left, right| compare_directories(left, right, *sort));

    result_count_label(
        ui,
        directories.len().min(120),
        directories.len(),
        stats.largest_dirs.len(),
        "目录",
    );

    if directories.is_empty() {
        ui.label("没有匹配的目录。");
        return;
    }

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
                        directory_row(ui, dir, stats.total_size, status_message);
                    }
                });
        });
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
) {
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

    path_actions(ui, &dir.path, &dir.path, status_message);
    ui.end_row();
}

fn file_table(
    ui: &mut egui::Ui,
    stats: &ScanStats,
    search_query: &str,
    sort: &mut SortState<FileSortKey>,
    status_message: &mut String,
) {
    let query = normalized_query(search_query);
    let mut files: Vec<_> = stats
        .largest_files
        .iter()
        .filter(|file| file_matches(file, &query))
        .collect();
    files.sort_by(|left, right| compare_files(left, right, *sort));

    result_count_label(
        ui,
        files.len().min(120),
        files.len(),
        stats.largest_files.len(),
        "文件",
    );

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
    serde_json::to_writer_pretty(writer, stats)?;
    Ok(())
}

fn load_scan_result_from_path(path: &Path) -> anyhow::Result<ScanStats> {
    let file = File::open(path)?;
    let mut stats: ScanStats = serde_json::from_reader(file)?;
    stats.rebuild_indexes();
    Ok(stats)
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

    for file in &stats.largest_files {
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
    Ok(format!(
        "{} {}，{} 个最大文件，{} 个类型，{} 条错误",
        format::count(directory_count as u64),
        directory_scope,
        format::count(stats.largest_files.len() as u64),
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

fn cleanup_preview_table(
    ui: &mut egui::Ui,
    preview: Option<&CleanupPreview>,
    search_query: &str,
    sort: &mut SortState<CleanupSortKey>,
    status_message: &mut String,
) {
    let Some(preview) = preview else {
        ui.label("点击“生成清理预览”后，这里会显示 dry-run 候选。当前版本不会删除任何文件。");
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
