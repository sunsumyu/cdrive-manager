use std::{
    f32::consts::TAU,
    hash::{Hash, Hasher, DefaultHasher},
    path::{Path, PathBuf},
};

use egui::{Align2, Color32, FontId, Pos2, Rect, Sense, Shape, Stroke, Ui, Vec2};

use crate::{
    color_palette::ColorPalette,
    format,
    model::{DirectoryNode, DirectoryTree},
    treemap::TreemapAction,
};

#[derive(Debug, Clone)]
pub struct SunburstSegment {
    label: String,
    path: PathBuf,
    size: u64,
    start_angle: f32,
    end_angle: f32,
    inner_radius: f32,
    outer_radius: f32,
    kind: SunburstSegmentKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SunburstSegmentKind {
    Directory,
    DirectFiles,
    Other,
}

pub fn draw_sunburst(
    ui: &mut Ui,
    tree: &DirectoryTree,
    root_index: usize,
    total_size: u64,
    empty_message: &str,
    color_palette: &ColorPalette,
) -> Option<TreemapAction> {
    let available = ui.available_size_before_wrap();
    let desired_size = Vec2::new(available.x.max(280.0), available.y.max(260.0));
    let (rect, _) = ui.allocate_exact_size(desired_size, Sense::hover());
    let painter = ui.painter_at(rect);
    painter.rect_filled(rect, 8.0, Color32::from_rgb(24, 27, 34));

    if total_size == 0 {
        painter.text(
            rect.center(),
            Align2::CENTER_CENTER,
            empty_message,
            FontId::proportional(16.0),
            Color32::from_gray(170),
        );
        return None;
    }

    let segments = build_sunburst_segments(tree, root_index, rect, 3, 16);
    if segments.is_empty() {
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
    for (index, segment) in segments.iter().enumerate() {
        // Use unified color system
        let color = match segment.kind {
            SunburstSegmentKind::Directory => {
                let path_str = segment.path.to_string_lossy();
                let ext_hint = path_str.rsplit('.').next().unwrap_or("");
                color_palette.color_for_extension(ext_hint)
            },
            SunburstSegmentKind::DirectFiles => Color32::from_rgb(96, 125, 139),
            SunburstSegmentKind::Other => Color32::from_rgb(88, 88, 96),
        };
        
        // Draw with cushion shading effect
        draw_cushion_shaded_segment(&painter, rect.center(), segment, color);

        let response = ui
            .interact(
                rect,
                ui.make_persistent_id((
                    "sunburst",
                    index,
                    &segment.path,
                    segment.start_angle.to_bits(),
                )),
                Sense::click(),
            )
            .on_hover_ui_at_pointer(|ui| {
                if pointer_in_segment(ui, rect.center(), segment) {
                    ui.label(&segment.label);
                    ui.label(segment_kind_label(segment.kind));
                    ui.label(segment.path.display().to_string());
                    ui.label(format!(
                        "{} ({})",
                        format::bytes(segment.size),
                        format::percent(segment.size, total_size)
                    ));
                }
            });

        let pointer_inside = response
            .hover_pos()
            .map(|pos| point_in_segment(pos, rect.center(), segment))
            .unwrap_or(false);

        if pointer_inside && response.clicked() && segment.kind == SunburstSegmentKind::Directory {
            action = Some(TreemapAction::Enter(segment.path.clone()));
        }

        response.context_menu(|ui| {
            let pointer_inside = ui
                .ctx()
                .pointer_hover_pos()
                .map(|pos| point_in_segment(pos, rect.center(), segment))
                .unwrap_or(false);
            if !pointer_inside {
                return;
            }

            if segment.kind == SunburstSegmentKind::Directory && ui.button("进入目录").clicked()
            {
                action = Some(TreemapAction::Enter(segment.path.clone()));
                ui.close();
            }
            if ui.button("复制路径").clicked() {
                action = Some(TreemapAction::CopyPath(segment.path.clone()));
                ui.close();
            }
            ui.separator();
            if ui.button("打开位置").clicked() {
                action = Some(TreemapAction::OpenLocation(segment.path.clone()));
                ui.close();
            }
        });

        if segment.end_angle - segment.start_angle > 0.22
            && segment.outer_radius - segment.inner_radius > 24.0
        {
            let angle = (segment.start_angle + segment.end_angle) / 2.0;
            let radius = (segment.inner_radius + segment.outer_radius) / 2.0;
            let pos = rect.center() + Vec2::angled(angle) * radius;
            painter.text(
                pos,
                Align2::CENTER_CENTER,
                compact_label(&segment.label, 14),
                FontId::proportional(11.0),
                Color32::WHITE,
            );
        }
    }

    let center_radius = (rect.width().min(rect.height()) * 0.09).max(24.0);
    painter.circle_filled(rect.center(), center_radius, Color32::from_rgb(32, 36, 44));
    painter.text(
        rect.center(),
        Align2::CENTER_CENTER,
        "旭日图",
        FontId::proportional(14.0),
        Color32::from_gray(230),
    );

    action
}

pub fn build_sunburst_segments(
    tree: &DirectoryTree,
    root_index: usize,
    rect: Rect,
    max_depth: usize,
    per_node_limit: usize,
) -> Vec<SunburstSegment> {
    let Some(root) = tree.nodes.get(root_index) else {
        return Vec::new();
    };
    if root.record.total_size == 0 || max_depth == 0 {
        return Vec::new();
    }

    let radius = rect.width().min(rect.height()) * 0.46;
    let inner_base = radius * 0.18;
    let ring_width = (radius - inner_base) / max_depth as f32;
    let mut segments = Vec::new();
    append_segments_for_node(
        tree,
        root_index,
        0,
        max_depth,
        per_node_limit,
        0.0,
        TAU,
        inner_base,
        ring_width,
        &mut segments,
    );
    segments
}

#[allow(clippy::too_many_arguments)]
fn append_segments_for_node(
    tree: &DirectoryTree,
    node_index: usize,
    depth: usize,
    max_depth: usize,
    per_node_limit: usize,
    start_angle: f32,
    end_angle: f32,
    inner_base: f32,
    ring_width: f32,
    output: &mut Vec<SunburstSegment>,
) {
    if depth >= max_depth {
        return;
    }

    let node = &tree.nodes[node_index];
    let mut items = sunburst_items_for_node(tree, node, per_node_limit);
    if items.is_empty() {
        return;
    }

    let total: u64 = items.iter().map(|item| item.size).sum();
    if total == 0 {
        return;
    }

    let mut cursor = start_angle;
    let sweep = end_angle - start_angle;
    for item in items.drain(..) {
        let item_sweep = sweep * (item.size as f32 / total as f32);
        let item_start = cursor;
        let item_end = (cursor + item_sweep).min(end_angle);
        cursor = item_end;

        output.push(SunburstSegment {
            label: item.label.clone(),
            path: item.path.clone(),
            size: item.size,
            start_angle: item_start,
            end_angle: item_end,
            inner_radius: inner_base + depth as f32 * ring_width,
            outer_radius: inner_base + (depth + 1) as f32 * ring_width,
            kind: item.kind,
        });

        if item.kind == SunburstSegmentKind::Directory {
            if let Some(child_index) = tree.node_index_for_path(&item.path) {
                append_segments_for_node(
                    tree,
                    child_index,
                    depth + 1,
                    max_depth,
                    per_node_limit,
                    item_start,
                    item_end,
                    inner_base,
                    ring_width,
                    output,
                );
            }
        }
    }
}

#[derive(Debug, Clone)]
struct SunburstItem {
    label: String,
    path: PathBuf,
    size: u64,
    kind: SunburstSegmentKind,
}

fn sunburst_items_for_node(
    tree: &DirectoryTree,
    node: &DirectoryNode,
    limit: usize,
) -> Vec<SunburstItem> {
    let mut items: Vec<_> = node
        .children
        .iter()
        .filter_map(|child_index| tree.nodes.get(*child_index))
        .map(|child| SunburstItem {
            label: child.record.name(),
            path: child.record.path.clone(),
            size: child.record.total_size,
            kind: SunburstSegmentKind::Directory,
        })
        .collect();

    if node.record.direct_file_size > 0 {
        items.push(SunburstItem {
            label: format!(
                "直属文件 ({})",
                format::count(node.record.direct_file_count)
            ),
            path: node.record.path.clone(),
            size: node.record.direct_file_size,
            kind: SunburstSegmentKind::DirectFiles,
        });
    }

    items.sort_by(|left, right| {
        right
            .size
            .cmp(&left.size)
            .then_with(|| left.label.cmp(&right.label))
    });
    aggregate_sunburst_items(items, node.record.path.clone(), limit)
}

fn aggregate_sunburst_items(
    mut items: Vec<SunburstItem>,
    dir: PathBuf,
    limit: usize,
) -> Vec<SunburstItem> {
    if items.len() <= limit || limit < 2 {
        return items;
    }

    let hidden = items.split_off(limit - 1);
    let hidden_count = hidden.len();
    let hidden_size = hidden
        .iter()
        .fold(0_u64, |total, item| total.saturating_add(item.size));
    if hidden_size > 0 {
        items.push(SunburstItem {
            label: format!("其它 {} 项", format::count(hidden_count as u64)),
            path: dir,
            size: hidden_size,
            kind: SunburstSegmentKind::Other,
        });
    }
    items
}

fn annular_sector_points(center: Pos2, segment: &SunburstSegment) -> Vec<Pos2> {
    let sweep = (segment.end_angle - segment.start_angle).max(0.0);
    let steps = ((sweep / TAU) * 96.0).ceil().clamp(4.0, 28.0) as usize;
    let mut points = Vec::with_capacity((steps + 1) * 2);

    for index in 0..=steps {
        let t = index as f32 / steps as f32;
        let angle = segment.start_angle + sweep * t;
        points.push(center + Vec2::angled(angle) * segment.outer_radius);
    }
    for index in (0..=steps).rev() {
        let t = index as f32 / steps as f32;
        let angle = segment.start_angle + sweep * t;
        points.push(center + Vec2::angled(angle) * segment.inner_radius);
    }
    points
}

fn pointer_in_segment(ui: &egui::Ui, center: Pos2, segment: &SunburstSegment) -> bool {
    ui.ctx()
        .pointer_hover_pos()
        .map(|pos| point_in_segment(pos, center, segment))
        .unwrap_or(false)
}

fn point_in_segment(pos: Pos2, center: Pos2, segment: &SunburstSegment) -> bool {
    let vector = pos - center;
    let radius = vector.length();
    if radius < segment.inner_radius || radius > segment.outer_radius {
        return false;
    }
    let mut angle = vector.y.atan2(vector.x);
    if angle < 0.0 {
        angle += TAU;
    }
    angle >= segment.start_angle && angle <= segment.end_angle
}

fn segment_kind_label(kind: SunburstSegmentKind) -> &'static str {
    match kind {
        SunburstSegmentKind::Directory => "目录",
        SunburstSegmentKind::DirectFiles => "直属文件合计",
        SunburstSegmentKind::Other => "其它项目合计",
    }
}

fn compact_label(label: &str, max_chars: usize) -> String {
    if label.chars().count() <= max_chars {
        return label.to_owned();
    }
    let mut output: String = label.chars().take(max_chars.saturating_sub(1)).collect();
    output.push('…');
    output
}

fn path_palette_index(path: &Path) -> usize {
    let mut hasher = DefaultHasher::new();
    path.hash(&mut hasher);
    hasher.finish() as usize
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

/// Draw a sunburst segment with cushion shading effect
fn draw_cushion_shaded_segment(
    painter: &egui::Painter,
    center: Pos2,
    segment: &SunburstSegment,
    base_color: Color32,
) {
    // Draw multiple layers for gradient effect
    let layer_count = 2;
    for layer in (0..layer_count).rev() {
        let shade_factor = (layer as f32 / layer_count as f32) * 0.3;
        let layer_color = ColorPalette::darken(base_color, shade_factor);
        let shrink_radius = layer as f32 * 3.0;
        
        let adjusted_segment = SunburstSegment {
            inner_radius: segment.inner_radius + shrink_radius,
            outer_radius: segment.outer_radius - shrink_radius,
            ..segment.clone()
        };
        
        let points = annular_sector_points(center, &adjusted_segment);
        painter.add(Shape::convex_polygon(
            points,
            layer_color,
            Stroke::NONE,
        ));
    }
    
    // Draw border
    let border_color = ColorPalette::darken(base_color, 0.4);
    let points = annular_sector_points(center, segment);
    painter.add(Shape::convex_polygon(
        points,
        base_color,
        Stroke::new(1.0, border_color),
    ));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{DirectoryRecord, DirectoryTree};
    use std::collections::HashMap;

    #[test]
    fn sunburst_segments_cover_full_circle_for_root_children() {
        let tree = test_tree();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        let segments = build_sunburst_segments(&tree, tree.root_index, rect, 1, 16);
        let sweep: f32 = segments
            .iter()
            .map(|segment| segment.end_angle - segment.start_angle)
            .sum();
        assert!((sweep - TAU).abs() < 0.001);
    }

    #[test]
    fn zero_size_tree_has_no_segments() {
        let root = PathBuf::from("C:\\root");
        let mut tree = DirectoryTree {
            root_index: 0,
            nodes: vec![DirectoryNode {
                record: DirectoryRecord {
                    path: root.clone(),
                    total_size: 0,
                    direct_file_count: 0,
                    direct_file_size: 0,
                    descendant_file_count: 0,
                },
                parent: None,
                children: Vec::new(),
            }],
            path_index: HashMap::new(),
        };
        tree.rebuild_path_index();
        let rect = Rect::from_min_size(Pos2::ZERO, Vec2::new(400.0, 400.0));
        assert!(build_sunburst_segments(&tree, 0, rect, 3, 16).is_empty());
    }

    #[test]
    fn sunburst_items_aggregate_other() {
        let tree = test_tree();
        let items = sunburst_items_for_node(&tree, &tree.nodes[tree.root_index], 2);
        assert_eq!(items.len(), 2);
        assert_eq!(items[1].kind, SunburstSegmentKind::Other);
    }

    fn test_tree() -> DirectoryTree {
        let root = PathBuf::from("C:\\root");
        let a = root.join("a");
        let b = root.join("b");
        let c = root.join("c");
        let mut tree = DirectoryTree {
            root_index: 0,
            nodes: vec![
                node(root.clone(), 100, None, vec![1, 2, 3]),
                node(a, 50, Some(0), Vec::new()),
                node(b, 30, Some(0), Vec::new()),
                node(c, 20, Some(0), Vec::new()),
            ],
            path_index: HashMap::new(),
        };
        tree.rebuild_path_index();
        tree
    }

    fn node(
        path: PathBuf,
        total_size: u64,
        parent: Option<usize>,
        children: Vec<usize>,
    ) -> DirectoryNode {
        DirectoryNode {
            record: DirectoryRecord {
                path,
                total_size,
                direct_file_count: 0,
                direct_file_size: 0,
                descendant_file_count: 0,
            },
            parent,
            children,
        }
    }
}
