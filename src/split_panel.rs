//! Resizable split panel component for egui
//!
//! Provides horizontal and vertical split panels with draggable splitter bars.
//! The split ratio is persisted using egui's ID system.

use egui::{Color32, Id, Pos2, Rect, Sense, Ui, Vec2};

const DEFAULT_SPLITTER_WIDTH: f32 = 4.0;
const MIN_RATIO: f32 = 0.10;
const MAX_RATIO: f32 = 0.90;

/// Horizontal split panel (left-right)
pub struct HorizontalSplit {
    id: Id,
    initial_ratio: f32,
    min_left_width: f32,
    min_right_width: f32,
    splitter_width: f32,
}

impl HorizontalSplit {
    pub fn new(id_source: impl Hash, initial_ratio: f32) -> Self {
        Self {
            id: Id::new(id_source),
            initial_ratio: initial_ratio.clamp(MIN_RATIO, MAX_RATIO),
            min_left_width: 180.0,
            min_right_width: 260.0,
            splitter_width: DEFAULT_SPLITTER_WIDTH,
        }
    }

    pub fn with_min_widths(mut self, min_left: f32, min_right: f32) -> Self {
        self.min_left_width = min_left;
        self.min_right_width = min_right;
        self
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        left_content: impl FnOnce(&mut Ui),
        right_content: impl FnOnce(&mut Ui),
    ) {
        let available_width = ui.available_width();
        let available_height = ui.available_height();

        // Get or initialize the ratio from persistence
        let ratio = ui.data_mut(|d| d.get_temp::<f32>(self.id).unwrap_or(self.initial_ratio));

        // Calculate min/max ratios based on constraints
        let min_ratio = self.min_left_width / available_width;
        let max_ratio = 1.0 - self.min_right_width / available_width;
        let clamped_ratio = ratio.clamp(min_ratio.max(MIN_RATIO), max_ratio.min(MAX_RATIO));

        let left_width = (available_width * clamped_ratio).max(self.min_left_width);
        let right_width =
            (available_width - left_width - self.splitter_width).max(self.min_right_width);

        // Allocate space for left panel
        let left_rect =
            Rect::from_min_size(ui.cursor().min, Vec2::new(left_width, available_height));

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(left_rect), |ui| {
            left_content(ui);
        });

        // Position for splitter
        let splitter_x = left_rect.max.x;
        let splitter_rect = Rect::from_min_size(
            Pos2::new(splitter_x, left_rect.min.y),
            Vec2::new(self.splitter_width, available_height),
        );

        // Handle splitter interaction
        let splitter_id = self.id.with("splitter");
        let response = ui.interact(splitter_rect, splitter_id, Sense::drag());

        // Update ratio if dragged
        if response.dragged() {
            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                let relative_x = pointer_pos.x - left_rect.min.x;
                let updated_ratio = relative_x / available_width;
                let clamped =
                    updated_ratio.clamp(min_ratio.max(MIN_RATIO), max_ratio.min(MAX_RATIO));
                // Persist the new ratio
                ui.data_mut(|d| {
                    d.insert_temp(self.id, clamped);
                });
            }
        }

        // Draw splitter
        let painter = ui.painter_at(splitter_rect);
        let splitter_color = if response.dragged() {
            Color32::from_rgb(100, 150, 200) // Active highlight
        } else if response.hovered() {
            Color32::from_rgb(80, 120, 180) // Hover highlight
        } else {
            Color32::from_rgb(50, 55, 65) // Default subtle
        };
        painter.rect_filled(splitter_rect, 0.0, splitter_color);

        // Optional: draw resize indicator on hover
        if response.hovered() || response.dragged() {
            painter.text(
                splitter_rect.center(),
                egui::Align2::CENTER_CENTER,
                "⟷",
                egui::FontId::proportional(12.0),
                Color32::from_gray(200),
            );
        }

        // Allocate space for right panel
        let right_rect = Rect::from_min_size(
            Pos2::new(splitter_rect.max.x, left_rect.min.y),
            Vec2::new(right_width, available_height),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(right_rect), |ui| {
            right_content(ui);
        });
    }

    /// Get current ratio (useful for other calculations)
    pub fn get_ratio(&self, ui: &Ui) -> f32 {
        ui.data_mut(|d| d.get_temp::<f32>(self.id).unwrap_or(self.initial_ratio))
    }
}

/// Vertical split panel (top-bottom)
pub struct VerticalSplit {
    id: Id,
    initial_ratio: f32,
    min_top_height: f32,
    min_bottom_height: f32,
    splitter_height: f32,
}

impl VerticalSplit {
    pub fn new(id_source: impl Hash, initial_ratio: f32) -> Self {
        Self {
            id: Id::new(id_source),
            initial_ratio: initial_ratio.clamp(MIN_RATIO, MAX_RATIO),
            min_top_height: 260.0,
            min_bottom_height: 120.0,
            splitter_height: DEFAULT_SPLITTER_WIDTH,
        }
    }

    pub fn with_min_heights(mut self, min_top: f32, min_bottom: f32) -> Self {
        self.min_top_height = min_top;
        self.min_bottom_height = min_bottom;
        self
    }

    pub fn show(
        &mut self,
        ui: &mut Ui,
        top_content: impl FnOnce(&mut Ui),
        bottom_content: impl FnOnce(&mut Ui),
    ) {
        let available_width = ui.available_width();
        let available_height = ui.available_height();

        // Get or initialize the ratio from persistence
        let ratio = ui.data_mut(|d| d.get_temp::<f32>(self.id).unwrap_or(self.initial_ratio));

        // Calculate min/max ratios based on constraints
        let min_ratio = self.min_top_height / available_height;
        let max_ratio = 1.0 - self.min_bottom_height / available_height;
        let clamped_ratio = ratio.clamp(min_ratio.max(MIN_RATIO), max_ratio.min(MAX_RATIO));

        let top_height = (available_height * clamped_ratio).max(self.min_top_height);
        let bottom_height =
            (available_height - top_height - self.splitter_height).max(self.min_bottom_height);

        // Allocate space for top panel
        let top_rect = Rect::from_min_size(ui.cursor().min, Vec2::new(available_width, top_height));

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(top_rect), |ui| {
            top_content(ui);
        });

        // Position for splitter
        let splitter_y = top_rect.max.y;
        let splitter_rect = Rect::from_min_size(
            Pos2::new(top_rect.min.x, splitter_y),
            Vec2::new(available_width, self.splitter_height),
        );

        // Handle splitter interaction
        let splitter_id = self.id.with("splitter");
        let response = ui.interact(splitter_rect, splitter_id, Sense::drag());

        // Update ratio if dragged
        if response.dragged() {
            if let Some(pointer_pos) = ui.ctx().pointer_interact_pos() {
                let relative_y = pointer_pos.y - top_rect.min.y;
                let updated_ratio = relative_y / available_height;
                let clamped =
                    updated_ratio.clamp(min_ratio.max(MIN_RATIO), max_ratio.min(MAX_RATIO));
                // Persist the new ratio
                ui.data_mut(|d| {
                    d.insert_temp(self.id, clamped);
                });
            }
        }

        // Draw splitter
        let painter = ui.painter_at(splitter_rect);
        let splitter_color = if response.dragged() {
            Color32::from_rgb(100, 150, 200) // Active highlight
        } else if response.hovered() {
            Color32::from_rgb(80, 120, 180) // Hover highlight
        } else {
            Color32::from_rgb(50, 55, 65) // Default subtle
        };
        painter.rect_filled(splitter_rect, 0.0, splitter_color);

        // Optional: draw resize indicator on hover
        if response.hovered() || response.dragged() {
            painter.text(
                splitter_rect.center(),
                egui::Align2::CENTER_CENTER,
                "↕",
                egui::FontId::proportional(12.0),
                Color32::from_gray(200),
            );
        }

        // Allocate space for bottom panel
        let bottom_rect = Rect::from_min_size(
            Pos2::new(top_rect.min.x, splitter_rect.max.y),
            Vec2::new(available_width, bottom_height),
        );

        ui.allocate_new_ui(egui::UiBuilder::new().max_rect(bottom_rect), |ui| {
            bottom_content(ui);
        });
    }

    /// Get current ratio (useful for other calculations)
    pub fn get_ratio(&self, ui: &Ui) -> f32 {
        ui.data_mut(|d| d.get_temp::<f32>(self.id).unwrap_or(self.initial_ratio))
    }
}

/// Helper trait for hashable ID sources
pub trait Hash: std::hash::Hash + Clone + 'static {}
impl<T: std::hash::Hash + Clone + 'static> Hash for T {}
