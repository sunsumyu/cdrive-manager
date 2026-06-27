use std::path::PathBuf;

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui, Vec2};

use crate::{format, model::DirectoryRecord};

#[derive(Debug, Clone)]
pub struct TreemapItem {
    pub label: String,
    pub path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Clone)]
pub enum TreemapAction {
    Enter(PathBuf),
    CopyPath(PathBuf),
    OpenLocation(PathBuf),
}

impl From<&DirectoryRecord> for TreemapItem {
    fn from(value: &DirectoryRecord) -> Self {
        Self {
            label: value.name(),
            path: value.path.clone(),
            size: value.total_size,
        }
    }
}

pub fn draw_treemap(
    ui: &mut Ui,
    items: &[TreemapItem],
    total_size: u64,
    empty_message: &str,
) -> Option<TreemapAction> {
    let available = ui.available_size_before_wrap();
    let desired_size = Vec2::new(available.x.max(280.0), available.y.max(260.0));
    let (rect, _) = ui.allocate_exact_size(desired_size, Sense::hover());
    let painter = ui.painter_at(rect);

    painter.rect_filled(rect, 8.0, Color32::from_rgb(24, 27, 34));

    if items.is_empty() || total_size == 0 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            empty_message,
            FontId::proportional(16.0),
            Color32::from_gray(170),
        );
        return None;
    }

    let display_items: Vec<_> = items
        .iter()
        .filter(|item| item.size > 0)
        .take(36)
        .cloned()
        .collect();

    if display_items.is_empty() {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            empty_message,
            FontId::proportional(16.0),
            Color32::from_gray(170),
        );
        return None;
    }

    let mut action = None;
    let rects = layout_slice(rect.shrink(6.0), &display_items);
    for (index, (item, item_rect)) in display_items.iter().zip(rects).enumerate() {
        if item_rect.width() <= 1.0 || item_rect.height() <= 1.0 {
            continue;
        }

        let color = palette(index);
        painter.rect_filled(item_rect, 4.0, color);
        painter.rect_stroke(
            item_rect,
            4.0,
            Stroke::new(1.0, Color32::from_black_alpha(80)),
            egui::StrokeKind::Inside,
        );

        let path_text = item.path.display().to_string();
        let response = ui
            .interact(
                item_rect,
                ui.make_persistent_id(("treemap", index, &item.path)),
                Sense::click(),
            )
            .on_hover_text(format!(
                "{}\n{}\n{} ({})\n左键进入目录，右键查看更多操作",
                item.label,
                path_text,
                format::bytes(item.size),
                format::percent(item.size, total_size)
            ));

        if response.clicked() {
            action = Some(TreemapAction::Enter(item.path.clone()));
        }

        response.context_menu(|ui| {
            if ui.button("进入目录").clicked() {
                action = Some(TreemapAction::Enter(item.path.clone()));
                ui.close();
            }

            if ui.button("复制路径").clicked() {
                action = Some(TreemapAction::CopyPath(item.path.clone()));
                ui.close();
            }

            ui.separator();

            if ui.button("打开位置").clicked() {
                action = Some(TreemapAction::OpenLocation(item.path.clone()));
                ui.close();
            }
        });

        if item_rect.width() > 72.0 && item_rect.height() > 42.0 {
            let label = compact_label(&item.label, item_rect.width());
            painter.text(
                item_rect.left_top() + Vec2::new(6.0, 6.0),
                Align2::LEFT_TOP,
                label,
                FontId::proportional(13.0),
                Color32::WHITE,
            );
            painter.text(
                item_rect.left_bottom() + Vec2::new(6.0, -6.0),
                Align2::LEFT_BOTTOM,
                format::bytes(item.size),
                FontId::monospace(12.0),
                Color32::from_gray(245),
            );
        }
    }

    action
}

fn layout_slice(rect: Rect, items: &[TreemapItem]) -> Vec<Rect> {
    let mut output = Vec::with_capacity(items.len());
    layout_recursive(rect, items, &mut output);
    output
}

fn layout_recursive(rect: Rect, items: &[TreemapItem], output: &mut Vec<Rect>) {
    if items.is_empty() {
        return;
    }

    if items.len() == 1 {
        output.push(rect);
        return;
    }

    let total: u64 = items.iter().map(|item| item.size).sum();
    if total == 0 {
        output.push(rect);
        return;
    }

    let half = total as f64 / 2.0;
    let mut split_index = 0;
    let mut running = 0_u64;

    for (index, item) in items.iter().enumerate() {
        if index > 0 && running as f64 >= half {
            break;
        }
        running = running.saturating_add(item.size);
        split_index = index + 1;
    }

    split_index = split_index.clamp(1, items.len() - 1);
    let first_size: u64 = items[..split_index].iter().map(|item| item.size).sum();
    let ratio = first_size as f32 / total as f32;

    if rect.width() >= rect.height() {
        let split_x = rect.left() + rect.width() * ratio;
        let left = Rect::from_min_max(rect.min, Pos2::new(split_x, rect.max.y)).shrink(1.5);
        let right = Rect::from_min_max(Pos2::new(split_x, rect.min.y), rect.max).shrink(1.5);
        layout_recursive(left, &items[..split_index], output);
        layout_recursive(right, &items[split_index..], output);
    } else {
        let split_y = rect.top() + rect.height() * ratio;
        let top = Rect::from_min_max(rect.min, Pos2::new(rect.max.x, split_y)).shrink(1.5);
        let bottom = Rect::from_min_max(Pos2::new(rect.min.x, split_y), rect.max).shrink(1.5);
        layout_recursive(top, &items[..split_index], output);
        layout_recursive(bottom, &items[split_index..], output);
    }
}

fn compact_label(label: &str, width: f32) -> String {
    let max_chars = (width / 8.0).max(4.0) as usize;
    if label.chars().count() <= max_chars {
        return label.to_owned();
    }

    let mut output: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    output.push('…');
    output
}

fn palette(index: usize) -> Color32 {
    const COLORS: [Color32; 12] = [
        Color32::from_rgb(68, 138, 255),
        Color32::from_rgb(0, 188, 212),
        Color32::from_rgb(76, 175, 80),
        Color32::from_rgb(255, 193, 7),
        Color32::from_rgb(255, 112, 67),
        Color32::from_rgb(171, 71, 188),
        Color32::from_rgb(38, 166, 154),
        Color32::from_rgb(156, 204, 101),
        Color32::from_rgb(92, 107, 192),
        Color32::from_rgb(236, 64, 122),
        Color32::from_rgb(141, 110, 99),
        Color32::from_rgb(120, 144, 156),
    ];

    COLORS[index % COLORS.len()]
}
