//! The HUD overlay, drawn into the top right corner of every frame shown.
//!
//! Includes an in-depth FPS & frame-time measurement tool, an interactive
//! FPS limit/ceiling selector (30, 60, 120, Unlimited) toggled with F1, and
//! a native PS5 DualSense / Xbox / Keyboard input mapping cheat-sheet toggled with F2.
//!
//! Drawn on the CPU with a 5x7 bitmap font directly into a shared buffer,
//! copied onto the swap chain's back buffer before presentation.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// Measured FPS in tenths (e.g. 300 = 30.0 fps).
static TENTHS: AtomicU32 = AtomicU32::new(300);
/// Measured frame time in tenths of a ms (e.g. 333 = 33.3 ms).
static MS_TENTHS: AtomicU32 = AtomicU32::new(333);
/// Average draw calls per frame.
static DRAWS: AtomicU32 = AtomicU32::new(21);
/// Whether full controller mapping details are expanded.
static EXPANDED: AtomicBool = AtomicBool::new(true);

/// Whether the overlay is drawn.
fn on() -> bool {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ON.get_or_init(|| std::env::var_os("VICTORY_FPS_COUNTER").is_none_or(|v| v != "0"))
}

/// The frame rate just measured.
pub(crate) fn set(fps: f64) {
    if fps.is_finite() && fps >= 0.0 {
        TENTHS.store((fps * 10.0).round().min(99_999.0) as u32, Ordering::Relaxed);
    }
}

/// Update full performance metrics.
pub(crate) fn set_metrics(fps: f64, ms: f64, draws: f64) {
    set(fps);
    if ms.is_finite() && ms >= 0.0 {
        MS_TENTHS.store((ms * 10.0).round().min(99_999.0) as u32, Ordering::Relaxed);
    }
    if draws.is_finite() && draws >= 0.0 {
        DRAWS.store(draws.round().min(99_999.0) as u32, Ordering::Relaxed);
    }
}

/// Check keyboard hotkeys for toggling FPS ceiling and HUD detail.
fn check_keys() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{GetAsyncKeyState, VK_F1, VK_F2};
        static F1_DOWN: AtomicBool = AtomicBool::new(false);
        static F2_DOWN: AtomicBool = AtomicBool::new(false);

        // SAFETY: GetAsyncKeyState reads async keyboard state without side effects.
        let f1 = unsafe { (GetAsyncKeyState(VK_F1.0 as i32) as u16 & 0x8000) != 0 };
        if f1 && !F1_DOWN.swap(true, Ordering::Relaxed) {
            crate::frame::cycle_limit();
        } else if !f1 {
            F1_DOWN.store(false, Ordering::Relaxed);
        }

        let f2 = unsafe { (GetAsyncKeyState(VK_F2.0 as i32) as u16 & 0x8000) != 0 };
        if f2 && !F2_DOWN.swap(true, Ordering::Relaxed) {
            let prev = EXPANDED.load(Ordering::Relaxed);
            EXPANDED.store(!prev, Ordering::Relaxed);
        } else if !f2 {
            F2_DOWN.store(false, Ordering::Relaxed);
        }
    }
}

/// A glyph's seven rows, five dots each (bit 4 is the left one).
fn glyph(c: char) -> [u8; 7] {
    let c = c.to_ascii_uppercase();
    match c {
        '0' => [0b01110, 0b10001, 0b10011, 0b10101, 0b11001, 0b10001, 0b01110],
        '1' => [0b00100, 0b01100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        '2' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0b01000, 0b11111],
        '3' => [0b11110, 0b00001, 0b00001, 0b01110, 0b00001, 0b00001, 0b11110],
        '4' => [0b00010, 0b00110, 0b01010, 0b10010, 0b11111, 0b00010, 0b00010],
        '5' => [0b11111, 0b10000, 0b11110, 0b00001, 0b00001, 0b10001, 0b01110],
        '6' => [0b00110, 0b01000, 0b10000, 0b11110, 0b10001, 0b10001, 0b01110],
        '7' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b01000, 0b01000],
        '8' => [0b01110, 0b10001, 0b10001, 0b01110, 0b10001, 0b10001, 0b01110],
        '9' => [0b01110, 0b10001, 0b10001, 0b01111, 0b00001, 0b00010, 0b01100],
        'A' => [0b01110, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'B' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10001, 0b10001, 0b11110],
        'C' => [0b01110, 0b10001, 0b10000, 0b10000, 0b10000, 0b10001, 0b01110],
        'D' => [0b11110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b11110],
        'E' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b11111],
        'F' => [0b11111, 0b10000, 0b10000, 0b11110, 0b10000, 0b10000, 0b10000],
        'G' => [0b01110, 0b10001, 0b10000, 0b10111, 0b10001, 0b10001, 0b01110],
        'H' => [0b10001, 0b10001, 0b10001, 0b11111, 0b10001, 0b10001, 0b10001],
        'I' => [0b01110, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b01110],
        'J' => [0b00001, 0b00001, 0b00001, 0b00001, 0b10001, 0b10001, 0b01110],
        'K' => [0b10001, 0b10010, 0b10100, 0b11000, 0b10100, 0b10010, 0b10001],
        'L' => [0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b10000, 0b11111],
        'M' => [0b10001, 0b11011, 0b10101, 0b10001, 0b10001, 0b10001, 0b10001],
        'N' => [0b10001, 0b11001, 0b10101, 0b10011, 0b10001, 0b10001, 0b10001],
        'O' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'P' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10000, 0b10000, 0b10000],
        'Q' => [0b01110, 0b10001, 0b10001, 0b10001, 0b10101, 0b10010, 0b01101],
        'R' => [0b11110, 0b10001, 0b10001, 0b11110, 0b10100, 0b10010, 0b10001],
        'S' => [0b01111, 0b10000, 0b10000, 0b01110, 0b00001, 0b00001, 0b11110],
        'T' => [0b11111, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        'U' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01110],
        'V' => [0b10001, 0b10001, 0b10001, 0b10001, 0b10001, 0b01010, 0b00100],
        'W' => [0b10001, 0b10001, 0b10001, 0b10101, 0b10101, 0b11011, 0b10001],
        'X' => [0b10001, 0b10001, 0b01010, 0b00100, 0b01010, 0b10001, 0b10001],
        'Y' => [0b10001, 0b10001, 0b01010, 0b00100, 0b00100, 0b00100, 0b00100],
        'Z' => [0b11111, 0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0b11111],
        '.' => [0, 0, 0, 0, 0, 0b01100, 0b01100],
        ':' => [0, 0b01100, 0b01100, 0, 0b01100, 0b01100, 0],
        '-' => [0, 0, 0, 0b11111, 0, 0, 0],
        '+' => [0, 0b00100, 0b00100, 0b11111, 0b00100, 0b00100, 0],
        '/' => [0b00001, 0b00010, 0b00100, 0b01000, 0b10000, 0, 0],
        '|' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0b00100],
        '[' => [0b01110, 0b01000, 0b01000, 0b01000, 0b01000, 0b01000, 0b01110],
        ']' => [0b01110, 0b00010, 0b00010, 0b00010, 0b00010, 0b00010, 0b01110],
        '(' => [0b00010, 0b00100, 0b01000, 0b01000, 0b01000, 0b00100, 0b00010],
        ')' => [0b01000, 0b00100, 0b00010, 0b00010, 0b00010, 0b00100, 0b01000],
        '<' => [0b00010, 0b00100, 0b01000, 0b10000, 0b01000, 0b00100, 0b00010],
        '>' => [0b01000, 0b00100, 0b00010, 0b00001, 0b00010, 0b00100, 0b01000],
        ',' => [0, 0, 0, 0, 0, 0b00100, 0b01000],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '=' => [0, 0b11111, 0, 0b11111, 0, 0, 0],
        '%' => [0b11001, 0b11010, 0b00100, 0b01000, 0b01011, 0b10011, 0],
        _ => [0; 7],
    }
}

fn format_last_button(btn: u32) -> &'static str {
    if btn & 0x4 != 0 {
        "[A / X / ENTER]"
    } else if btn & 0x8 != 0 {
        "[B / O / BKSP]"
    } else if btn & 0x10 != 0 {
        "[X / SQR / X]"
    } else if btn & 0x20 != 0 {
        "[Y / TRI / Y]"
    } else if btn & 0x40 != 0 {
        "[D-PAD UP]"
    } else if btn & 0x80 != 0 {
        "[D-PAD DOWN]"
    } else if btn & 0x100 != 0 {
        "[D-PAD LEFT]"
    } else if btn & 0x200 != 0 {
        "[D-PAD RIGHT]"
    } else if btn & 0x400 != 0 {
        "[LB / L1 / Q]"
    } else if btn & 0x800 != 0 {
        "[RB / R1 / E]"
    } else if btn & 0x1 != 0 {
        "[MENU / OPTIONS]"
    } else if btn & 0x2 != 0 {
        "[VIEW / SHARE]"
    } else if btn & 0x1000 != 0 {
        "[L-STICK / L3]"
    } else if btn & 0x2000 != 0 {
        "[R-STICK / R3]"
    } else if btn != 0 {
        "[EVENT]"
    } else {
        "[NONE]"
    }
}

/// Generate overlay text lines based on current metrics and limits.
fn get_overlay_lines(tenths: u32, ms: f64, draws: f64, limit: u32, expanded: bool) -> Vec<String> {
    let limit_str = match limit {
        30 => "[30]   60    120   UNLIM",
        60 => " 30   [60]   120   UNLIM",
        120 => " 30    60   [120]  UNLIM",
        0 => " 30    60    120  [UNLIM]",
        custom => return vec![format!("FPS LIMIT: {}", custom)],
    };

    let fps_f = (tenths as f64) / 10.0;
    let mut lines = Vec::new();
    lines.push(format!("SCARLETT | FPS: {:>4.1} ({:>4.1} MS) | {:>2.0} DRAWS", fps_f, ms, draws));
    lines.push(format!("FPS CEILING: {}  <F1>", limit_str));

    let dev = victory_core::input_status::get_active_device();
    let last_btn = victory_core::input_status::get_last_button();
    let dev_str = match dev {
        1 => "PS5 DUALSENSE (XINPUT) [OK]",
        2 => "KEYBOARD FALLBACK [OK]",
        _ => "STANDBY (DUALSENSE / KBD)",
    };
    lines.push(format!("INPUT: {} | LAST: {}", dev_str, format_last_button(last_btn)));

    if expanded {
        lines.push("-------------------------------------------------".to_string());
        lines.push("INPUT: PS5 DUALSENSE / XBOX / KEYBOARD".to_string());
        lines.push("[X / A / ENTER] SELECT     [O / B / BKSP] BACK".to_string());
        lines.push("[SQR / X / X]   ACTION     [TRI / Y / Y]  MENU".to_string());
        lines.push("[L1 / LB / Q]   PREV TAB   [R1 / RB / E]  NEXT TAB".to_string());
        lines.push("[L2 / LT / Z]   AIM/BRAKE  [R2 / RT / C]  FIRE/ACCEL".to_string());
        lines.push("[L-STICK / WASD] MOVE      [R-STICK / IJKL] CAMERA".to_string());
        lines.push("[D-PAD / ARROWS] NAVIGATE  [OPTIONS / ESC] PAUSE".to_string());
        lines.push("[SHARE / TAB]   VIEW MAP   [L3/R3 / F/H]  CLICK".to_string());
        lines.push("F1: CYCLE FPS LIMIT | F2: HIDE CONTROLS".to_string());
    } else {
        lines.push("PS5/XBOX/KBD ACTIVE | <F2: SHOW CONTROLS>".to_string());
    }
    lines
}

/// Bytes per pixel for supported swap chain formats.
fn pixel_bytes(format: DXGI_FORMAT) -> Option<usize> {
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM | DXGI_FORMAT_R10G10B10A2_UNORM => Some(4),
        DXGI_FORMAT_R16G16B16A16_FLOAT => Some(8),
        _ => None,
    }
}

/// Write a single pixel: bright cyan text or dark high-contrast backdrop.
fn pixel(format: DXGI_FORMAT, lit: bool, out: &mut [u8]) {
    let (r, g, b): (f32, f32, f32) = if lit {
        (0.15, 0.92, 0.95) // Bright crisp cyan text
    } else {
        (0.04, 0.05, 0.08) // Dark charcoal background
    };
    let byte = |v: f32| (v * 255.0).round() as u8;
    match format {
        DXGI_FORMAT_R8G8B8A8_UNORM => out.copy_from_slice(&[byte(r), byte(g), byte(b), 255]),
        DXGI_FORMAT_B8G8R8A8_UNORM => out.copy_from_slice(&[byte(b), byte(g), byte(r), 255]),
        DXGI_FORMAT_R10G10B10A2_UNORM => {
            let ten = |v: f32| (v * 1023.0).round() as u32;
            out.copy_from_slice(&(ten(r) | ten(g) << 10 | ten(b) << 20 | 3 << 30).to_le_bytes());
        }
        _ => {
            for (i, v) in [r, g, b, 1.0].into_iter().enumerate() {
                out[i * 2..i * 2 + 2].copy_from_slice(&half(v).to_le_bytes());
            }
        }
    }
}

/// Convert float [0, 1] to 16-bit half precision float.
fn half(v: f32) -> u16 {
    let bits = v.clamp(0.0, 1.0).to_bits();
    if v <= 0.0 {
        return 0;
    }
    let exponent = ((bits >> 23) & 0xFF) as i32 - 127 + 15;
    if exponent <= 0 {
        return 0;
    }
    ((exponent as u32) << 10 | (bits >> 13) & 0x3FF) as u16
}

/// Render multiline text into a 2D boolean pixel map.
fn bitmap(lines: &[String], scale: u32) -> (u32, u32, Vec<bool>) {
    let pad_x = 3 * scale;
    let pad_y = 3 * scale;
    let line_height = 7 * scale;
    let line_spacing = 2 * scale;
    let max_cols = lines.iter().map(|l| l.len()).max().unwrap_or(0) as u32;
    let width = 2 * pad_x + max_cols * 6 * scale;
    let num_lines = lines.len() as u32;
    let height = 2 * pad_y + num_lines * line_height + (num_lines.saturating_sub(1)) * line_spacing;
    let mut lit = vec![false; (width * height) as usize];

    for (line_idx, line) in lines.iter().enumerate() {
        let line_y = pad_y + (line_idx as u32) * (line_height + line_spacing);
        for (col_idx, c) in line.chars().enumerate() {
            let rows = glyph(c);
            let char_x = pad_x + (col_idx as u32) * 6 * scale;
            for (row, bits) in rows.into_iter().enumerate() {
                for col in 0..5u32 {
                    if (bits >> (4 - col)) & 1 == 0 {
                        continue;
                    }
                    let x0 = char_x + col * scale;
                    let y0 = line_y + (row as u32) * scale;
                    for y in y0..y0 + scale {
                        for x in x0..x0 + scale {
                            if y < height && x < width {
                                lit[(y * width + x) as usize] = true;
                            }
                        }
                    }
                }
            }
        }
    }
    (width, height, lit)
}

/// A GPU buffer holding the rendered overlay texture footprint.
struct Made {
    buffer: ID3D12Resource,
    width: u32,
    height: u32,
    pitch: u32,
}

// SAFETY: ID3D12Resource is thread-safe on Windows D3D12.
unsafe impl Send for Made {}

/// Cache of made overlay buffers.
static MADE: Mutex<Option<HashMap<u64, Made>>> = Mutex::new(None);

/// Copies the HUD overlay into the top right corner of the swap chain back buffer.
pub(crate) fn draw(
    host: &crate::host::Host,
    list: &ID3D12GraphicsCommandList,
    back: &ID3D12Resource,
    (width, height, format): (u32, u32, DXGI_FORMAT),
) {
    if !on() {
        return;
    }
    check_keys();

    let tenths = TENTHS.load(Ordering::Relaxed);
    let ms_tenths = MS_TENTHS.load(Ordering::Relaxed);
    let draws = DRAWS.load(Ordering::Relaxed);
    let limit = crate::frame::current_limit();
    let expanded = EXPANDED.load(Ordering::Relaxed);

    let Some(bytes) = pixel_bytes(format).filter(|_| tenths != u32::MAX) else {
        return;
    };

    let scale = (height / 540).clamp(2, 6);
    let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
    let made = made.get_or_insert_with(HashMap::new);
    if made.len() > 128 {
        made.clear();
    }

    let dev = victory_core::input_status::get_active_device();
    let last_btn = victory_core::input_status::get_last_button();

    let key = {
        let mut h = (tenths as u64) | ((ms_tenths as u64) << 16);
        h ^= (draws as u64) << 32;
        h ^= (limit as u64) << 48;
        h ^= (dev as u64) << 56;
        h ^= (last_btn as u64) << 20;
        h ^= if expanded { 1 << 63 } else { 0 };
        h ^= (format.0 as u64) << 24;
        h ^= (scale as u64) << 8;
        h
    };

    if !made.contains_key(&key) {
        let ms = (ms_tenths as f64) / 10.0;
        let lines = get_overlay_lines(tenths, ms, draws as f64, limit, expanded);
        let (w, h, lit) = bitmap(&lines, scale);
        let pitch = (w as usize * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
        let Some((buffer, data)) = host.shared_buffer((pitch * h as usize) as u64) else {
            return;
        };
        // SAFETY: Buffer memory mapped from D3D12 shared allocation.
        let out = unsafe { std::slice::from_raw_parts_mut(data, pitch * h as usize) };
        for y in 0..h as usize {
            for x in 0..w as usize {
                let at = y * pitch + x * bytes;
                pixel(format, lit[y * w as usize + x], &mut out[at..at + bytes]);
            }
        }
        made.insert(key, Made { buffer, width: w, height: h, pitch: pitch as u32 });
    }

    let Some(m) = made.get(&key) else {
        return;
    };

    let margin = 4 * scale;
    if m.width + 2 * margin > width || m.height + 2 * margin > height {
        return;
    }

    // Top-right alignment calculation
    let x_pos = width.saturating_sub(m.width + margin);
    let y_pos = margin;

    // SAFETY: D3D12 texture copy from mapped buffer to backbuffer.
    unsafe {
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&m.buffer),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: format,
                        Width: m.width,
                        Height: m.height,
                        Depth: 1,
                        RowPitch: m.pitch,
                    },
                },
            },
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(back),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        list.CopyTextureRegion(&to, x_pos, y_pos, 0, &from, None);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlay_lines_render_fps_and_limits() {
        let lines = get_overlay_lines(300, 33.3, 21.0, 30, false);
        assert_eq!(lines[0], "SCARLETT | FPS: 30.0 (33.3 MS) | 21 DRAWS");
        assert!(lines[1].contains("[30]   60    120   UNLIM"));

        let lines_60 = get_overlay_lines(600, 16.7, 45.0, 60, false);
        assert!(lines_60[1].contains(" 30   [60]   120   UNLIM"));

        let lines_unlim = get_overlay_lines(1250, 8.0, 50.0, 0, false);
        assert!(lines_unlim[1].contains(" 30    60    120  [UNLIM]"));
    }

    #[test]
    fn bitmap_renders_multiline() {
        let lines = vec!["ABC".to_string(), "123".to_string()];
        let (w, h, lit) = bitmap(&lines, 2);
        assert!(w > 0 && h > 0);
        assert_eq!(lit.len(), (w * h) as usize);
    }

    #[test]
    fn pixels_match_formats() {
        let mut four = [0u8; 4];
        pixel(DXGI_FORMAT_R8G8B8A8_UNORM, true, &mut four);
        assert_eq!(four[3], 255);
        pixel(DXGI_FORMAT_B8G8R8A8_UNORM, true, &mut four);
        assert_eq!(four[3], 255);
    }
}
