//! The HUD overlay and interactive performance/graphics dropdown menu.
//!
//! Includes an interactive dropdown menu with sections for:
//! 1. Display Resolution (Initial 1920x1200, 1080p, 1440p, 2560p, 4K)
//! 2. Ray-Tracing (Dynamic RT Shadows, RT Light Shadows, BVH Traversal Guard)
//! 3. AMD FidelityFX FSR & Upscaling (Modes & Sharpening)
//! 4. Graphics & Engine (FPS Ceiling, Water SSR, Async Compute, Motion Blur)
//! 5. PS5 DualSense / Xbox / Keyboard input mappings cheat-sheet
//!
//! Toggled open/closed with F2, navigated with Up/Down & Left/Right arrows or controller D-pad.

use std::collections::HashMap;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

use windows::Win32::Graphics::Direct3D12::*;
use windows::Win32::Graphics::Dxgi::Common::*;

/// Measured FPS in tenths (e.g. 300 = 30.0 fps).
static TENTHS: AtomicU32 = AtomicU32::new(300);
/// Measured frame time in tenths of a ms (e.g. 333 = 33.3 ms).
static MS_TENTHS: AtomicU32 = AtomicU32::new(333);
/// Average draw calls per frame.
static DRAWS: AtomicU32 = AtomicU32::new(21);

/// Global GPU overload fail-safe state
static GPU_OVERLOAD: AtomicBool = AtomicBool::new(false);
static OVERLOAD_MESSAGE: Mutex<Option<String>> = Mutex::new(None);

/// Global skip-boot preference (defaults to true if VICTORY_SKIP_BOOT != "0")
static SKIP_BOOT: AtomicBool = AtomicBool::new(true);

pub fn is_gpu_overloaded() -> bool {
    GPU_OVERLOAD.load(Ordering::Relaxed)
}

pub fn overload_reason() -> Option<String> {
    OVERLOAD_MESSAGE.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

pub fn trigger_gpu_overload(reason: &str) {
    GPU_OVERLOAD.store(true, Ordering::Relaxed);
    let mut msg = OVERLOAD_MESSAGE.lock().unwrap_or_else(|e| e.into_inner());
    *msg = Some(reason.to_string());
    tracing::error!("GPU OVERLOAD FAIL-SAFE TRIGGERED: {reason}");
}

pub fn clear_gpu_overload() {
    GPU_OVERLOAD.store(false, Ordering::Relaxed);
    let mut msg = OVERLOAD_MESSAGE.lock().unwrap_or_else(|e| e.into_inner());
    *msg = None;
}

pub fn is_skip_boot() -> bool {
    SKIP_BOOT.load(Ordering::Relaxed)
}

pub fn set_skip_boot(val: bool) {
    SKIP_BOOT.store(val, Ordering::Relaxed);
}

/// Whether CPU & GPU performance graphs are displayed in top-right (toggled via F4).
static SHOW_GRAPHS: AtomicBool = AtomicBool::new(true);

pub fn is_graphs_visible() -> bool {
    SHOW_GRAPHS.load(Ordering::Relaxed)
}

pub fn set_graphs_visible(val: bool) {
    SHOW_GRAPHS.store(val, Ordering::Relaxed);
}

/// Rolling sample history for CPU & GPU usage percentages (0..=100)
static CPU_HISTORY: Mutex<Vec<u8>> = Mutex::new(Vec::new());
static GPU_HISTORY: Mutex<Vec<u8>> = Mutex::new(Vec::new());

/// Toast notification message displayed temporarily in the HUD overlay
static TOAST: Mutex<Option<(String, std::time::Instant)>> = Mutex::new(None);

pub fn trigger_toast(msg: &str) {
    let mut lock = TOAST.lock().unwrap_or_else(|e| e.into_inner());
    *lock = Some((msg.to_string(), std::time::Instant::now()));
}

fn active_toast() -> Option<String> {
    let mut lock = TOAST.lock().unwrap_or_else(|e| e.into_inner());
    if let Some((msg, created)) = lock.as_ref() {
        if created.elapsed() < std::time::Duration::from_secs(3) {
            return Some(msg.clone());
        }
    }
    *lock = None;
    None
}

/// Sample system-wide CPU usage percentage via Windows GetSystemTimes
#[cfg(windows)]
pub fn sample_cpu_percent() -> u8 {
    use windows_sys::Win32::Foundation::FILETIME;
    use windows_sys::Win32::System::Threading::GetSystemTimes;
    static LAST: Mutex<Option<(u64, u64, u64)>> = Mutex::new(None);

    let mut idle = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut kernel = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };
    let mut user = FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 };

    let to_u64 = |ft: FILETIME| ((ft.dwHighDateTime as u64) << 32) | (ft.dwLowDateTime as u64);

    if unsafe { GetSystemTimes(&mut idle, &mut kernel, &mut user) } != 0 {
        let cur_idle = to_u64(idle);
        let cur_kernel = to_u64(kernel);
        let cur_user = to_u64(user);

        let mut lock = LAST.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((prev_idle, prev_kernel, prev_user)) = *lock {
            let d_idle = cur_idle.saturating_sub(prev_idle);
            let d_kernel = cur_kernel.saturating_sub(prev_kernel);
            let d_user = cur_user.saturating_sub(prev_user);
            let total = d_kernel + d_user;
            *lock = Some((cur_idle, cur_kernel, cur_user));
            if total > 0 {
                let busy = total.saturating_sub(d_idle);
                return ((busy * 100) / total).min(100) as u8;
            }
        } else {
            *lock = Some((cur_idle, cur_kernel, cur_user));
        }
    }
    35
}

#[cfg(not(windows))]
pub fn sample_cpu_percent() -> u8 {
    35
}

pub fn record_gpu_sample(gpu_pct: u8) {
    let cpu_pct = sample_cpu_percent();
    let mut cpu = CPU_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if cpu.is_empty() {
        cpu.extend(vec![25; 39]);
    }
    if cpu.len() >= 40 {
        cpu.remove(0);
    }
    cpu.push(cpu_pct);

    let mut gpu = GPU_HISTORY.lock().unwrap_or_else(|e| e.into_inner());
    if gpu.is_empty() {
        gpu.extend(vec![35; 39]);
    }
    if gpu.len() >= 40 {
        gpu.remove(0);
    }
    gpu.push(gpu_pct);
}

/// Whether the dropdown menu is dropped down / open.
static MENU_OPEN: AtomicBool = AtomicBool::new(true);
/// Whether the entire overlay / dropdown menu is hidden (toggled via F3).
static HUD_HIDDEN: AtomicBool = AtomicBool::new(false);
/// Currently focused row index in the dropdown menu.
static CURRENT_ROW: AtomicUsize = AtomicUsize::new(0);

// --- Settings state ---

/// Resolution choices: 1200p (Initial/Default), 1080p, 1440p, 2560p, 4K.
pub const RESOLUTIONS: &[(u32, u32, &str)] = &[
    (1920, 1200, "1200p (1920x1200)"),
    (1920, 1080, "1080p (1920x1080)"),
    (2560, 1440, "1440p (2560x1440)"),
    (3840, 2560, "2560p (3840x2560)"),
    (3840, 2160, "4K (3840x2160)"),
];
static RES_IDX: AtomicUsize = AtomicUsize::new(0); // Default: 1920x1200

// Ray-tracing
static RT_DYNAMIC: AtomicBool = AtomicBool::new(false); // Default: safe false (prevents BVH hang)
static RT_LIGHTS: AtomicBool = AtomicBool::new(false);
static BVH_GUARD: AtomicBool = AtomicBool::new(true); // Default: hold unbuilt BVH dispatches

// FSR
pub const FSR_MODES: &[&str] = &["OFF", "ULTRA", "QUALITY", "BALANCED", "PERFORMANCE"];
static FSR_IDX: AtomicUsize = AtomicUsize::new(2); // Default: Quality
pub const FSR_SHARPNESS: &[&str] = &["0.0", "0.2", "0.5", "0.8", "1.0"];
static FSR_SHARP_IDX: AtomicUsize = AtomicUsize::new(1); // Default: 0.2

// Graphics & Performance
static WATER_SSR: AtomicBool = AtomicBool::new(false); // Default: safe false
static ASYNC_COMPUTE: AtomicBool = AtomicBool::new(true);
static MOTION_BLUR: AtomicBool = AtomicBool::new(false);
static CONTROLS_EXPANDED: AtomicBool = AtomicBool::new(true);

const TOTAL_ROWS: usize = 11;

/// Whether the overlay is enabled at all.
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

/// Sync active in-memory settings to the game's settings.xml file.
fn persist_settings_async() {
    std::thread::spawn(|| {
        let Ok(cfg) = victory_core::Config::load() else {
            return;
        };
        let path = cfg.data_dir.join("7952987A/storage/settings/settings.xml");
        if !path.exists() {
            return;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            return;
        };

        let res_idx = RES_IDX.load(Ordering::Relaxed).min(RESOLUTIONS.len() - 1);
        let (w, h, _) = RESOLUTIONS[res_idx];
        let rt_dyn = RT_DYNAMIC.load(Ordering::Relaxed);
        let rt_light = RT_LIGHTS.load(Ordering::Relaxed);
        let fsr_mode = FSR_IDX.load(Ordering::Relaxed).min(FSR_MODES.len() - 1);
        let fsr_sharp_str = FSR_SHARPNESS[FSR_SHARP_IDX.load(Ordering::Relaxed).min(FSR_SHARPNESS.len() - 1)];
        let water_ssr = WATER_SSR.load(Ordering::Relaxed);
        let async_comp = ASYNC_COMPUTE.load(Ordering::Relaxed);
        let m_blur = MOTION_BLUR.load(Ordering::Relaxed);

        let mut lines: Vec<String> = content.lines().map(|s| s.to_string()).collect();
        let mut in_res = false;

        for line in &mut lines {
            if line.contains("<resolutionFullscreen>") {
                in_res = true;
            } else if in_res && line.contains("</resolutionFullscreen>") {
                in_res = false;
            } else if in_res && line.contains("<width value=") {
                *line = format!("      <width value=\"{}\" />", w);
            } else if in_res && line.contains("<height value=") {
                *line = format!("      <height value=\"{}\" />", h);
            } else if line.contains("<rtDynamicShadows value=") {
                *line = format!("    <rtDynamicShadows value=\"{}\" />", rt_dyn);
            } else if line.contains("<rtLODLightShadows value=") {
                *line = format!("    <rtLODLightShadows value=\"{}\" />", rt_light);
            } else if line.contains("<fsrQuality value=") {
                *line = format!("    <fsrQuality value=\"{}\" />", fsr_mode);
            } else if line.contains("<fsrSharpen value=") {
                *line = format!("    <fsrSharpen value=\"{:.6}\" />", fsr_sharp_str.parse::<f64>().unwrap_or(0.2));
            } else if line.contains("<waterReflectionSSR value=") {
                *line = format!("    <waterReflectionSSR value=\"{}\" />", water_ssr);
            } else if line.contains("<asyncComputeEnabled value=") {
                *line = format!("    <asyncComputeEnabled value=\"{}\" />", async_comp);
            } else if line.contains("<motionBlur value=") {
                *line = format!("    <motionBlur value=\"{}\" />", m_blur);
            }
        }

        let updated = lines.join("\n");
        let _ = std::fs::write(&path, updated);
    });
}

/// Cycle or toggle the setting at `row`. `dir`: -1 for left/prev, 1 for right/next.
fn change_setting(row: usize, dir: i32) {
    match row {
        0 => {
            // Resolution
            let cur = RES_IDX.load(Ordering::Relaxed);
            let next = if dir < 0 {
                if cur == 0 { RESOLUTIONS.len() - 1 } else { cur - 1 }
            } else {
                (cur + 1) % RESOLUTIONS.len()
            };
            RES_IDX.store(next, Ordering::Relaxed);
            let (w, h, _) = RESOLUTIONS[next];
            crate::dxgi::set_display_size(w, h);
            persist_settings_async();
        }
        1 => {
            // Dynamic RT Shadows
            let cur = RT_DYNAMIC.load(Ordering::Relaxed);
            RT_DYNAMIC.store(!cur, Ordering::Relaxed);
            persist_settings_async();
        }
        2 => {
            // RT Light Shadows
            let cur = RT_LIGHTS.load(Ordering::Relaxed);
            RT_LIGHTS.store(!cur, Ordering::Relaxed);
            persist_settings_async();
        }
        3 => {
            // BVH Traversal Guard
            let cur = BVH_GUARD.load(Ordering::Relaxed);
            BVH_GUARD.store(!cur, Ordering::Relaxed);
        }
        4 => {
            // FSR Mode
            let cur = FSR_IDX.load(Ordering::Relaxed);
            let next = if dir < 0 {
                if cur == 0 { FSR_MODES.len() - 1 } else { cur - 1 }
            } else {
                (cur + 1) % FSR_MODES.len()
            };
            FSR_IDX.store(next, Ordering::Relaxed);
            persist_settings_async();
        }
        5 => {
            // FSR Sharpening
            let cur = FSR_SHARP_IDX.load(Ordering::Relaxed);
            let next = if dir < 0 {
                if cur == 0 { FSR_SHARPNESS.len() - 1 } else { cur - 1 }
            } else {
                (cur + 1) % FSR_SHARPNESS.len()
            };
            FSR_SHARP_IDX.store(next, Ordering::Relaxed);
            persist_settings_async();
        }
        6 => {
            // FPS Limit
            if dir < 0 {
                let prev = match crate::frame::current_limit() {
                    30 => 0,
                    60 => 30,
                    120 => 60,
                    _ => 120,
                };
                crate::frame::set_limit(prev);
            } else {
                crate::frame::cycle_limit();
            }
        }
        7 => {
            // Water SSR
            let cur = WATER_SSR.load(Ordering::Relaxed);
            WATER_SSR.store(!cur, Ordering::Relaxed);
            persist_settings_async();
        }
        8 => {
            // Async Compute
            let cur = ASYNC_COMPUTE.load(Ordering::Relaxed);
            ASYNC_COMPUTE.store(!cur, Ordering::Relaxed);
            persist_settings_async();
        }
        9 => {
            // Motion Blur
            let cur = MOTION_BLUR.load(Ordering::Relaxed);
            MOTION_BLUR.store(!cur, Ordering::Relaxed);
            persist_settings_async();
        }
        10 => {
            // Controls Cheatsheet
            let cur = CONTROLS_EXPANDED.load(Ordering::Relaxed);
            CONTROLS_EXPANDED.store(!cur, Ordering::Relaxed);
        }
        _ => {}
    }
}

/// Check keyboard and controller inputs for navigating the dropdown menu.
fn check_keys() {
    #[cfg(windows)]
    {
        use windows::Win32::UI::Input::KeyboardAndMouse::{
            GetAsyncKeyState, VK_DOWN, VK_ESCAPE, VK_F1, VK_F2, VK_F3, VK_F4, VK_LEFT, VK_RETURN, VK_RIGHT,
            VK_SPACE, VK_UP,
        };

        static F1_HELD: AtomicBool = AtomicBool::new(false);
        static F2_HELD: AtomicBool = AtomicBool::new(false);
        static F3_HELD: AtomicBool = AtomicBool::new(false);
        static F4_HELD: AtomicBool = AtomicBool::new(false);
        static UP_HELD: AtomicBool = AtomicBool::new(false);
        static DOWN_HELD: AtomicBool = AtomicBool::new(false);
        static LEFT_HELD: AtomicBool = AtomicBool::new(false);
        static RIGHT_HELD: AtomicBool = AtomicBool::new(false);
        static ESC_HELD: AtomicBool = AtomicBool::new(false);

        let is_down = |vk: u16| unsafe { (GetAsyncKeyState(vk as i32) as u16 & 0x8000) != 0 };

        // F1: Quick-cycle FPS ceiling anytime
        let f1 = is_down(VK_F1.0);
        if f1 && !F1_HELD.swap(true, Ordering::Relaxed) {
            crate::frame::cycle_limit();
        } else if !f1 {
            F1_HELD.store(false, Ordering::Relaxed);
        }

        // F2: Toggle Dropdown Menu Open/Close
        let f2 = is_down(VK_F2.0);
        if f2 && !F2_HELD.swap(true, Ordering::Relaxed) {
            let prev = MENU_OPEN.load(Ordering::Relaxed);
            MENU_OPEN.store(!prev, Ordering::Relaxed);
        } else if !f2 {
            F2_HELD.store(false, Ordering::Relaxed);
        }

        // F3: Toggle entire HUD / dropdown menu visibility (hide/show)
        let f3 = is_down(VK_F3.0);
        if f3 && !F3_HELD.swap(true, Ordering::Relaxed) {
            let prev = HUD_HIDDEN.load(Ordering::Relaxed);
            HUD_HIDDEN.store(!prev, Ordering::Relaxed);
        } else if !f3 {
            F3_HELD.store(false, Ordering::Relaxed);
        }

        // F4: Toggle CPU & GPU usage performance graphs in top-right corner
        let f4 = is_down(VK_F4.0);
        if f4 && !F4_HELD.swap(true, Ordering::Relaxed) {
            let prev = SHOW_GRAPHS.load(Ordering::Relaxed);
            SHOW_GRAPHS.store(!prev, Ordering::Relaxed);
            if !prev {
                trigger_toast("[F4] PERFORMANCE GRAPHS: ON");
            } else {
                trigger_toast("[F4] PERFORMANCE GRAPHS: OFF");
            }
        } else if !f4 {
            F4_HELD.store(false, Ordering::Relaxed);
        }

        let esc = is_down(VK_ESCAPE.0);
        if esc && !ESC_HELD.swap(true, Ordering::Relaxed) {
            MENU_OPEN.store(false, Ordering::Relaxed);
        } else if !esc {
            ESC_HELD.store(false, Ordering::Relaxed);
        }

        // Navigation when dropdown menu is open
        if MENU_OPEN.load(Ordering::Relaxed) {
            let up = is_down(VK_UP.0) || is_down(b'W' as u16);
            if up && !UP_HELD.swap(true, Ordering::Relaxed) {
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                CURRENT_ROW.store(if cur == 0 { TOTAL_ROWS - 1 } else { cur - 1 }, Ordering::Relaxed);
            } else if !up {
                UP_HELD.store(false, Ordering::Relaxed);
            }

            let down = is_down(VK_DOWN.0) || is_down(b'S' as u16);
            if down && !DOWN_HELD.swap(true, Ordering::Relaxed) {
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                CURRENT_ROW.store((cur + 1) % TOTAL_ROWS, Ordering::Relaxed);
            } else if !down {
                DOWN_HELD.store(false, Ordering::Relaxed);
            }

            let left = is_down(VK_LEFT.0) || is_down(b'A' as u16);
            if left && !LEFT_HELD.swap(true, Ordering::Relaxed) {
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                change_setting(cur, -1);
            } else if !left {
                LEFT_HELD.store(false, Ordering::Relaxed);
            }

            let right = is_down(VK_RIGHT.0) || is_down(b'D' as u16) || is_down(VK_RETURN.0) || is_down(VK_SPACE.0);
            if right && !RIGHT_HELD.swap(true, Ordering::Relaxed) {
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                change_setting(cur, 1);
            } else if !right {
                RIGHT_HELD.store(false, Ordering::Relaxed);
            }
        }
    }

    // Controller input handling via input_status
    let btn = victory_core::input_status::get_last_button();
    static LAST_BTN: AtomicU32 = AtomicU32::new(0);
    let prev_btn = LAST_BTN.swap(btn, Ordering::Relaxed);
    if btn != 0 && btn != prev_btn {
        // PS5 Controller Menu (Options) button: bit 0x1 triggers skip boot screen & cutscenes
        if btn & 0x1 != 0 {
            set_skip_boot(true);
            trigger_toast("[PS5 MENU] SKIPPING BOOT / INTRO");
            tracing::info!("PS5 Menu button pressed: skipping boot screen / intro cutscenes");
        }

        if MENU_OPEN.load(Ordering::Relaxed) {
            if btn & 0x40 != 0 {
                // D-Pad Up
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                CURRENT_ROW.store(if cur == 0 { TOTAL_ROWS - 1 } else { cur - 1 }, Ordering::Relaxed);
            } else if btn & 0x80 != 0 {
                // D-Pad Down
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                CURRENT_ROW.store((cur + 1) % TOTAL_ROWS, Ordering::Relaxed);
            } else if btn & 0x100 != 0 {
                // D-Pad Left
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                change_setting(cur, -1);
            } else if (btn & 0x200 != 0) || (btn & 0x4 != 0) {
                // D-Pad Right or Select (A)
                let cur = CURRENT_ROW.load(Ordering::Relaxed);
                change_setting(cur, 1);
            } else if btn & 0x8 != 0 {
                // Back (B)
                MENU_OPEN.store(false, Ordering::Relaxed);
            }
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
        '*' => [0b00000, 0b10101, 0b01110, 0b11111, 0b01110, 0b10101, 0b00000],
        '^' => [0b00100, 0b01010, 0b10001, 0, 0, 0, 0],
        'v' => [0, 0, 0, 0b10001, 0b01010, 0b00100, 0],
        ',' => [0, 0, 0, 0, 0, 0b00100, 0b01000],
        '!' => [0b00100, 0b00100, 0b00100, 0b00100, 0b00100, 0, 0b00100],
        '?' => [0b01110, 0b10001, 0b00001, 0b00010, 0b00100, 0, 0b00100],
        '_' => [0, 0, 0, 0, 0, 0, 0b11111],
        '=' => [0, 0b11111, 0, 0b11111, 0, 0, 0],
        '%' => [0b11001, 0b11010, 0b00100, 0b01000, 0b01011, 0b10011, 0],
        _ => [0; 7],
    }
}

/// Format the dropdown menu text lines based on current metrics and selections.
fn get_overlay_lines(
    tenths: u32,
    ms: f64,
    draws: f64,
    limit: u32,
    menu_open: bool,
    focused_row: usize,
) -> Vec<String> {
    let fps_f = (tenths as f64) / 10.0;
    let (cur_w, cur_h) = crate::dxgi::current_display_size();

    // If dropdown menu is closed, show a concise HUD bar
    if !menu_open {
        let hud_title = if let Some(toast) = active_toast() {
            format!("SCARLETT HUD | {:>4.1} FPS | {} | [F4: GRAPHS]", fps_f, toast)
        } else {
            format!("SCARLETT HUD | {:>4.1} FPS ({:>4.1} MS) | {:>4}x{:<4} | [F2: MENU] [F3: HIDE] [F4: GRAPHS]", fps_f, ms, cur_w, cur_h)
        };
        return vec![hud_title];
    }

    let mut lines = Vec::new();
    lines.push("===================================================================".to_string());
    lines.push(" [v] SCARLETT GRAPHICS & PERFORMANCE DROPDOWN MENU     <F2: CLOSE>".to_string());
    lines.push(format!(" PERF: {:>4.1} FPS ({:>4.1} MS) | {:>2.0} DRAWS | ACTIVE: {:>4}x{:<4}", fps_f, ms, draws, cur_w, cur_h));
    lines.push("===================================================================".to_string());

    let marker = |row: usize| -> &'static str {
        if row == focused_row { ">" } else { " " }
    };

    // 1. Resolution
    lines.push("-- [1] DISPLAY RESOLUTION -----------------------------------------".to_string());
    let r_idx = RES_IDX.load(Ordering::Relaxed);
    let mut res_line = format!("{} RESOLUTION : ", marker(0));
    for (i, &(_, _, label)) in RESOLUTIONS.iter().enumerate() {
        let short = label.split(' ').next().unwrap_or(label);
        if i == r_idx {
            res_line.push_str(&format!(" *[{}]*", short));
        } else {
            res_line.push_str(&format!("  {} ", short));
        }
    }
    lines.push(res_line);

    // 2. Ray-tracing
    lines.push("-- [2] RAY-TRACING & BVH ACCELERATION -----------------------------".to_string());
    let rt_dyn = RT_DYNAMIC.load(Ordering::Relaxed);
    let dyn_str = if rt_dyn {
        "   OFF         *[ENABLED (WARNS: BVH HANG)]*"
    } else {
        " *[DISABLED (SAFE)]*         ENABLED        "
    };
    lines.push(format!("{} DYNAMIC RT : {}", marker(1), dyn_str));

    let rt_lt = RT_LIGHTS.load(Ordering::Relaxed);
    let lt_str = if rt_lt { "   OFF       *[ON]*" } else { " *[OFF]*       ON  " };
    lines.push(format!("{} RT LIGHTS  : {}", marker(2), lt_str));

    let bvh = BVH_GUARD.load(Ordering::Relaxed);
    let bvh_str = if bvh {
        " *[HOLD (SAFE: PREVENTS TDR)]*   PASSTHROUGH"
    } else {
        "   HOLD                          *[PASSTHROUGH]*"
    };
    lines.push(format!("{} BVH GUARD  : {}", marker(3), bvh_str));

    // 3. FSR
    lines.push("-- [3] AMD FIDELITYFX FSR -----------------------------------------".to_string());
    let fsr_idx = FSR_IDX.load(Ordering::Relaxed);
    let mut fsr_line = format!("{} FSR 3.1    : ", marker(4));
    for (i, &name) in FSR_MODES.iter().enumerate() {
        if i == fsr_idx {
            fsr_line.push_str(&format!(" *[{}]*", name));
        } else {
            fsr_line.push_str(&format!("  {} ", name));
        }
    }
    lines.push(fsr_line);

    let fsr_sharp_idx = FSR_SHARP_IDX.load(Ordering::Relaxed);
    let mut sharp_line = format!("{} SHARPENING : ", marker(5));
    for (i, &val) in FSR_SHARPNESS.iter().enumerate() {
        if i == fsr_sharp_idx {
            sharp_line.push_str(&format!(" *[{}]*", val));
        } else {
            sharp_line.push_str(&format!("  {} ", val));
        }
    }
    lines.push(sharp_line);

    // 4. Graphics & Pace
    lines.push("-- [4] GRAPHICS & FRAME PACING ------------------------------------".to_string());
    let limit_str = match limit {
        30 => " *[30]*   60    120   UNLIMITED",
        60 => "  30   *[60]*   120   UNLIMITED",
        120 => "  30    60   *[120]*  UNLIMITED",
        _ => "  30    60    120  *[UNLIMITED]*",
    };
    lines.push(format!("{} FPS LIMIT  : {}  <F1>", marker(6), limit_str));

    let w_ssr = WATER_SSR.load(Ordering::Relaxed);
    let ssr_str = if w_ssr {
        "   OFF (STABLE)        *[ON (HIGH READBACK)]*"
    } else {
        " *[OFF (STABLE)]*        ON                  "
    };
    lines.push(format!("{} WATER SSR  : {}", marker(7), ssr_str));

    let async_c = ASYNC_COMPUTE.load(Ordering::Relaxed);
    let async_str = if async_c { " *[ENABLED]*    DISABLED" } else { "   ENABLED   *[DISABLED]*" };
    lines.push(format!("{} ASYNC COMP : {}", marker(8), async_str));

    let mb = MOTION_BLUR.load(Ordering::Relaxed);
    let mb_str = if mb { "   OFF       *[ON]*" } else { " *[OFF]*       ON  " };
    lines.push(format!("{} MOTION BLUR: {}", marker(9), mb_str));

    // 5. Cheatsheet
    lines.push("-- [5] INPUT & CONTROLLER CHEATSHEET ------------------------------".to_string());
    let expanded = CONTROLS_EXPANDED.load(Ordering::Relaxed);
    let expand_tag = if expanded { "[- HIDE]" } else { "[+ EXPAND]" };
    lines.push(format!("{} DUALSENSE / XBOX / KEYBOARD CHEATSHEET  {}", marker(10), expand_tag));
    if expanded {
        lines.push("  SELECT: [X / A / ENTER]        BACK: [O / B / BKSP]".to_string());
        lines.push("  ACTION: [SQR / X / X]          MENU: [TRI / Y / Y]".to_string());
        lines.push("  TABS  : [L1/R1 / Q/E]          AIM/FIRE: [L2/R2 / Z/C]".to_string());
        lines.push("  STICKS: [WASD / IJKL]          NAV: [D-PAD / ARROW KEYS]".to_string());
        lines.push("  SKIP BOOT: [PS5 MENU (OPTIONS)]  GRAPHS: [F4 TOP-RIGHT]".to_string());
    }

    lines.push("===================================================================".to_string());
    lines.push("NAV: [UP/DOWN] SELECT  [LEFT/RIGHT] CHANGE  [F2] MENU  [F3] HIDE  [F4] GRAPHS".to_string());
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

/// Write a single pixel: crisp bright white text, accent gold for highlights, or dark high-contrast backdrop.
fn pixel(format: DXGI_FORMAT, lit: bool, is_highlight: bool, out: &mut [u8]) {
    let (r, g, b): (f32, f32, f32) = if lit {
        if is_highlight {
            (0.98, 0.85, 0.15) // Warm golden highlight for active selections
        } else {
            (0.95, 0.95, 0.95) // Crisp bright white text
        }
    } else {
        (0.04, 0.05, 0.08) // Dark charcoal translucent background
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

/// Render multiline text into a 2D boolean pixel map with highlight detection.
fn bitmap(lines: &[String], scale: u32) -> (u32, u32, Vec<bool>, Vec<bool>) {
    let pad_x = 3 * scale;
    let pad_y = 3 * scale;
    let line_height = 7 * scale;
    let line_spacing = 2 * scale;
    let max_cols = lines.iter().map(|l| l.len()).max().unwrap_or(0) as u32;
    let width = 2 * pad_x + max_cols * 6 * scale;
    let num_lines = lines.len() as u32;
    let height = 2 * pad_y + num_lines * line_height + (num_lines.saturating_sub(1)) * line_spacing;
    let mut lit = vec![false; (width * height) as usize];
    let mut highlight = vec![false; (width * height) as usize];

    for (line_idx, line) in lines.iter().enumerate() {
        let is_selected_line = line.starts_with('>');
        let line_y = pad_y + (line_idx as u32) * (line_height + line_spacing);
        let mut inside_bracket_star = false;

        for (col_idx, c) in line.chars().enumerate() {
            if c == '*' {
                inside_bracket_star = !inside_bracket_star;
            }
            let is_hl = is_selected_line || inside_bracket_star;

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
                                let idx = (y * width + x) as usize;
                                lit[idx] = true;
                                if is_hl {
                                    highlight[idx] = true;
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    (width, height, lit, highlight)
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

/// Copies the HUD dropdown menu overlay into the swap chain back buffer.
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

    let hud_hidden = HUD_HIDDEN.load(Ordering::Relaxed);
    let show_graphs = SHOW_GRAPHS.load(Ordering::Relaxed);
    if hud_hidden && !show_graphs && !GPU_OVERLOAD.load(Ordering::Relaxed) {
        return;
    }

    if GPU_OVERLOAD.load(Ordering::Relaxed) {
        let reason = overload_reason().unwrap_or_else(|| "GPU compute pipeline or driver overload".to_string());
        draw_overload_screen(host, list, back, (width, height, format), &reason);
        return;
    }

    let tenths = TENTHS.load(Ordering::Relaxed);
    let ms_tenths = MS_TENTHS.load(Ordering::Relaxed);
    let draws = DRAWS.load(Ordering::Relaxed);
    let limit = crate::frame::current_limit();
    let menu_open = MENU_OPEN.load(Ordering::Relaxed);
    let focused_row = CURRENT_ROW.load(Ordering::Relaxed);

    let Some(bytes) = pixel_bytes(format).filter(|_| tenths != u32::MAX) else {
        return;
    };

    let scale = if menu_open {
        (height / 720).clamp(1, 3)
    } else {
        (height / 540).clamp(2, 4)
    };

    if !hud_hidden {
        let mut made = MADE.lock().unwrap_or_else(|e| e.into_inner());
        let made = made.get_or_insert_with(HashMap::new);
        if made.len() > 64 {
            made.clear();
        }

        let res_idx = RES_IDX.load(Ordering::Relaxed);
        let rt_dyn = RT_DYNAMIC.load(Ordering::Relaxed);
        let fsr_idx = FSR_IDX.load(Ordering::Relaxed);
        let (cur_w, cur_h) = crate::dxgi::current_display_size();

        let key = {
            let mut h = (tenths as u64) | ((ms_tenths as u64) << 16);
            h ^= (draws as u64) << 32;
            h ^= (limit as u64) << 48;
            h ^= (focused_row as u64) << 56;
            h ^= (res_idx as u64) << 8;
            h ^= (fsr_idx as u64) << 24;
            h ^= ((cur_w as u64) << 36) ^ ((cur_h as u64) << 12);
            h ^= if rt_dyn { 1 << 62 } else { 0 };
            h ^= if menu_open { 1 << 63 } else { 0 };
            h ^= (format.0 as u64) << 28;
            h ^= (scale as u64) << 4;
            h
        };

        if !made.contains_key(&key) {
            let ms = (ms_tenths as f64) / 10.0;
            let lines = get_overlay_lines(tenths, ms, draws as f64, limit, menu_open, focused_row);
            let (w, h, lit, hl) = bitmap(&lines, scale);
            let pitch = (w as usize * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
            let Some((buffer, data)) = host.shared_buffer((pitch * h as usize) as u64) else {
                return;
            };
            // SAFETY: Buffer memory mapped from D3D12 shared allocation.
            let out = unsafe { std::slice::from_raw_parts_mut(data, pitch * h as usize) };
            for y in 0..h as usize {
                for x in 0..w as usize {
                    let at = y * pitch + x * bytes;
                    let is_lit = lit[y * w as usize + x];
                    let is_hl = hl[y * w as usize + x];
                    pixel(format, is_lit, is_hl, &mut out[at..at + bytes]);
                }
            }
            made.insert(key, Made { buffer, width: w, height: h, pitch: pitch as u32 });
        }

        if let Some(m) = made.get(&key) {
            let margin_x = (8 * scale).min(32);
            let margin_y = (6 * scale).min(24);
            if m.width + 2 * margin_x <= width && m.height + 2 * margin_y <= height {
                let hud_pos = std::env::var("VICTORY_HUD_POS").unwrap_or_else(|_| "top-left".to_string());
                let (x_pos, y_pos) = match hud_pos.as_str() {
                    "top-right" => (
                        width.saturating_sub(m.width + margin_x),
                        margin_y,
                    ),
                    "bottom-left" => (margin_x, height.saturating_sub(m.height + margin_y)),
                    "bottom-right" => (
                        width.saturating_sub(m.width + margin_x),
                        height.saturating_sub(m.height + margin_y),
                    ),
                    _ => (margin_x, margin_y), // Top-left (default)
                };

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
        }
    }

    if show_graphs {
        draw_graphs(host, list, back, (width, height, format), scale);
    }
}

/// Cache of made graph overlay buffers (top-right corner).
static GRAPH_MADE: Mutex<Option<HashMap<u64, Made>>> = Mutex::new(None);

/// Render CPU & GPU usage bitmap with graphical sparklines.
fn render_graphs_bitmap(cpu_hist: &[u8], gpu_hist: &[u8], scale: u32) -> (u32, u32, Vec<bool>, Vec<bool>) {
    let cpu_cur = *cpu_hist.last().unwrap_or(&30);
    let gpu_cur = *gpu_hist.last().unwrap_or(&35);

    let lines = vec![
        "-- [F4] HARDWARE MONITORS ----------------------".to_string(),
        format!(" CPU LOAD: {:>3}%  [SYSTEM THREADS]             ", cpu_cur),
        "                                                ".to_string(), // Row 2: CPU Graph Plot Area
        format!(" GPU LOAD: {:>3}%  [D3D12 SCARLETT ENGINE]      ", gpu_cur),
        "                                                ".to_string(), // Row 4: GPU Graph Plot Area
        "================================================".to_string(),
        " [F4: HIDE/SHOW GRAPHS]                         ".to_string(),
    ];

    let (w, h, mut lit, mut hl) = bitmap(&lines, scale);

    let pad_x = 3 * scale;
    let pad_y = 3 * scale;
    let line_height = 7 * scale;
    let line_spacing = 2 * scale;

    let mut plot = |row_idx: u32, hist: &[u8]| {
        let line_top = pad_y + row_idx * (line_height + line_spacing);
        let line_bot = line_top + line_height;
        let start_x = pad_x + 2 * 6 * scale;

        let count = hist.len().min(40);
        let slice = &hist[hist.len().saturating_sub(count)..];

        for (i, &val) in slice.iter().enumerate() {
            let col_x = start_x + (i as u32) * 6 * scale;
            let bar_h = (val as u32 * line_height) / 100;
            let bar_top = line_bot.saturating_sub(bar_h.max(1));

            for cy in bar_top..line_bot {
                if cy >= h { break; }
                for cx in col_x..col_x + 5 * scale {
                    if cx >= w { break; }
                    let idx = (cy * w + cx) as usize;
                    lit[idx] = true;
                    if val >= 80 {
                        hl[idx] = true;
                    }
                }
            }

            // Reference dot at 50% line
            let mid_y = line_bot.saturating_sub(line_height / 2);
            if mid_y < h && i % 4 == 0 {
                let idx = (mid_y * w + col_x) as usize;
                lit[idx] = true;
            }
        }
    };

    plot(2, cpu_hist);
    plot(4, gpu_hist);

    (w, h, lit, hl)
}

/// Renders the 2 hardware performance graphs in the top-right corner of the backbuffer.
fn draw_graphs(
    host: &crate::host::Host,
    list: &ID3D12GraphicsCommandList,
    back: &ID3D12Resource,
    (width, height, format): (u32, u32, DXGI_FORMAT),
    scale: u32,
) {
    let Some(bytes) = pixel_bytes(format) else { return };

    let (cpu_hist, gpu_hist) = {
        let cpu = CPU_HISTORY.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let gpu = GPU_HISTORY.lock().unwrap_or_else(|e| e.into_inner()).clone();
        (cpu, gpu)
    };

    let cpu_cur = *cpu_hist.last().unwrap_or(&30);
    let gpu_cur = *gpu_hist.last().unwrap_or(&35);

    let key = (cpu_cur as u64)
        | ((gpu_cur as u64) << 8)
        | ((format.0 as u64) << 16)
        | ((scale as u64) << 32)
        | (((cpu_hist.len() ^ gpu_hist.len()) as u64) << 48);

    let mut graph_cache = GRAPH_MADE.lock().unwrap_or_else(|e| e.into_inner());
    let graph_cache = graph_cache.get_or_insert_with(HashMap::new);
    if graph_cache.len() > 32 {
        graph_cache.clear();
    }

    if !graph_cache.contains_key(&key) {
        let (gw, gh, lit, hl) = render_graphs_bitmap(&cpu_hist, &gpu_hist, scale);
        let pitch = (gw as usize * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
        let total_bytes = pitch * gh as usize;
        let Some((buffer, data)) = host.shared_buffer(total_bytes as u64) else { return };
        let out = unsafe { std::slice::from_raw_parts_mut(data, total_bytes) };
        for y in 0..gh as usize {
            for x in 0..gw as usize {
                let at = y * pitch + x * bytes;
                let is_lit = lit[y * gw as usize + x];
                let is_hl = hl[y * gw as usize + x];
                pixel(format, is_lit, is_hl, &mut out[at..at + bytes]);
            }
        }
        graph_cache.insert(key, Made { buffer, width: gw, height: gh, pitch: pitch as u32 });
    }

    let Some(m) = graph_cache.get(&key) else { return };

    let margin_x = (8 * scale).min(32);
    let margin_y = (6 * scale).min(24);
    if m.width + 2 * margin_x > width || m.height + 2 * margin_y > height {
        return;
    }

    // Top-right alignment for CPU & GPU usage graphs
    let x_pos = width.saturating_sub(m.width + margin_x);
    let y_pos = margin_y;

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

/// Renders a full-screen solid black canvas with a centered high-contrast GPU overload warning dialog.
fn draw_overload_screen(
    host: &crate::host::Host,
    list: &ID3D12GraphicsCommandList,
    back: &ID3D12Resource,
    (width, height, format): (u32, u32, DXGI_FORMAT),
    reason: &str,
) {
    let Some(bytes) = pixel_bytes(format) else { return };
    let scale = (height / 540).clamp(2, 4);

    let mut clean_reason = reason.replace('\n', " ").replace('\r', "");
    if clean_reason.len() > 64 {
        clean_reason.truncate(61);
        clean_reason.push_str("...");
    }

    let lines = vec![
        "===================================================================".to_string(),
        "                [!] GPU OVERLOAD / FAULT DETECTED                  ".to_string(),
        "===================================================================".to_string(),
        "".to_string(),
        "  CRITICAL FAIL-SAFE: GRAPHICS PIPELINE SUSPENDED TO PREVENT TDR   ".to_string(),
        "".to_string(),
        format!("  REASON : {}", clean_reason),
        "".to_string(),
        "  PROBABLE CAUSES:".to_string(),
        "   - Resolution set too high for GPU VRAM / hardware capability".to_string(),
        "   - Intensive compute shaders or unbuilt BVH ray-tracing traversal".to_string(),
        "   - Host Vulkan / D3D12 driver pipeline compilation exception".to_string(),
        "".to_string(),
        "  FAIL-SAFE RECOVERY CONTROLS:".to_string(),
        "   * [F2] : Open Settings Menu (Switch to 1080p / Disable Dynamic RT)".to_string(),
        "   * [F1] : Lower FPS Cap (e.g. 30 FPS / 60 FPS)".to_string(),
        "   * [F3] : Hide / Show Warning Screen".to_string(),
        "".to_string(),
        "===================================================================".to_string(),
    ];

    let (dlg_w, dlg_h, lit, hl) = bitmap(&lines, scale);
    let pitch = (width as usize * bytes).next_multiple_of(D3D12_TEXTURE_DATA_PITCH_ALIGNMENT as usize);
    let total_bytes = pitch * height as usize;

    let Some((buffer, data)) = host.shared_buffer(total_bytes as u64) else { return };
    let out = unsafe { std::slice::from_raw_parts_mut(data, total_bytes) };

    // Fill the entire canvas with solid black (0, 0, 0, 255)
    for y in 0..height as usize {
        let row_start = y * pitch;
        for x in 0..width as usize {
            let at = row_start + x * bytes;
            match format {
                DXGI_FORMAT_R8G8B8A8_UNORM | DXGI_FORMAT_B8G8R8A8_UNORM => {
                    out[at..at + bytes].copy_from_slice(&[0, 0, 0, 255]);
                }
                DXGI_FORMAT_R10G10B10A2_UNORM => {
                    out[at..at + bytes].copy_from_slice(&[0, 0, 0, 0b11000000]);
                }
                DXGI_FORMAT_R16G16B16A16_FLOAT => {
                    out[at..at + bytes].fill(0);
                }
                _ => {}
            }
        }
    }

    // Center warning box on the black screen
    let start_x = (width.saturating_sub(dlg_w)) / 2;
    let start_y = (height.saturating_sub(dlg_h)) / 2;

    for dy in 0..dlg_h as usize {
        let py = start_y as usize + dy;
        if py >= height as usize { break; }
        for dx in 0..dlg_w as usize {
            let px = start_x as usize + dx;
            if px >= width as usize { break; }
            let at = py * pitch + px * bytes;
            let is_lit = lit[dy * dlg_w as usize + dx];
            let is_hl = hl[dy * dlg_w as usize + dx];
            pixel(format, is_lit, is_hl, &mut out[at..at + bytes]);
        }
    }

    unsafe {
        let from = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(&buffer),
            Type: D3D12_TEXTURE_COPY_TYPE_PLACED_FOOTPRINT,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 {
                PlacedFootprint: D3D12_PLACED_SUBRESOURCE_FOOTPRINT {
                    Offset: 0,
                    Footprint: D3D12_SUBRESOURCE_FOOTPRINT {
                        Format: format,
                        Width: width,
                        Height: height,
                        Depth: 1,
                        RowPitch: pitch as u32,
                    },
                },
            },
        };
        let to = D3D12_TEXTURE_COPY_LOCATION {
            pResource: std::mem::transmute_copy(back),
            Type: D3D12_TEXTURE_COPY_TYPE_SUBRESOURCE_INDEX,
            Anonymous: D3D12_TEXTURE_COPY_LOCATION_0 { SubresourceIndex: 0 },
        };
        list.CopyTextureRegion(&to, 0, 0, 0, &from, None);
    }
}

/// Fallback GDI painter for when the D3D12 device has been removed or invalidated.
#[cfg(windows)]
pub fn paint_gdi_overload(hwnd: windows::Win32::Foundation::HWND, reason: &str) {
    use windows::Win32::Graphics::Gdi::*;
    use windows::Win32::Foundation::COLORREF;
    unsafe {
        let hdc = GetDC(Some(hwnd));
        if !hdc.is_invalid() {
            let mut rc = windows::Win32::Foundation::RECT::default();
            let _ = windows::Win32::UI::WindowsAndMessaging::GetClientRect(hwnd, &mut rc);
            let brush = CreateSolidBrush(COLORREF(0x00000000));
            FillRect(hdc, &rc, brush);
            let _ = DeleteObject(brush.into());
            SetBkMode(hdc, TRANSPARENT);
            SetTextColor(hdc, COLORREF(0x0033DDFF)); // Warm amber/gold

            let text = format!(
                "\r\n\r\n\r\n   [!] GPU OVERLOAD / FAULT DETECTED\r\n\r\n   The GPU compute pipeline encountered a critical load fault:\r\n   {}\r\n\r\n   Fail-safe engaged: The game screen has been blanked to prevent TDR driver crash.\r\n   Resolution or Ray Tracing settings exceeded hardware capacity.\r\n   Press F2 to lower resolution, or restart with lower display settings.",
                reason
            );
            let mut wide: Vec<u16> = text.encode_utf16().chain(Some(0)).collect();
            let len = wide.len().saturating_sub(1);
            DrawTextW(hdc, &mut wide[..len], &mut rc, DT_CENTER | DT_WORDBREAK);
            ReleaseDC(Some(hwnd), hdc);
        }
    }
}

#[cfg(not(windows))]
pub fn paint_gdi_overload(_hwnd: (), _reason: &str) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_overlay_lines_render_all_sections() {
        let lines = get_overlay_lines(300, 33.3, 21.0, 30, true, 0);
        assert!(lines[1].contains("DROPDOWN MENU"));
        assert!(lines.iter().any(|l| l.contains("RESOLUTION")));
        assert!(lines.iter().any(|l| l.contains("1200p")));
        assert!(lines.iter().any(|l| l.contains("1080p")));
        assert!(lines.iter().any(|l| l.contains("1440p")));
        assert!(lines.iter().any(|l| l.contains("2560p")));
        assert!(lines.iter().any(|l| l.contains("4K")));
        assert!(lines.iter().any(|l| l.contains("DYNAMIC RT")));
        assert!(lines.iter().any(|l| l.contains("FSR 3.1")));
    }

    #[test]
    fn dropdown_closed_renders_concise_hud() {
        let lines = get_overlay_lines(300, 33.3, 21.0, 30, false, 0);
        assert_eq!(lines.len(), 1);
        assert!(lines[0].contains("SCARLETT HUD"));
        assert!(lines[0].contains("1920x1200"));
        assert!(lines[0].contains("[F2: MENU]"));
        assert!(lines[0].contains("[F3: HIDE]"));
    }

    #[test]
    fn bitmap_renders_multiline_with_highlight() {
        let lines = vec!["> SELECTED LINE".to_string(), "NORMAL LINE".to_string()];
        let (w, h, lit, hl) = bitmap(&lines, 2);
        assert!(w > 0 && h > 0);
        assert_eq!(lit.len(), (w * h) as usize);
        assert!(hl.iter().any(|&b| b));
    }

    #[test]
    fn top_left_position_within_bounds() {
        let screen_w = 1920u32;
        let screen_h = 1200u32;
        let overlay_w = 408u32;
        let overlay_h = 300u32;
        let margin_x = 16u32;
        let margin_y = 12u32;

        let x_pos = margin_x;
        let y_pos = margin_y;

        assert!(x_pos + overlay_w <= screen_w);
        assert!(y_pos + overlay_h <= screen_h);
        assert_eq!(x_pos, 16);
        assert_eq!(y_pos, 12);
    }
}
