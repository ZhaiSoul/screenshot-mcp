use crate::dxgi;
use crate::window::{self, ScreenRect, WindowFilter};
use image::{DynamicImage, ImageBuffer, Rgba, RgbaImage};
use std::collections::HashSet;
use std::ffi::c_void;
use std::ptr;
use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::UI::WindowsAndMessaging::*;

// PW_RENDERFULLCONTENT flag for PrintWindow
const PW_RENDERFULLCONTENT: u32 = 2;

// PrintWindow is not in windows-sys, define it manually
#[link(name = "user32")]
extern "system" {
    fn PrintWindow(hwnd: HWND, hdcblit: HDC, nflags: u32) -> i32;
}

// ── Public API ────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum ImageFormat {
    Png,
    Jpeg,
}

pub fn take_screenshot(
    monitor_index: usize,
    filters: Option<&WindowFilter>,
    format: ImageFormat,
) -> Result<Vec<u8>, String> {
    let monitor_rect = window::get_monitor_rect(monitor_index)?;

    let canvas = if let Some(filter) = filters {
        composite_screenshot(monitor_index, &monitor_rect, filter)?
    } else {
        capture_full_screen(monitor_index, &monitor_rect)?
    };

    encode_image(&canvas, format)
}

// ── Full screen capture (no filter) ───────────────────────────────────

fn capture_full_screen(monitor_index: usize, monitor_rect: &ScreenRect) -> Result<RgbaImage, String> {
    // Try DXGI Desktop Duplication first (handles HDR correctly)
    match dxgi::capture_screen(monitor_index) {
        Ok(image) => {
            tracing::debug!("DXGI capture succeeded");
            return Ok(image);
        }
        Err(e) => {
            tracing::warn!("DXGI capture failed ({}), falling back to GDI", e);
        }
    }

    // Fallback: GDI BitBlt
    capture_full_screen_gdi(monitor_rect)
}

fn capture_full_screen_gdi(monitor_rect: &ScreenRect) -> Result<RgbaImage, String> {
    let width = monitor_rect.width() as u32;
    let height = monitor_rect.height() as u32;
    if width == 0 || height == 0 {
        return Err("Monitor has zero size".into());
    }

    unsafe {
        let hdc_screen = GetDC(ptr::null_mut());
        if hdc_screen.is_null() {
            return Err("GetDC failed".into());
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_null() {
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("CreateCompatibleDC failed".into());
        }

        let hbitmap = CreateCompatibleBitmap(hdc_screen, width as i32, height as i32);
        if hbitmap.is_null() {
            DeleteDC(hdc_mem);
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("CreateCompatibleBitmap failed".into());
        }

        let old_bmp = SelectObject(hdc_mem, hbitmap);

        let result = BitBlt(
            hdc_mem,
            0,
            0,
            width as i32,
            height as i32,
            hdc_screen,
            monitor_rect.left,
            monitor_rect.top,
            SRCCOPY | CAPTUREBLT,
        );

        if result == 0 {
            SelectObject(hdc_mem, old_bmp);
            DeleteObject(hbitmap);
            DeleteDC(hdc_mem);
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("BitBlt failed".into());
        }

        let image = bitmap_to_image(hdc_mem, hbitmap, width, height);

        SelectObject(hdc_mem, old_bmp);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(ptr::null_mut(), hdc_screen);

        image
    }
}

// ── Composite screenshot (with window filter) ────────────────────────

fn composite_screenshot(
    monitor_index: usize,
    monitor_rect: &ScreenRect,
    filter: &WindowFilter,
) -> Result<RgbaImage, String> {
    let monitor_width = monitor_rect.width() as u32;
    let monitor_height = monitor_rect.height() as u32;
    if monitor_width == 0 || monitor_height == 0 {
        return Err("Monitor has zero size".into());
    }

    // Step 1: Enumerate all visible windows
    let all_windows = window::enumerate_windows(false);

    // Step 2: Filter windows based on the filter rules
    let filtered_windows = window::filter_windows(&all_windows, filter);

    if filtered_windows.is_empty() {
        tracing::info!("No windows matched the filter, falling back to full screen");
        return capture_full_screen(monitor_index, monitor_rect);
    }

    // Step 3: Sort by z_order descending (draw bottommost first, topmost last)
    let mut sorted_windows = filtered_windows;
    sorted_windows.sort_by(|a, b| b.z_order.cmp(&a.z_order));

    // Step 4: Branch on filter mode
    match filter.mode {
        window::FilterMode::Include => {
            composite_include_mode(monitor_index, &sorted_windows)
        }
        window::FilterMode::Exclude => {
            composite_exclude_mode(monitor_rect, &sorted_windows)
        }
    }
}

// ── Monitor fallback logic ───────────────────────────────────────────────

/// Check if an image is effectively all black (or nearly so)
fn is_image_all_black(img: &RgbaImage) -> bool {
    let w = img.width();
    let h = img.height();
    if w == 0 || h == 0 { return true; }

    let mut sum: u64 = 0;
    let count = ((w / 16 + 1) * (h / 16 + 1)).max(1);
    let step_x = (w as usize / (w as usize / 16 + 1)).max(1) as u32;
    let step_y = (h as usize / (h as usize / 16 + 1)).max(1) as u32;

    for y in (0..h).step_by(step_y as usize) {
        for x in (0..w).step_by(step_x as usize) {
            let p = img.get_pixel(x, y);
            sum += p[0] as u64 + p[1] as u64 + p[2] as u64;
        }
    }
    let avg = sum / count.max(1) as u64;
    avg < 5
}

/// Try DXGI capture, auto-detecting the correct monitor from window position.
fn capture_with_monitor_fallback(
    preferred_monitor: usize,
    windows: &[window::WindowInfo],
) -> Result<(RgbaImage, usize, ScreenRect), String> {
    let monitors = window::enumerate_monitors();
    if monitors.is_empty() {
        return Err("No monitors found".into());
    }

    // Auto-detect best monitor by finding which overlaps most with target windows
    let best_monitor = if !windows.is_empty() {
        let bounds = compute_bounding_box(windows);
        find_best_monitor_for_rect(&monitors, &bounds).unwrap_or(preferred_monitor)
    } else {
        preferred_monitor.min(monitors.len() - 1)
    };

    tracing::info!(
        "Auto-detected monitor {} for capture (preferred: {})",
        best_monitor, preferred_monitor
    );

    // Try order: auto-detected best → user-specified preferred → all others
    let mut tried: HashSet<usize> = HashSet::new();
    for &idx in &[best_monitor, preferred_monitor] {
        if idx >= monitors.len() || !tried.insert(idx) { continue; }
        if let Some(r) = try_dxgi_capture(idx, &monitors) { return Ok(r); }
    }
    for i in 0..monitors.len() {
        if !tried.insert(i) { continue; }
        if let Some(r) = try_dxgi_capture(i, &monitors) { return Ok(r); }
    }

    // Last resort: GDI fallback on best-detected monitor
    tracing::warn!("All DXGI captures failed/black, falling back to GDI");
    let gdi_img = capture_full_screen_gdi(&monitors[best_monitor].rect)?;
    Ok((gdi_img, best_monitor, monitors[best_monitor].rect.clone()))
}

fn try_dxgi_capture(idx: usize, mons: &[window::MonitorInfo]) -> Option<(RgbaImage, usize, ScreenRect)> {
    // Retry up to 3 times — DXGI format can fluctuate between BGRA8 and FLOAT
    // on HDR displays, especially when game is in exclusive fullscreen mode
    const MAX_RETRIES: u32 = 3;
    for attempt in 1..=MAX_RETRIES {
        match dxgi::capture_screen(idx) {
            Ok(img) if !is_image_all_black(&img) => {
                if attempt > 1 {
                    tracing::info!("DXGI monitor {}: OK (retry {})", idx, attempt);
                }
                return Some((img, idx, mons[idx].rect.clone()));
            }
            Ok(_) => {
                tracing::warn!("DXGI monitor {}: all-black data (retry {}/{})", idx, attempt, MAX_RETRIES);
            }
            Err(e) => {
                tracing::warn!("DXGI monitor {}: {} (retry {}/{})", idx, e, attempt, MAX_RETRIES);
            }
        }
        // Brief wait between retries to allow DXGI format to stabilize
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    None
}

fn find_best_monitor_for_rect(monitors: &[window::MonitorInfo], rect: &ScreenRect) -> Option<usize> {
    let mut best_idx = 0usize;
    let mut best_overlap = 0u64;
    for (i, mon) in monitors.iter().enumerate() {
        let overlap = compute_overlap_area(rect, &mon.rect);
        if overlap > best_overlap {
            best_overlap = overlap;
            best_idx = i;
        }
    }
    Some(best_idx)
}

fn compute_overlap_area(a: &ScreenRect, b: &ScreenRect) -> u64 {
    let left = a.left.max(b.left);
    let top = a.top.max(b.top);
    let right = a.right.min(b.right);
    let bottom = a.bottom.min(b.bottom);
    if left < right && top < bottom {
        ((right - left) as u64) * ((bottom - top) as u64)
    } else {
        0
    }
}

// ── Include mode: only matched windows from DXGI full-screen capture ──────

fn composite_include_mode(
    monitor_index: usize,
    windows: &[window::WindowInfo],
) -> Result<RgbaImage, String> {
    // ── Strategy: Windows Graphics Capture (OBS-style) ─────────────────
    //
    // 1. Try WGC per-window capture — reads GPU buffer directly,
    //    zero impact on window state (no z-order, no activation)
    // 2. Fallback: DXGI Desktop Duplication + crop if WGC unavailable
    //
    // This matches OBS "Game Capture" / "Window Capture" behavior:
    //   - The user never notices the screenshot being taken
    //   - Overlay windows on top of the target do NOT appear in output
    //   - DirectX/HDR content is captured correctly

    let bounds = compute_bounding_box(windows);
    let canvas_w = bounds.width() as u32;
    let canvas_h = bounds.height() as u32;

    if canvas_w == 0 || canvas_h == 0 {
        return Err("Cropped area has zero size".into());
    }

    tracing::info!(
        "Include mode: crop=({},{}) {}x{}",
        bounds.left, bounds.top, canvas_w, canvas_h
    );

    // Create black canvas sized to bounding box
    let mut canvas = ImageBuffer::from_pixel(canvas_w, canvas_h, Rgba([0, 0, 0, 255]));

    // ── Attempt WGC capture for each matched window ────────────────────
    let mut need_fallback = Vec::new();

    for win_info in windows {
        let dst_x = win_info.rect.left - bounds.left;
        let dst_y = win_info.rect.top - bounds.top;

        match crate::wgc::capture_window(win_info.hwnd) {
            Ok(window_img) => {
                if !is_image_all_black(&window_img) {
                    overlay_image(&mut canvas, &window_img, dst_x, dst_y);
                    tracing::info!(
                        "WGC capture OK: {} ({}) {}x{} at ({},{})",
                        win_info.title,
                        win_info.process_name,
                        window_img.width(),
                        window_img.height(),
                        dst_x, dst_y
                    );
                } else {
                    tracing::warn!(
                        "WGC returned empty for {} ({}), will fallback",
                        win_info.title, win_info.process_name
                    );
                    need_fallback.push(win_info);
                }
            }
            Err(e) => {
                tracing::warn!(
                    "WGC failed for {} ({}): {}, will fallback to DXGI",
                    win_info.title, win_info.process_name, e
                );
                need_fallback.push(win_info);
            }
        }
    }

    // ── Fallback: DXGI for any windows that WGC couldn't handle ────────
    if !need_fallback.is_empty() {
        tracing::info!("{} window(s) using DXGI fallback", need_fallback.len());
        
        let (screen, _screen_monitor_index, screen_monitor_rect) =
            capture_with_monitor_fallback(monitor_index, windows)?;

        let screen_w = screen.width();
        let screen_h = screen.height();

        for win_info in &need_fallback {
            let src_x = win_info.rect.left - screen_monitor_rect.left;
            let src_y = win_info.rect.top - screen_monitor_rect.top;
            let win_w = win_info.rect.width() as u32;
            let win_h = win_info.rect.height() as u32;

            if win_w == 0 || win_h == 0 || src_x < 0 || src_y < 0 { continue; }

            let dst_x = win_info.rect.left - bounds.left;
            let dst_y = win_info.rect.top - bounds.top;

            let clamped_src_x = src_x.min(screen_w as i32);
            let clamped_src_y = src_y.min(screen_h as i32);
            let copy_w = (win_w as i32).min(screen_w as i32 - clamped_src_x).max(0) as u32;
            let copy_h = (win_h as i32).min(screen_h as i32 - clamped_src_y).max(0) as u32;

            if copy_w > 0 && copy_h > 0 {
                let cropped = image::imageops::crop_imm(
                    &screen,
                    clamped_src_x as u32,
                    clamped_src_y as u32,
                    copy_w,
                    copy_h,
                );
                overlay_image(&mut canvas, &cropped.to_image(), dst_x, dst_y);
                tracing::info!(
                    "DXGI fallback: {} ({}) {}x{} at ({},{})",
                    win_info.title, win_info.process_name,
                    copy_w, copy_h, dst_x, dst_y
                );
            }
        }
    }

    Ok(canvas)
}

// ── Exclude mode: desktop minus excluded windows ─────────────────────────

fn composite_exclude_mode(
    monitor_rect: &ScreenRect,
    windows: &[window::WindowInfo],
) -> Result<RgbaImage, String> {
    // Full monitor rect as canvas
    let mut canvas = draw_desktop_background(monitor_rect)?;

    // Draw all non-excluded windows onto desktop background
    for win_info in windows {
        match capture_single_window(win_info) {
            Ok(window_img) => {
                let x = win_info.rect.left - monitor_rect.left;
                let y = win_info.rect.top - monitor_rect.top;
                overlay_image(&mut canvas, &window_img, x, y);
            }
            Err(e) => {
                tracing::warn!(
                    "Failed to capture window {} ({}): {}",
                    win_info.title, win_info.process_name, e
                );
            }
        }
    }

    Ok(canvas)
}

// ── Desktop background ───────────────────────────────────────────────

/// Compute the bounding box that contains all matched windows
fn compute_bounding_box(windows: &[window::WindowInfo]) -> ScreenRect {
    let mut bounds = ScreenRect {
        left: i32::MAX,
        top: i32::MAX,
        right: i32::MIN,
        bottom: i32::MIN,
    };

    for w in windows {
        if !w.rect.is_empty() {
            bounds.left = bounds.left.min(w.rect.left);
            bounds.top = bounds.top.min(w.rect.top);
            bounds.right = bounds.right.max(w.rect.right);
            bounds.bottom = bounds.bottom.max(w.rect.bottom);
        }
    }

    // Safety fallback if no valid rects found
    if bounds.is_empty() {
        return ScreenRect {
            left: 0,
            top: 0,
            right: 1,
            bottom: 1,
        };
    }

    bounds
}

fn draw_desktop_background(monitor_rect: &ScreenRect) -> Result<RgbaImage, String> {
    let width = monitor_rect.width() as u32;
    let height = monitor_rect.height() as u32;

    // Get desktop solid color
    let desktop_color = unsafe { GetSysColor(COLOR_DESKTOP) };
    let r = (desktop_color & 0xFF) as u8;
    let g = ((desktop_color >> 8) & 0xFF) as u8;
    let b = ((desktop_color >> 16) & 0xFF) as u8;
    let bg_color = Rgba([r, g, b, 255]);

    let mut canvas = ImageBuffer::from_pixel(width, height, bg_color);

    // Try to load and draw wallpaper
    if let Some(wallpaper_path) = get_wallpaper_path() {
        match load_wallpaper(&wallpaper_path, width, height) {
            Ok(wallpaper_img) => {
                let wp_w = wallpaper_img.width();
                let wp_h = wallpaper_img.height();
                let x = if wp_w < width { (width - wp_w) as i64 / 2 } else { 0 };
                let y = if wp_h < height { (height - wp_h) as i64 / 2 } else { 0 };
                image::imageops::overlay(&mut canvas, &wallpaper_img, x, y);
                tracing::debug!("Loaded wallpaper from {}", wallpaper_path);
            }
            Err(e) => {
                tracing::debug!("Could not load wallpaper: {}", e);
            }
        }
    }

    Ok(canvas)
}

fn get_wallpaper_path() -> Option<String> {
    unsafe {
        let mut buf = [0u16; 260];
        let result = SystemParametersInfoW(
            SPI_GETDESKWALLPAPER,
            buf.len() as u32,
            buf.as_mut_ptr() as *mut c_void,
            0,
        );

        if result == 0 {
            return None;
        }

        let len = buf.iter().position(|&c| c == 0).unwrap_or(0);
        if len == 0 {
            return None;
        }

        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

fn load_wallpaper(
    path: &str,
    target_width: u32,
    target_height: u32,
) -> Result<DynamicImage, String> {
    let img = image::open(path).map_err(|e| format!("Failed to open wallpaper: {e}"))?;

    let style = get_wallpaper_style();

    match style {
        WallpaperStyle::Stretched => Ok(img
            .resize_exact(target_width, target_height, image::imageops::FilterType::Lanczos3)
            .into()),
        WallpaperStyle::Fit => {
            let (w, h) = fit_dimensions(img.width(), img.height(), target_width, target_height);
            Ok(img
                .resize_exact(w, h, image::imageops::FilterType::Lanczos3)
                .into())
        }
        WallpaperStyle::Fill => {
            let (w, h) = fill_dimensions(img.width(), img.height(), target_width, target_height);
            let mut resized = img.resize_exact(w, h, image::imageops::FilterType::Lanczos3);
            let x = (resized.width() - target_width) / 2;
            let y = (resized.height() - target_height) / 2;
            Ok(resized.crop(x, y, target_width, target_height).into())
        }
        WallpaperStyle::Tiled => {
            let mut canvas = RgbaImage::new(target_width, target_height);
            for ty in (0..target_height).step_by(img.height() as usize) {
                for tx in (0..target_width).step_by(img.width() as usize) {
                    image::imageops::overlay(&mut canvas, &img, tx as i64, ty as i64);
                }
            }
            Ok(DynamicImage::ImageRgba8(canvas))
        }
        WallpaperStyle::Centered | WallpaperStyle::Unknown => Ok(img.into()),
    }
}

enum WallpaperStyle {
    Centered,
    Stretched,
    Fit,
    Fill,
    Tiled,
    Unknown,
}

fn get_wallpaper_style() -> WallpaperStyle {
    use winreg::enums::*;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let Ok(desktop) = hkcu.open_subkey(r"Control Panel\Desktop") else {
        return WallpaperStyle::Unknown;
    };

    let wallpaper_style: Result<String, _> = desktop.get_value("WallpaperStyle");
    let tile_wallpaper: Result<String, _> = desktop.get_value("TileWallpaper");

    let style_str = wallpaper_style.unwrap_or_default();
    let tile_str = tile_wallpaper.unwrap_or_default();

    match style_str.as_str() {
        "2" => WallpaperStyle::Stretched,
        "6" => WallpaperStyle::Fit,
        "10" => WallpaperStyle::Fill,
        "0" if tile_str == "1" => WallpaperStyle::Tiled,
        "0" => WallpaperStyle::Centered,
        _ => WallpaperStyle::Unknown,
    }
}

fn fit_dimensions(w: u32, h: u32, tw: u32, th: u32) -> (u32, u32) {
    let scale_w = tw as f64 / w as f64;
    let scale_h = th as f64 / h as f64;
    let scale = scale_w.min(scale_h);
    ((w as f64 * scale) as u32, (h as f64 * scale) as u32)
}

fn fill_dimensions(w: u32, h: u32, tw: u32, th: u32) -> (u32, u32) {
    let scale_w = tw as f64 / w as f64;
    let scale_h = th as f64 / h as f64;
    let scale = scale_w.max(scale_h);
    ((w as f64 * scale) as u32, (h as f64 * scale) as u32)
}

// ── Single window capture ────────────────────────────────────────────

fn capture_single_window(win_info: &window::WindowInfo) -> Result<RgbaImage, String> {
    let hwnd = win_info.hwnd as HWND;
    let width = win_info.rect.width() as u32;
    let height = win_info.rect.height() as u32;

    if width == 0 || height == 0 {
        return Err("Window has zero size".into());
    }

    // Clamp to reasonable size
    if width > 7680 || height > 4320 {
        return Err(format!("Window too large: {}x{}", width, height));
    }

    unsafe {
        let hdc_screen = GetDC(ptr::null_mut());
        if hdc_screen.is_null() {
            return Err("GetDC failed".into());
        }

        let hdc_mem = CreateCompatibleDC(hdc_screen);
        if hdc_mem.is_null() {
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("CreateCompatibleDC failed".into());
        }

        // Create DIB section for direct pixel access
        let bmi = BITMAPINFO {
            bmiHeader: BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width as i32,
                biHeight: -(height as i32), // top-down bitmap
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: width * height * 4,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            },
            bmiColors: [RGBQUAD {
                rgbBlue: 0,
                rgbGreen: 0,
                rgbRed: 0,
                rgbReserved: 0,
            }],
        };

        let mut ppv_bits: *mut c_void = ptr::null_mut();
        let hbitmap = CreateDIBSection(
            hdc_mem,
            &bmi,
            DIB_RGB_COLORS,
            &mut ppv_bits,
            ptr::null_mut(),
            0,
        );

        if hbitmap.is_null() || ppv_bits.is_null() {
            DeleteDC(hdc_mem);
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("CreateDIBSection failed".into());
        }

        let old_bmp = SelectObject(hdc_mem, hbitmap);

        // Try PrintWindow with PW_RENDERFULLCONTENT first
        let mut captured = false;

        if PrintWindow(hwnd, hdc_mem, PW_RENDERFULLCONTENT) != 0 {
            captured = true;
        } else if PrintWindow(hwnd, hdc_mem, 0) != 0 {
            captured = true;
            tracing::debug!("PrintWindow succeeded without PW_RENDERFULLCONTENT");
        } else {
            // Last resort: try BitBlt from window DC
            let hdc_window = GetWindowDC(hwnd);
            if !hdc_window.is_null() {
                if BitBlt(
                    hdc_mem,
                    0,
                    0,
                    width as i32,
                    height as i32,
                    hdc_window,
                    0,
                    0,
                    SRCCOPY | CAPTUREBLT,
                ) != 0
                {
                    captured = true;
                    tracing::debug!("BitBlt from window DC succeeded");
                }
                ReleaseDC(hwnd, hdc_window);
            }
        }

        if !captured {
            SelectObject(hdc_mem, old_bmp);
            DeleteObject(hbitmap);
            DeleteDC(hdc_mem);
            ReleaseDC(ptr::null_mut(), hdc_screen);
            return Err("All window capture methods failed".into());
        }

        // Read pixel data from the DIB section
        let pixel_count = (width * height * 4) as usize;
        let pixels = std::slice::from_raw_parts(ppv_bits as *const u8, pixel_count);
        let mut img_data = pixels.to_vec();

        // Convert BGRA to RGBA
        for chunk in img_data.chunks_exact_mut(4) {
            chunk.swap(0, 2); // B <-> R
            chunk[3] = 255;    // Force opaque
        }

        // Cleanup GDI objects
        SelectObject(hdc_mem, old_bmp);
        DeleteObject(hbitmap);
        DeleteDC(hdc_mem);
        ReleaseDC(ptr::null_mut(), hdc_screen);

        RgbaImage::from_raw(width, height, img_data)
            .ok_or_else(|| "Failed to create image from raw pixel data".into())
    }
}

// ── Overlay image onto canvas ────────────────────────────────────────

fn overlay_image(canvas: &mut RgbaImage, overlay: &RgbaImage, x: i32, y: i32) {
    let canvas_w = canvas.width() as i32;
    let canvas_h = canvas.height() as i32;

    for (ox, oy, pixel) in overlay.enumerate_pixels() {
        let cx = x + ox as i32;
        let cy = y + oy as i32;

        if cx >= 0 && cx < canvas_w && cy >= 0 && cy < canvas_h {
            let canvas_pixel = canvas.get_pixel(cx as u32, cy as u32);
            let blended = alpha_blend(*canvas_pixel, *pixel);
            canvas.put_pixel(cx as u32, cy as u32, blended);
        }
    }
}

fn alpha_blend(bg: Rgba<u8>, fg: Rgba<u8>) -> Rgba<u8> {
    let alpha = fg.0[3] as f32 / 255.0;
    let inv_alpha = 1.0 - alpha;

    Rgba([
        (fg.0[0] as f32 * alpha + bg.0[0] as f32 * inv_alpha) as u8,
        (fg.0[1] as f32 * alpha + bg.0[1] as f32 * inv_alpha) as u8,
        (fg.0[2] as f32 * alpha + bg.0[2] as f32 * inv_alpha) as u8,
        255,
    ])
}

// ── Bitmap to image conversion (for full screen capture) ─────────────

fn bitmap_to_image(
    hdc: HDC,
    hbitmap: HBITMAP,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    let bmi = BITMAPINFO {
        bmiHeader: BITMAPINFOHEADER {
            biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
            biWidth: width as i32,
            biHeight: -(height as i32), // top-down
            biPlanes: 1,
            biBitCount: 32,
            biCompression: BI_RGB,
            biSizeImage: width * height * 4,
            biXPelsPerMeter: 0,
            biYPelsPerMeter: 0,
            biClrUsed: 0,
            biClrImportant: 0,
        },
        bmiColors: [RGBQUAD {
            rgbBlue: 0,
            rgbGreen: 0,
            rgbRed: 0,
            rgbReserved: 0,
        }],
    };

    let mut img_data = vec![0u8; (width * height * 4) as usize];

    let result = unsafe {
        GetDIBits(
            hdc,
            hbitmap,
            0,
            height,
            img_data.as_mut_ptr() as *mut c_void,
            &bmi as *const BITMAPINFO as *mut BITMAPINFO,
            DIB_RGB_COLORS,
        )
    };

    if result == 0 {
        return Err("GetDIBits failed".into());
    }

    // Convert BGRA to RGBA
    for chunk in img_data.chunks_exact_mut(4) {
        chunk.swap(0, 2); // B <-> R
        chunk[3] = 255; // Force opaque
    }

    RgbaImage::from_raw(width, height, img_data)
        .ok_or_else(|| "Failed to create image from raw pixel data".into())
}

// ── Image encoding ───────────────────────────────────────────────────

fn encode_image(img: &RgbaImage, format: ImageFormat) -> Result<Vec<u8>, String> {
    let mut buf = std::io::Cursor::new(Vec::new());
    match format {
        ImageFormat::Png => img
            .write_to(&mut buf, image::ImageFormat::Png)
            .map_err(|e| format!("PNG encoding failed: {e}"))?,
        ImageFormat::Jpeg => {
            let rgb = DynamicImage::ImageRgba8(img.clone()).to_rgb8();
            rgb.write_to(&mut buf, image::ImageFormat::Jpeg)
                .map_err(|e| format!("JPEG encoding failed: {e}"))?;
        }
    }
    Ok(buf.into_inner())
}

pub fn image_to_base64(data: &[u8]) -> String {
    base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data)
}

pub fn format_mime_type(format: ImageFormat) -> &'static str {
    match format {
        ImageFormat::Png => "image/png",
        ImageFormat::Jpeg => "image/jpeg",
    }
}
