// Windows Graphics Capture (WGC) — HDR-aware window capture
//
// Uses `kscreenshot` crate wrapping Windows.Graphics.Capture.
// HDR handling is built-in: detects HDR color space, reads SDR White Level
// from DisplayConfig, converts monitor gamut → sRGB/Rec.709, applies gamma.
//
// Key properties:
//   - Captures specific HWND's GPU-rendered content directly
//   - Does NOT modify window state (no z-order, activation, minimize)
//   - Works for DirectX/OpenGL/Vulkan applications
//   - Requires Windows 10 1903+ (build 18362)

use image::RgbaImage;
use kscreenshot::{CaptureRequest, ScreenCaptureManager, WindowId};

/// Capture a single window using Windows Graphics Capture.
///
/// Reads GPU buffer directly, zero impact on any window state.
/// HDR content is automatically tone-mapped to sRGB BGRA8.
pub fn capture_window(hwnd: usize) -> Result<RgbaImage, String> {
    tracing::info!("WGC: capturing hwnd={:#x}", hwnd);

    let manager = ScreenCaptureManager::new()
        .map_err(|e| format!("WGC: create manager failed: {}", e))?;

    let window_id = WindowId(hwnd as u64);

    let result = manager.capture(CaptureRequest::window(window_id))
        .map_err(|e| format!("WGC: capture failed: {}", e))?;

    // kscreenshot outputs BGRA8; convert to RGBA
    let rgba_data = result.source.to_rgba();

    RgbaImage::from_raw(result.source.width, result.source.height, rgba_data)
        .ok_or_else(|| "WGC: RgbaImage creation failed".into())
}
