//! 文件浏览与传输界面。

mod browser;
mod panel;
mod sftp_tab;
mod transfers;

pub(crate) use panel::{FilesPanel, FilesPanelEvent};
pub(crate) use sftp_tab::SftpView;
pub(crate) use transfers::{button as transfers_button, list as transfer_list};

/// 须在 [`crate::app::AppState`] 就绪后调用。
pub(crate) fn init(cx: &mut gpui_kit::App) {
    browser::bind_keys(cx);
    transfers::init(cx);
}

/// 以 1024 为进制的大小，保留三位有效数字左右。
pub(crate) fn format_size(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KB", "MB", "GB", "TB", "PB"];
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    let mut value = bytes as f64 / 1024.;
    let mut unit = 0;
    while value >= 1024. && unit + 1 < UNITS.len() {
        value /= 1024.;
        unit += 1;
    }
    let digits = if value >= 100. {
        0
    } else if value >= 10. {
        1
    } else {
        2
    };
    format!("{value:.digits$} {}", UNITS[unit])
}

/// 修改时间（Unix 秒）：今年内省略年份。
pub(crate) fn format_time(secs: u64) -> String {
    use chrono::{Datelike, Local, TimeZone};
    let Some(time) = i64::try_from(secs)
        .ok()
        .and_then(|s| Local.timestamp_opt(s, 0).single())
    else {
        return String::new();
    };
    if time.year() == Local::now().year() {
        time.format("%m-%d %H:%M").to_string()
    } else {
        time.format("%Y-%m-%d").to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes() {
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1023), "1023 B");
        assert_eq!(format_size(1536), "1.50 KB");
        assert_eq!(format_size(20 * 1024 * 1024), "20.0 MB");
        assert_eq!(format_size(300 * 1024 * 1024 * 1024), "300 GB");
    }
}
