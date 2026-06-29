//! Unified color palette system for disk analysis visualization
//!
//! Provides consistent color mapping based on file extensions and categories.
//! Used by Treemap, Sunburst, and ExtensionPanel for color-coordinated display.

use std::collections::HashMap;
use egui::Color32;

/// File category classification
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileCategory {
    Executable,    // exe, dll, msi, cab
    Document,      // doc, pdf, txt, rtf, odt
    Media,         // mp4, mp3, jpg, png, avi, mov
    Code,          // rs, py, js, cpp, h, java
    System,        // sys, drv, cat, inf, reg
    Temporary,     // tmp, log, bak, old, temp
    Archive,       // zip, rar, 7z, tar, gz, bz2
    Data,          // json, xml, csv, yaml, toml
    Other,
}

impl FileCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Executable => "可执行文件",
            Self::Document => "文档",
            Self::Media => "媒体",
            Self::Code => "代码",
            Self::System => "系统",
            Self::Temporary => "临时",
            Self::Archive => "压缩包",
            Self::Data => "数据",
            Self::Other => "其它",
        }
    }

    pub fn icon(self) -> &'static str {
        match self {
            Self::Executable => "⚙",
            Self::Document => "📄",
            Self::Media => "🎬",
            Self::Code => "💻",
            Self::System => "🔧",
            Self::Temporary => "🗑",
            Self::Archive => "📦",
            Self::Data => "📊",
            Self::Other => "",
        }
    }

    pub fn default_color(self) -> Color32 {
        match self {
            Self::Executable => Color32::from_rgb(220, 53, 69),      // Red
            Self::Document => Color32::from_rgb(72, 166, 233),       // Light Blue
            Self::Media => Color32::from_rgb(156, 39, 176),          // Purple
            Self::Code => Color32::from_rgb(76, 175, 80),            // Green
            Self::System => Color32::from_rgb(255, 152, 0),          // Orange
            Self::Temporary => Color32::from_rgb(158, 158, 158),     // Gray
            Self::Archive => Color32::from_rgb(255, 193, 7),         // Amber/Yellow
            Self::Data => Color32::from_rgb(0, 150, 136),            // Teal
            Self::Other => Color32::from_rgb(121, 134, 203),         // Soft Blue
        }
    }
}

/// Extension to category mapping
const EXTENSION_CATEGORIES: &[(&str, FileCategory)] = &[
    // Executable
    ("exe", FileCategory::Executable),
    ("dll", FileCategory::Executable),
    ("msi", FileCategory::Executable),
    ("cab", FileCategory::Executable),
    ("com", FileCategory::Executable),
    ("bat", FileCategory::Executable),
    ("cmd", FileCategory::Executable),
    ("ps1", FileCategory::Executable),
    
    // Document
    ("doc", FileCategory::Document),
    ("docx", FileCategory::Document),
    ("pdf", FileCategory::Document),
    ("txt", FileCategory::Document),
    ("rtf", FileCategory::Document),
    ("odt", FileCategory::Document),
    ("xlsx", FileCategory::Document),
    ("xls", FileCategory::Document),
    ("ppt", FileCategory::Document),
    ("pptx", FileCategory::Document),
    ("md", FileCategory::Document),
    ("html", FileCategory::Document),
    ("htm", FileCategory::Document),
    
    // Media
    ("mp4", FileCategory::Media),
    ("mp3", FileCategory::Media),
    ("jpg", FileCategory::Media),
    ("jpeg", FileCategory::Media),
    ("png", FileCategory::Media),
    ("gif", FileCategory::Media),
    ("bmp", FileCategory::Media),
    ("avi", FileCategory::Media),
    ("mov", FileCategory::Media),
    ("mkv", FileCategory::Media),
    ("wmv", FileCategory::Media),
    ("flv", FileCategory::Media),
    ("wav", FileCategory::Media),
    ("flac", FileCategory::Media),
    ("aac", FileCategory::Media),
    ("ogg", FileCategory::Media),
    ("wma", FileCategory::Media),
    ("webm", FileCategory::Media),
    ("svg", FileCategory::Media),
    ("ico", FileCategory::Media),
    ("tiff", FileCategory::Media),
    ("webp", FileCategory::Media),
    
    // Code
    ("rs", FileCategory::Code),
    ("py", FileCategory::Code),
    ("js", FileCategory::Code),
    ("ts", FileCategory::Code),
    ("cpp", FileCategory::Code),
    ("c", FileCategory::Code),
    ("h", FileCategory::Code),
    ("hpp", FileCategory::Code),
    ("java", FileCategory::Code),
    ("go", FileCategory::Code),
    ("rb", FileCategory::Code),
    ("php", FileCategory::Code),
    ("swift", FileCategory::Code),
    ("kt", FileCategory::Code),
    ("cs", FileCategory::Code),
    ("lua", FileCategory::Code),
    ("sh", FileCategory::Code),
    ("bash", FileCategory::Code),
    ("zsh", FileCategory::Code),
    ("fish", FileCategory::Code),
    ("psm1", FileCategory::Code),
    ("asm", FileCategory::Code),
    ("s", FileCategory::Code),
    
    // System
    ("sys", FileCategory::System),
    ("drv", FileCategory::System),
    ("cat", FileCategory::System),
    ("inf", FileCategory::System),
    ("reg", FileCategory::System),
    ("dll", FileCategory::System),
    ("ocx", FileCategory::System),
    ("ax", FileCategory::System),
    
    // Temporary
    ("tmp", FileCategory::Temporary),
    ("temp", FileCategory::Temporary),
    ("log", FileCategory::Temporary),
    ("bak", FileCategory::Temporary),
    ("old", FileCategory::Temporary),
    ("swp", FileCategory::Temporary),
    ("cache", FileCategory::Temporary),
    
    // Archive
    ("zip", FileCategory::Archive),
    ("rar", FileCategory::Archive),
    ("7z", FileCategory::Archive),
    ("tar", FileCategory::Archive),
    ("gz", FileCategory::Archive),
    ("bz2", FileCategory::Archive),
    ("xz", FileCategory::Archive),
    ("cab", FileCategory::Archive),
    ("iso", FileCategory::Archive),
    ("dmg", FileCategory::Archive),
    ("pkg", FileCategory::Archive),
    ("deb", FileCategory::Archive),
    ("rpm", FileCategory::Archive),
    
    // Data
    ("json", FileCategory::Data),
    ("xml", FileCategory::Data),
    ("csv", FileCategory::Data),
    ("yaml", FileCategory::Data),
    ("yml", FileCategory::Data),
    ("toml", FileCategory::Data),
    ("ini", FileCategory::Data),
    ("cfg", FileCategory::Data),
    ("conf", FileCategory::Data),
    ("sql", FileCategory::Data),
    ("db", FileCategory::Data),
    ("sqlite", FileCategory::Data),
];

/// Color palette for unified visualization
pub struct ColorPalette {
    category_colors: HashMap<FileCategory, Color32>,
    extension_cache: HashMap<String, (FileCategory, Color32)>,
}

impl ColorPalette {
    pub fn new() -> Self {
        Self {
            category_colors: Self::default_category_colors(),
            extension_cache: HashMap::new(),
        }
    }

    fn default_category_colors() -> HashMap<FileCategory, Color32> {
        let mut colors = HashMap::new();
        for category in [
            FileCategory::Executable,
            FileCategory::Document,
            FileCategory::Media,
            FileCategory::Code,
            FileCategory::System,
            FileCategory::Temporary,
            FileCategory::Archive,
            FileCategory::Data,
            FileCategory::Other,
        ] {
            colors.insert(category, category.default_color());
        }
        colors
    }

    /// Get color for file extension (thread-safe, no cache mutation)
    pub fn color_for_extension(&self, extension: &str) -> Color32 {
        let normalized = extension.to_ascii_lowercase();
        
        // Find category and color directly
        let category = Self::category_for_extension_static(&normalized);
        self.category_colors.get(&category).copied()
            .unwrap_or_else(|| FileCategory::Other.default_color())
    }

    /// Static color lookup without struct instance
    pub fn color_for_extension_static(extension: &str) -> Color32 {
        let category = Self::category_for_extension_static(extension);
        category.default_color()
    }

    /// Get color for extension with caching (mutable version for performance)
    pub fn color_for_extension_cached(&mut self, extension: &str) -> Color32 {
        let normalized = extension.to_ascii_lowercase();
        
        // Check cache first
        if let Some((_, color)) = self.extension_cache.get(&normalized) {
            return *color;
        }

        // Find category and color
        let category = Self::category_for_extension_static(&normalized);
        let color = self.category_colors.get(&category).copied()
            .unwrap_or_else(|| FileCategory::Other.default_color());

        // Cache the result
        self.extension_cache.insert(normalized, (category, color));
        
        color
    }

    /// Get category for file extension (static, no cache)
    pub fn category_for_extension_static(extension: &str) -> FileCategory {
        let normalized = extension.to_ascii_lowercase();
        EXTENSION_CATEGORIES
            .iter()
            .find(|(ext, _)| *ext == normalized)
            .map(|(_, cat)| *cat)
            .unwrap_or(FileCategory::Other)
    }

    /// Get category for file extension (using self for consistency)
    pub fn category_for_extension(&self, extension: &str) -> FileCategory {
        Self::category_for_extension_static(extension)
    }

    /// Get color for category
    pub fn color_for_category(&self, category: FileCategory) -> Color32 {
        self.category_colors.get(&category).copied()
            .unwrap_or_else(|| FileCategory::Other.default_color())
    }

    /// Darken a color for depth visualization
    pub fn darken(color: Color32, factor: f32) -> Color32 {
        let factor = factor.clamp(0.0, 1.0);
        let r = (color.r() as f32 * (1.0 - factor)) as u8;
        let g = (color.g() as f32 * (1.0 - factor)) as u8;
        let b = (color.b() as f32 * (1.0 - factor)) as u8;
        Color32::from_rgb(r, g, b)
    }

    /// Lighten a color for highlights
    pub fn lighten(color: Color32, factor: f32) -> Color32 {
        let factor = factor.clamp(0.0, 1.0);
        let r = ((color.r() as f32 + (255.0 - color.r() as f32) * factor) as u8).min(255);
        let g = ((color.g() as f32 + (255.0 - color.g() as f32) * factor) as u8).min(255);
        let b = ((color.b() as f32 + (255.0 - color.b() as f32) * factor) as u8).min(255);
        Color32::from_rgb(r, g, b)
    }

    /// Draw cushion shading effect for rectangles
    pub fn draw_cushion_rect(
        painter: &egui::Painter,
        rect: egui::Rect,
        base_color: Color32,
        corner_radius: f32,
        depth_layers: usize,
    ) {
        let layer_count = depth_layers.min(4);
        
        // Draw from outer to inner layers
        for layer in 0..layer_count {
            let shade_factor = (layer as f32 / layer_count as f32) * 0.4;
            let layer_color = Self::darken(base_color, shade_factor);
            let shrink_amount = layer as f32 * 1.5;
            let layer_rect = rect.shrink(shrink_amount);
            
            painter.rect_filled(layer_rect, corner_radius.max(0.0), layer_color);
        }

        // Draw border stroke
        painter.rect_stroke(
            rect,
            corner_radius,
            egui::Stroke::new(1.0, Self::darken(base_color, 0.5)),
            egui::StrokeKind::Inside,
        );
    }
}

impl Default for ColorPalette {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extension_categories() {
        assert_eq!(
            ColorPalette::category_for_extension_static("exe"),
            FileCategory::Executable
        );
        assert_eq!(
            ColorPalette::category_for_extension_static("rs"),
            FileCategory::Code
        );
        assert_eq!(
            ColorPalette::category_for_extension_static("mp4"),
            FileCategory::Media
        );
        assert_eq!(
            ColorPalette::category_for_extension_static("unknown_ext"),
            FileCategory::Other
        );
    }

    #[test]
    fn test_color_darken_lighten() {
        let base = Color32::from_rgb(100, 150, 200);
        
        let darker = ColorPalette::darken(base, 0.5);
        assert!(darker.r() < base.r());
        assert!(darker.g() < base.g());
        assert!(darker.b() < base.b());
        
        let lighter = ColorPalette::lighten(base, 0.5);
        assert!(lighter.r() > base.r());
        assert!(lighter.g() > base.g());
        assert!(lighter.b() > base.b());
    }

    #[test]
    fn test_color_palette_caching() {
        let mut palette = ColorPalette::new();
        let color1 = palette.color_for_extension("exe");
        let color2 = palette.color_for_extension("EXE"); // Should be same
        assert_eq!(color1, color2);
    }
}
