// Windows Graphics Capture (WGC) — OBS-style window capture
//
// Uses `windows-capture` crate wrapping Windows.Graphics.Capture,
// the same underlying API OBS uses for "Game Capture" / "Window Capture".
//
// Key properties:
//   - Captures specific HWND's GPU-rendered content directly
//   - Does NOT modify window state (no z-order, activation, minimize)
//   - Works for DirectX/OpenGL/Vulkan applications
//   - Requires Windows 10 1903+ (build 18362)

use image::RgbaImage;
use std::sync::{Arc, Mutex};
use windows_capture::{
    capture::{Context, GraphicsCaptureApiHandler},
    frame::Frame,
    graphics_capture_api::InternalCaptureControl,
    settings::{
        ColorFormat, CursorCaptureSettings, DirtyRegionSettings, DrawBorderSettings,
        MinimumUpdateIntervalSettings, SecondaryWindowSettings, Settings,
    },
    window::Window,
};

/// Shared captured frame data between callback thread and caller
struct CapturedFrame {
    width: u32,
    height: u32,
    data: Vec<u8>,
}

type FrameSlot = Arc<Mutex<Option<CapturedFrame>>>;

/// Capture a single window using Windows Graphics Capture.
///
/// This is OBS-style capture: reads GPU buffer directly, zero impact on
/// any window state. No z-order change, no activation, no minimization.
pub fn capture_window(hwnd: usize) -> Result<RgbaImage, String> {
    tracing::info!("WGC: capturing hwnd={:#x}", hwnd);

    // Create Window from raw HWND
    let target = Window::from_raw_hwnd(hwnd as *mut std::ffi::c_void);

    // Shared result slot — passed through Settings Flags → Context.flags → handler
    let slot: FrameSlot = Arc::new(Mutex::new(None));

    // Build capture settings
    let settings = Settings::new(
        target,
        CursorCaptureSettings::WithoutCursor,
        DrawBorderSettings::WithoutBorder,
        SecondaryWindowSettings::Default,
        MinimumUpdateIntervalSettings::Default,
        DirtyRegionSettings::Default,
        ColorFormat::Bgra8,
        slot.clone(), // custom flags: delivered to handler via Context
    );

    // Start capture on background thread, returns CaptureControl handle
    let control = SingleFrameHandler::start_free_threaded(settings)
        .map_err(|e| format!("WGC: start_capture failed: {}", e))?;

    // Poll for result with timeout
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        {
            let g = slot.lock().unwrap();
            if g.is_some() {
                break;
            }
        }
        if std::time::Instant::now() > deadline {
            tracing::warn!("WGC: timeout after 3s");
            let _ = control.stop();
            return Err("WGC: timed out waiting for frame".into());
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }

    let _ = control.stop();

    // Extract frame
    let cf = slot.lock().unwrap().take().unwrap();
    bgra_to_rgba(&cf.data, cf.width, cf.height)
}

// ── Handler: receives frames from WGC callback ────────────────────────

struct SingleFrameHandler {
    slot: FrameSlot,
}

impl GraphicsCaptureApiHandler for SingleFrameHandler {
    type Flags = FrameSlot;
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn new(ctx: Context<Self::Flags>) -> Result<Self, Self::Error> {
        Ok(Self { slot: ctx.flags })
    }

    fn on_frame_arrived(
        &mut self,
        frame: &mut Frame,
        ctrl: InternalCaptureControl,
    ) -> Result<(), Self::Error> {
        let w = frame.width();
        let h = frame.height();
        tracing::debug!("WGC: frame arrived {}x{}", w, h);

        // Read BGRA pixel buffer
        let mut buf = frame.buffer()?;
        let pixels = if buf.has_padding() {
            buf.as_nopadding_buffer()
                .map(|b| b.to_vec())
                .unwrap_or_else(|_| buf.as_raw_buffer().to_vec())
        } else {
            buf.as_raw_buffer().to_vec()
        };

        *self.slot.lock().unwrap() = Some(CapturedFrame {
            width: w,
            height: h,
            data: pixels,
        });

        ctrl.stop(); // one frame is enough
        Ok(())
    }
}

// ── BGRA → RGBA conversion ────────────────────────────────────────────

fn bgra_to_rgba(bgra: &[u8], width: u32, height: u32) -> Result<RgbaImage, String> {
    let ppr = width as usize;           // pixels per row
    let stride = if !bgra.is_empty() && height > 0 {
        bgra.len() / height as usize    // actual bytes per row (may have padding)
    } else {
        ppr * 4
    };
    let mut rgba = Vec::with_capacity(ppr * height as usize * 4);

    for y in 0..height as usize {
        for x in 0..ppr {
            let off = y * stride + x * 4;
            if off.saturating_add(3) < bgra.len() {
                rgba.push(bgra[off + 2]);     // R
                rgba.push(bgra[off + 1]);     // G
                rgba.push(bgra[off]);         // B
                rgba.push(255);               // A
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 255]);
            }
        }
    }

    RgbaImage::from_raw(width, height, rgba).ok_or_else(|| "WGC: RgbaImage creation failed".into())
}
