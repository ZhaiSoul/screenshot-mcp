use crate::window;
use image::RgbaImage;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

/// Capture screen using DXGI Desktop Duplication (handles HDR correctly)
pub fn capture_screen(monitor_index: usize) -> Result<RgbaImage, String> {
    unsafe { capture_screen_inner(monitor_index) }
}

unsafe fn capture_screen_inner(monitor_index: usize) -> Result<RgbaImage, String> {
    let (device, context) = create_device()?;

    let dxgi_device: IDXGIDevice = device
        .cast()
        .map_err(|e| format!("Failed to cast to IDXGIDevice: {e}"))?;
    let adapter = dxgi_device
        .GetAdapter()
        .map_err(|e| format!("Failed to get adapter: {e}"))?;

    let output = find_output_by_monitor_index(&adapter, monitor_index)?;

    // Create output duplication
    let output1: IDXGIOutput1 = output
        .cast()
        .map_err(|e| format!("Failed to cast to IDXGIOutput1: {e}"))?;
    let duplication = output1
        .DuplicateOutput(&device)
        .map_err(|e| format!("DuplicateOutput failed: {e}"))?;

    // Acquire frame with retries
    let mut texture = None;
    let mut frame_info = std::mem::zeroed::<DXGI_OUTDUPL_FRAME_INFO>();

    for attempt in 0..5u32 {
        let mut resource: Option<IDXGIResource> = None;
        match duplication.AcquireNextFrame(500, &mut frame_info, &mut resource) {
            Ok(()) => {
                if let Some(res) = resource {
                    let tex: ID3D11Texture2D = res
                        .cast()
                        .map_err(|e| format!("Failed to cast to ID3D11Texture2D: {e}"))?;
                    texture = Some(tex);
                    break;
                }
            }
            Err(e) => {
                let code = e.code().0 as u32;
                if code == 0x887A0027 {
                    tracing::debug!("AcquireNextFrame timeout, attempt {}", attempt + 1);
                    continue;
                }
                if code == 0x887A0026 {
                    return Err("DXGI access lost".into());
                }
                return Err(format!("AcquireNextFrame failed: {e}"));
            }
        }
    }

    let texture = texture.ok_or("Failed to acquire frame after 5 attempts")?;

    // Read texture data (handles both BGRA8 and FP16 formats)
    read_texture_to_image(&device, &context, &texture, &duplication)
}

fn create_device() -> Result<(ID3D11Device, ID3D11DeviceContext), String> {
    unsafe {
        let mut device = None;
        let mut context = None;

        let hr = D3D11CreateDevice(
            None,
            D3D_DRIVER_TYPE_HARDWARE,
            windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
            D3D11_CREATE_DEVICE_FLAG(0u32),
            None,
            D3D11_SDK_VERSION,
            Some(&mut device),
            None,
            Some(&mut context),
        );

        if hr.is_err() {
            let hr2 = D3D11CreateDevice(
                None,
                D3D_DRIVER_TYPE_WARP,
                windows::Win32::Foundation::HMODULE(std::ptr::null_mut()),
                D3D11_CREATE_DEVICE_FLAG(0),
                None,
                D3D11_SDK_VERSION,
                Some(&mut device),
                None,
                Some(&mut context),
            );
            if hr2.is_err() {
                return Err("Failed to create D3D11 device".into());
            }
            tracing::info!("Using WARP (software) D3D11 device");
        }

        let device = device.ok_or("D3D11 device is null")?;
        let context = context.ok_or("D3D11 context is null")?;
        Ok((device, context))
    }
}

unsafe fn find_output_by_monitor_index(
    adapter: &IDXGIAdapter,
    target_index: usize,
) -> Result<IDXGIOutput, String> {
    let monitors = window::enumerate_monitors();
    let target_monitor = monitors.get(target_index).ok_or_else(|| {
        format!("Monitor index {} out of range", target_index)
    })?;
    let target_name = &target_monitor.name;

    let mut output_index = 0u32;
    loop {
        let output = match adapter.EnumOutputs(output_index) {
            Ok(o) => o,
            Err(_) => break,
        };

        let desc = output.GetDesc()
            .map_err(|e| format!("GetDesc failed: {e}"))?;

        let output_name = String::from_utf16_lossy(
            &desc.DeviceName.iter().take_while(|&&c| c != 0).copied().collect::<Vec<u16>>(),
        );

        if output_name == *target_name {
            return Ok(output);
        }
        output_index += 1;
    }

    adapter
        .EnumOutputs(target_index as u32)
        .map_err(|e| format!("Failed to get DXGI output {}: {e}", target_index))
}

unsafe fn read_texture_to_image(
    device: &ID3D11Device,
    context: &ID3D11DeviceContext,
    src_texture: &ID3D11Texture2D,
    duplication: &IDXGIOutputDuplication,
) -> Result<RgbaImage, String> {
    let mut desc = std::mem::zeroed::<D3D11_TEXTURE2D_DESC>();
    src_texture.GetDesc(&mut desc);

    let width = desc.Width as u32;
    let height = desc.Height as u32;

    tracing::debug!("Captured texture: {}x{}, format={:?}", width, height, desc.Format);

    // Use same staging format for CopyResource compatibility
    let staging_format = desc.Format;

    let staging_desc = D3D11_TEXTURE2D_DESC {
        Width: width,
        Height: height,
        MipLevels: 1,
        ArraySize: 1,
        Format: staging_format,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_STAGING,
        BindFlags: 0,
        CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
        MiscFlags: 0,
    };

    let mut staging_texture: Option<ID3D11Texture2D> = None;
    device
        .CreateTexture2D(&staging_desc, None, Some(&mut staging_texture))
        .map_err(|e| format!("CreateTexture2D staging failed: {e}"))?;
    let staging_texture = staging_texture.ok_or("Staging texture is null")?;

    context.CopyResource(&staging_texture, src_texture);

    let mut mapped = std::mem::zeroed::<D3D11_MAPPED_SUBRESOURCE>();
    context
        .Map(&staging_texture, 0, D3D11_MAP_READ, 0, Some(&mut mapped))
        .map_err(|e| format!("Map staging texture failed: {e}"))?;

    let row_pitch = mapped.RowPitch as usize;
    let pixels = std::slice::from_raw_parts(
        mapped.pData as *const u8,
        row_pitch * height as usize,
    );

    let image = match staging_format {
        DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R8G8B8A8_UNORM => {
            convert_bgra8_to_rgba(pixels, row_pitch, width, height)?
        }
        DXGI_FORMAT_R16G16B16A16_FLOAT => {
            convert_float16_to_rgba(pixels, row_pitch, width, height)?
        }
        _ => {
            context.Unmap(&staging_texture, 0);
            duplication.ReleaseFrame().ok();
            return Err(format!("Unsupported format: {:?}", staging_format));
        }
    };

    context.Unmap(&staging_texture, 0);
    duplication.ReleaseFrame().ok();
    Ok(image)
}

// ── Format converters ─────────────────────────────────────────────────────────

fn convert_bgra8_to_rgba(
    pixels: &[u8],
    row_pitch: usize,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    // Quick diagnostics (sample 9 points)
    let samples = [
        (0usize, 0),
        (width as usize / 2, 0),
        (width as usize - 1, 0),
        (0, height as usize / 2),
        (width as usize / 2, height as usize / 2),
        (width as usize - 1, height as usize / 2),
        (0, height as usize - 1),
        (width as usize / 2, height as usize - 1),
        (width as usize - 1, height as usize - 1),
    ];

    let mut sum_r = 0u64;
    let mut sum_g = 0u64;
    let mut sum_b = 0u64;
    let mut min_bright = 255u8;
    let mut max_bright = 0u8;

    for &(sx, sy) in &samples {
        let si = sy * row_pitch + sx * 4;
        if si + 2 < pixels.len() {
            let b = pixels[si];
            let g = pixels[si + 1];
            let r = pixels[si + 2];
            let bright = ((r as u32 + g as u32 + b as u32) / 3) as u8;
            sum_r += r as u64;
            sum_g += g as u64;
            sum_b += b as u64;
            min_bright = min_bright.min(bright);
            max_bright = max_bright.max(bright);
        }
    }

    tracing::info!(
        "DXGI pixel diag: avg=({},{},{}) min={} max={}",
        sum_r / samples.len() as u64,
        sum_g / samples.len() as u64,
        sum_b / samples.len() as u64,
        min_bright, max_bright
    );

    // Determine if the image needs HDR→SDR tonemap correction.
    //
    // On HDR monitors, DXGI BGRA8 data from Desktop Duplication is typically
    // already tone-mapped by Windows (scRGB → sRGB). However some drivers
    // return data in a different gamma curve that appears "washed out".
    //
    // Detection heuristic:
    //   - If min_brightness > 50 AND the image is very bright overall → likely
    //     needs inverse tonemap to recover contrast.
    //   - Otherwise use the data as-is (already correct sRGB).
    let avg_bright = ((sum_r + sum_g + sum_b) / 3) / (samples.len() as u64);
    let needs_tonemap_fix = min_bright > 50 && avg_bright > 140;

    tracing::info!("HDR tonemap correction needed: {} (min={}, avg={})", needs_tonemap_fix, min_bright, avg_bright);

    let mut img_data = vec![0u8; (width * height * 4) as usize];
    for y in 0..height as usize {
        let src_off = y * row_pitch;
        let dst_off = y * width as usize * 4;
        for x in 0..width as usize {
            let si = src_off + x * 4;
            let di = dst_off + x * 4;

            if needs_tonemap_fix {
                // Apply Reinhard-style inverse tonemap to restore HDR range
                // that was compressed into SDR by Windows' tone-mapper
                let r_f = pixels[si + 2] as f32 / 255.0;
                let g_f = pixels[si + 1] as f32 / 255.0;
                let b_f = pixels[si] as f32 / 255.0;

                img_data[di]     = (inverse_tonemap(r_f) * 255.0).clamp(0.0, 255.0) as u8; // R
                img_data[di + 1] = (inverse_tonemap(g_f) * 255.0).clamp(0.0, 255.0) as u8; // G
                img_data[di + 2] = (inverse_tonemap(b_f) * 255.0).clamp(0.0, 255.0) as u8; // B
            } else {
                // Direct BGRA→RGBA copy (data is already correct sRGB)
                img_data[di]     = pixels[si + 2];     // R
                img_data[di + 1] = pixels[si + 1];     // G
                img_data[di + 2] = pixels[si];         // B
            }
            img_data[di + 3] = 255; // A
        }
    }

    RgbaImage::from_raw(width, height, img_data).ok_or_else(|| "Failed to create RgbaImage".into())
}

/// Inverse Reinhard tonemap: expands compressed SDR range back toward full dynamic range.
/// This counteracts Windows' aggressive HDR→SDR compression on DXGI BGRA8 output.
fn inverse_tonemap(c: f32) -> f32 {
    let v = c.powf(0.85);
    (v * 1.15).clamp(0.0, 1.0)
}

// ── FP16 (R16G16B16A16_FLOAT) conversion with HDR tonemapping ──────────

/// Convert raw FP16 pixel data to RGBA8 image with ACES tonemapping.
/// Each pixel is 4 × 16-bit (BGRA order in memory).
fn convert_float16_to_rgba(
    pixels: &[u8],
    row_pitch: usize,
    width: u32,
    height: u32,
) -> Result<RgbaImage, String> {
    // Sample diagnostics
    let samples = [
        (0usize, 0),
        (width as usize / 2, 0),
        (width as usize - 1, 0),
        (0, height as usize / 2),
        (width as usize / 2, height as usize / 2),
        (width as usize - 1, height as usize / 2),
        (0, height as usize - 1),
        (width as usize / 2, height as usize - 1),
        (width as usize - 1, height as usize - 1),
    ];

    let mut max_lum = 0.0_f32;
    let mut nonzero_count = 0u32;

    for &(sx, sy) in &samples {
        let si = sy * row_pitch + sx * 8;
        if si + 6 < pixels.len() {
            let b = f16_to_f32(&pixels[si..si + 2]);
            let g = f16_to_f32(&pixels[si + 2..si + 4]);
            let r = f16_to_f32(&pixels[si + 4..si + 6]);
            let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
            if lum > 0.001 { nonzero_count += 1; }
            max_lum = max_lum.max(lum);
        }
    }

    tracing::info!(
        "FP16 diag: max_lum={:.2}, nonzero_samples={}/{}",
        max_lum, nonzero_count, samples.len()
    );

    // If ALL samples are zero or near-zero, the FP16 data is genuinely empty
    // (driver bug where the texture isn't populated)
    if nonzero_count == 0 {
        tracing::warn!("FP16 data appears all-zero — likely driver bug");
        return Err("FP16 texture contains no valid data".into());
    }

    let mut img_data = vec![0u8; (width * height * 4) as usize];

    // First pass: find max luminance across entire image for adaptive tonemap
    let mut global_max = 0.0_f32;
    for y in (0..height as usize).step_by(16) {
        for x in (0..width as usize).step_by(16) {
            let si = y * row_pitch + x * 8;
            if si + 6 < pixels.len() {
                let r = f16_to_f32(&pixels[si + 4..si + 6]);
                let g = f16_to_f32(&pixels[si + 2..si + 4]);
                let b = f16_to_f32(&pixels[si..si + 2]);
                let lum = 0.2126 * r + 0.7152 * g + 0.0722 * b;
                global_max = global_max.max(lum);
            }
        }
    }

    tracing::info!( "FP16 global max luminance: {:.2}", global_max);

    // Second pass: convert and tonemap
    for y in 0..height as usize {
        let src_off = y * row_pitch;
        let dst_off = y * width as usize * 4;
        for x in 0..width as usize {
            let si = src_off + x * 8;
            let di = dst_off + x * 4;

            if si + 8 <= pixels.len() {
                // FP16 is stored as BGRA
                let b = f16_to_f32(&pixels[si..si + 2]);
                let g = f16_to_f32(&pixels[si + 2..si + 4]);
                let r = f16_to_f32(&pixels[si + 4..si + 6]);
                // Alpha — clamp to [0, 1] range
                let a_raw = f16_to_f32(&pixels[si + 6..si + 8]);

                // ACES tonemapping for HDR→SDR conversion
                let (tr, tg, tb) = aces_tonemap(r, g, b);

                img_data[di]     = (tr * 255.0).clamp(0.0, 255.0) as u8; // R
                img_data[di + 1] = (tg * 255.0).clamp(0.0, 255.0) as u8; // G
                img_data[di + 2] = (tb * 255.0).clamp(0.0, 255.0) as u8; // B
                img_data[di + 3] = (a_raw.clamp(0.0, 1.0) * 255.0) as u8;   // A
            }
        }
    }

    RgbaImage::from_raw(width, height, img_data).ok_or_else(|| "Failed to create RgbaImage".into())
}

/// Decode an IEEE 754 half-precision (16-bit) float to f32.
/// Memory layout: little-endian bytes [lo, hi].
#[inline]
fn f16_to_f32(bytes: &[u8]) -> f32 {
    let h = (bytes[0] as u16) | ((bytes[1] as u16) << 8);
    let sign = ((h >> 15) & 1) as u32;
    let exponent = ((h >> 10) & 0x1F) as u32;
    let mantissa = (h & 0x3FF) as u32;

    if exponent == 0 {
        // Zero or subnormal
        if mantissa == 0 {
            f32::from_bits(sign << 31)
        } else {
            // Subnormal: implicit leading bit is 0
            let m = mantissa as f32 / (1 << 10) as f32;
            f32::from_bits(sign << 31) * m * 2.0f32.powi(-14)
        }
    } else if exponent == 31 {
        // Infinity or NaN
        f32::NAN
    } else {
        // Normal: add implicit leading 1
        let e = exponent as i32 - 15;
        let m = (mantissa | 0x400) as f32 / 1024.0;
        f32::from_bits(sign << 31) * m * 2.0f32.powi(e)
    }
}

/// ACES filmic tonemapping approximation.
/// Maps HDR values (possibly > 1.0) to SDR [0, 1] range with pleasing contrast.
fn aces_tonemap(r: f32, g: f32, b: f32) -> (f32, f32, f32) {
    // Input linear → Output sRGB via ACES approximation
    fn aces_single(x: f32) -> f32 {
        // Simplified ACES (source: Krzysztof Narkowicz)
        let a = 2.51_f32;
        let b = 0.03_f32;
        let c = 2.43_f32;
        let d = 0.59_f32;
        let e = 0.14_f32;

        // Clamp negative values (can happen from FP16 noise)
        let x = x.max(0.0);

        // Apply exposure adjustment for typical HDR content
        // Most game HDR content has peak brightness around 100-1000 nits
        // We scale down so that ~1.0 in scRGB maps to reasonable display brightness
        let x_adjusted = x * 0.9;

        (x_adjusted * (a * x_adjusted + b)) / (x_adjusted * (c * x_adjusted + d) + e)
    }

    (aces_single(r), aces_single(g), aces_single(b))
}
