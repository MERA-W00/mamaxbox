use std::sync::atomic::{AtomicU32, Ordering};

/// 0 = Detecting, 1 = PS5 DualSense / Gamepad (Slot 0), 2 = Keyboard Fallback
pub static ACTIVE_DEVICE: AtomicU32 = AtomicU32::new(0);

/// Bitmask of last button pressed
pub static LAST_BUTTON: AtomicU32 = AtomicU32::new(0);

pub fn set_active_device(device: u32) {
    ACTIVE_DEVICE.store(device, Ordering::Relaxed);
}

pub fn get_active_device() -> u32 {
    ACTIVE_DEVICE.load(Ordering::Relaxed)
}

pub fn set_last_button(button: u32) {
    if button != 0 {
        LAST_BUTTON.store(button, Ordering::Relaxed);
    }
}

pub fn get_last_button() -> u32 {
    LAST_BUTTON.load(Ordering::Relaxed)
}
