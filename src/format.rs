pub fn bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KB", "MB", "GB", "TB", "PB"];

    if value == 0 {
        return "0 B".to_owned();
    }

    let mut size = value as f64;
    let mut unit_index = 0;
    while size >= 1024.0 && unit_index < UNITS.len() - 1 {
        size /= 1024.0;
        unit_index += 1;
    }

    if unit_index == 0 {
        format!("{} {}", value, UNITS[unit_index])
    } else if size >= 100.0 {
        format!("{:.0} {}", size, UNITS[unit_index])
    } else if size >= 10.0 {
        format!("{:.1} {}", size, UNITS[unit_index])
    } else {
        format!("{:.2} {}", size, UNITS[unit_index])
    }
}

pub fn percent(part: u64, total: u64) -> String {
    if total == 0 {
        return "0.0%".to_owned();
    }

    format!("{:.1}%", part as f64 * 100.0 / total as f64)
}

pub fn count(value: u64) -> String {
    let text = value.to_string();
    let mut result = String::new();
    let mut chars_from_end = 0;

    for ch in text.chars().rev() {
        if chars_from_end == 3 {
            result.push(',');
            chars_from_end = 0;
        }
        result.push(ch);
        chars_from_end += 1;
    }

    result.chars().rev().collect()
}

/// Format duration in human-readable format (e.g., "2m 35s", "1h 15m", "45s")
pub fn duration(d: std::time::Duration) -> String {
    let total_secs = d.as_secs();

    if total_secs < 60 {
        return format!("{}s", total_secs);
    }

    let hours = total_secs / 3600;
    let minutes = (total_secs % 3600) / 60;
    let secs = total_secs % 60;

    if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else {
        format!("{}m {}s", minutes, secs)
    }
}
