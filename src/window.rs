use serde::{Deserialize, Serialize};
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::Diagnostics::ToolHelp::*;

use windows_sys::Win32::UI::WindowsAndMessaging::*;

// ── Types ──────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowInfo {
    pub hwnd: usize,
    pub title: String,
    pub process_name: String,
    pub pid: u32,
    pub rect: ScreenRect,
    pub is_visible: bool,
    pub z_order: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScreenRect {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl ScreenRect {
    pub fn width(&self) -> i32 {
        self.right - self.left
    }
    pub fn height(&self) -> i32 {
        self.bottom - self.top
    }
    pub fn is_empty(&self) -> bool {
        self.width() <= 0 || self.height() <= 0
    }
}

impl From<RECT> for ScreenRect {
    fn from(r: RECT) -> Self {
        Self {
            left: r.left,
            top: r.top,
            right: r.right,
            bottom: r.bottom,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterType {
    TitleExact,
    TitleContains,
    TitleRegex,
    ProcessName,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(rename_all = "snake_case")]
pub enum FilterMode {
    Include,
    Exclude,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FilterRule {
    #[serde(rename = "type")]
    pub rule_type: FilterType,
    pub value: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WindowFilter {
    pub mode: FilterMode,
    pub rules: Vec<FilterRule>,
}

// ── Monitor info ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MonitorInfo {
    pub index: usize,
    pub rect: ScreenRect,
    pub is_primary: bool,
    pub name: String,
}

// ── Window enumeration ────────────────────────────────────────────────

pub fn enumerate_windows(include_minimized: bool) -> Vec<WindowInfo> {
    let mut windows: Vec<WindowInfo> = Vec::new();

    unsafe {
        EnumWindows(
            Some(enum_windows_callback),
            &mut windows as *mut _ as LPARAM,
        );
    }

    // Assign z-order (0 = topmost)
    for (i, w) in windows.iter_mut().enumerate() {
        w.z_order = i as u32;
    }

    if !include_minimized {
        windows.retain(|w| {
            w.is_visible && w.rect.width() > 0 && w.rect.height() > 0
        });
    }

    windows
}

unsafe extern "system" fn enum_windows_callback(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let windows = &mut *(lparam as *mut Vec<WindowInfo>);

    // Skip invisible windows
    if IsWindowVisible(hwnd) == 0 {
        return 1;
    }

    // Get window title
    let title = get_window_title(hwnd);

    // Get window rect
    let mut rect = std::mem::zeroed::<RECT>();
    if GetWindowRect(hwnd, &mut rect) == 0 {
        return 1;
    }

    // Skip zero-size windows
    if rect.right <= rect.left || rect.bottom <= rect.top {
        return 1;
    }

    // Skip windows with empty title and small size
    if title.is_empty() && (rect.right - rect.left) < 2 && (rect.bottom - rect.top) < 2 {
        return 1;
    }

    // Get process ID
    let mut pid: u32 = 0;
    GetWindowThreadProcessId(hwnd, &mut pid);

    // Get process name
    let process_name = get_process_name(pid).unwrap_or_default();

    windows.push(WindowInfo {
        hwnd: hwnd as usize,
        title,
        process_name,
        pid,
        rect: ScreenRect::from(rect),
        is_visible: true,
        z_order: 0,
    });

    1 // Continue enumeration
}

fn get_window_title(hwnd: HWND) -> String {
    unsafe {
        let len = GetWindowTextLengthW(hwnd);
        if len == 0 {
            return String::new();
        }
        let mut buf = vec![0u16; (len as usize) + 1];
        let copied = GetWindowTextW(hwnd, buf.as_mut_ptr(), buf.len() as i32);
        if copied == 0 {
            return String::new();
        }
        OsString::from_wide(&buf[..copied as usize])
            .to_string_lossy()
            .into_owned()
    }
}

pub fn get_process_name(pid: u32) -> Option<String> {
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, pid);
        if snapshot == INVALID_HANDLE_VALUE {
            return None;
        }

        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;

        if Process32FirstW(snapshot, &mut entry) == 0 {
            CloseHandle(snapshot);
            return None;
        }

        loop {
            if entry.th32ProcessID == pid {
                let name = OsString::from_wide(
                    &entry.szExeFile.iter().take_while(|&&c| c != 0).copied().collect::<Vec<u16>>(),
                )
                .to_string_lossy()
                .into_owned();
                CloseHandle(snapshot);
                return Some(name);
            }
            if Process32NextW(snapshot, &mut entry) == 0 {
                break;
            }
        }

        CloseHandle(snapshot);
        None
    }
}

// ── Monitor enumeration ───────────────────────────────────────────────

pub fn enumerate_monitors() -> Vec<MonitorInfo> {
    let mut monitors: Vec<MonitorInfo> = Vec::new();

    unsafe {
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(enum_monitors_callback),
            &mut monitors as *mut _ as LPARAM,
        );
    }

    // Sort by position (left, then top) for consistent indexing
    monitors.sort_by(|a, b| {
        a.rect
            .left
            .cmp(&b.rect.left)
            .then(a.rect.top.cmp(&b.rect.top))
    });

    // Assign indices
    for (i, m) in monitors.iter_mut().enumerate() {
        m.index = i;
    }

    monitors
}

unsafe extern "system" fn enum_monitors_callback(
    hmonitor: HMONITOR,
    _hdc: HDC,
    _lprc_clip: *mut RECT,
    lparam: LPARAM,
) -> BOOL {
    let monitors = &mut *(lparam as *mut Vec<MonitorInfo>);

    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;

    if GetMonitorInfoW(hmonitor, &mut info as *mut MONITORINFOEXW as *mut MONITORINFO) != 0 {
        let name = OsString::from_wide(
            &info.szDevice.iter().take_while(|&&c| c != 0).copied().collect::<Vec<u16>>(),
        )
        .to_string_lossy()
        .into_owned();

        let is_primary = (info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY) != 0;

        monitors.push(MonitorInfo {
            index: 0,
            rect: ScreenRect::from(info.monitorInfo.rcMonitor),
            is_primary,
            name,
        });
    }

    1 // Continue enumeration
}

pub fn get_monitor_rect(index: usize) -> Result<ScreenRect, String> {
    let monitors = enumerate_monitors();
    monitors
        .get(index)
        .map(|m| m.rect.clone())
        .ok_or_else(|| {
            format!(
                "Monitor index {} out of range (found {} monitors)",
                index,
                monitors.len()
            )
        })
}

// ── Window matching ───────────────────────────────────────────────────

pub fn match_rule(window: &WindowInfo, rule: &FilterRule) -> bool {
    match rule.rule_type {
        FilterType::TitleExact => window.title == rule.value,
        FilterType::TitleContains => window
            .title
            .to_lowercase()
            .contains(&rule.value.to_lowercase()),
        FilterType::TitleRegex => {
            let Ok(re) = regex::Regex::new(&rule.value) else {
                tracing::warn!("Invalid regex pattern: {}", rule.value);
                return false;
            };
            re.is_match(&window.title)
        }
        FilterType::ProcessName => window
            .process_name
            .to_lowercase()
            .contains(&rule.value.to_lowercase()),
    }
}

pub fn match_window(window: &WindowInfo, filter: &WindowFilter) -> bool {
    let any_match = filter.rules.iter().any(|rule| match_rule(window, rule));
    match filter.mode {
        FilterMode::Include => any_match,
        FilterMode::Exclude => !any_match,
    }
}

pub fn filter_windows(windows: &[WindowInfo], filter: &WindowFilter) -> Vec<WindowInfo> {
    windows
        .iter()
        .filter(|w| match_window(w, filter))
        .cloned()
        .collect()
}

// ── Active/foreground window ───────────────────────────────────────────

/// Get the currently active (foreground) window's information.
/// Returns None if no window is in the foreground.
pub fn get_active_window() -> Option<WindowInfo> {
    unsafe {
        let hwnd = GetForegroundWindow();
        if hwnd.is_null() {
            return None;
        }

        let title = get_window_title(hwnd);

        // Get rect
        let mut rect = std::mem::zeroed::<RECT>();
        if GetWindowRect(hwnd, &mut rect) == 0 {
            return None;
        }

        let is_visible = IsWindowVisible(hwnd) != 0;

        // Get PID
        let mut pid: u32 = 0;
        GetWindowThreadProcessId(hwnd, &mut pid);

        let process_name = get_process_name(pid).unwrap_or_default();

        Some(WindowInfo {
            hwnd: hwnd as usize,
            title,
            process_name,
            pid,
            rect: ScreenRect::from(rect),
            is_visible,
            z_order: u32::MAX, // foreground is always top
        })
    }
}
