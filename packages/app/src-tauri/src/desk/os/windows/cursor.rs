// Behaviour from desktoplab (github.com/v7ren/desktoplab), commit 6e5360e, MIT.
//! Cursor position and move. DevHop dropped the result of `SetCursorPos`; failures are reported here.
//!
//! Moves made for another computer (`inject_move_to`) are real mouse input (`SendInput`, absolute on the virtual
//! desktop), not `SetCursorPos`: Windows then treats them like a mouse (the pointer shows again after typing, hover
//! and drag work everywhere, the idle timer resets) and applies no acceleration on top of the other computer's.
//! `SendInput` is processed asynchronously, so `GetCursorPos` right after it can still say the old point: the
//! point this file put the pointer on is remembered for a moment and wins over the system's answer (the macOS side
//! does the same).

use std::mem::size_of;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::POINT;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    SendInput, INPUT, INPUT_0, INPUT_MOUSE, MOUSEEVENTF_ABSOLUTE, MOUSEEVENTF_MOVE, MOUSEEVENTF_VIRTUALDESK, MOUSEINPUT,
};
use windows::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, GetSystemMetrics, SetCursorPos, SM_CXVIRTUALSCREEN, SM_CYVIRTUALSCREEN, SM_XVIRTUALSCREEN, SM_YVIRTUALSCREEN,
};

use super::dpi::DpiScope;

/// How long a point this file put the pointer on is trusted over `GetCursorPos`.
const TRACKED_MAX_AGE: Duration = Duration::from_millis(150);

static TRACKED: Mutex<Option<((i32, i32), Instant)>> = Mutex::new(None);

fn note(x: i32, y: i32) {
    *TRACKED.lock().unwrap_or_else(|p| p.into_inner()) = Some(((x, y), Instant::now()));
}

fn tracked() -> Option<(i32, i32)> {
    TRACKED.lock().unwrap_or_else(|p| p.into_inner()).filter(|(_, at)| at.elapsed() <= TRACKED_MAX_AGE).map(|(p, _)| p)
}

/// The system's answer, ignoring what this file remembers.
pub fn system_position() -> Result<(i32, i32), String> {
    let _dpi = DpiScope::new();
    let mut pt = POINT::default();
    // SAFETY: `pt` is a valid out pointer for the call.
    unsafe { GetCursorPos(&mut pt) }.map_err(|e| format!("GetCursorPos failed: {e}"))?;
    Ok((pt.x, pt.y))
}

pub fn position() -> Result<(i32, i32), String> {
    match tracked() {
        Some(p) => Ok(p),
        None => system_position(),
    }
}

pub fn warp(x: i32, y: i32) -> Result<(), String> {
    let _dpi = DpiScope::new();
    // SAFETY: plain call.
    unsafe { SetCursorPos(x, y) }.map_err(|e| format!("SetCursorPos failed: {e}"))?;
    note(x, y);
    Ok(())
}

/// The warp the capture hook makes back to the park point: not remembered (the parked pointer is read by nobody, and
/// a remembered park point would hide where the pointer really is once the capture ends).
pub fn warp_quiet(x: i32, y: i32) {
    let _dpi = DpiScope::new();
    // SAFETY: plain call.
    let _ = unsafe { SetCursorPos(x, y) };
}

/// The 0..=65535 absolute coordinate for pixel `v` of a virtual desktop `len` pixels long starting at `origin`:
/// aimed at the middle of the pixel, so either rounding Windows applies lands on it, the last pixel included.
pub fn normalize(v: i32, origin: i32, len: i32) -> i32 {
    let len = i64::from(len.max(1));
    let off = (i64::from(v) - i64::from(origin)).clamp(0, len - 1);
    ((off * 2 + 1) * 65_536 / (len * 2)).clamp(0, 65_535) as i32
}

/// Moves the pointer to `(x, y)` (physical desktop pixels) as real mouse input.
pub fn inject_move_to(x: i32, y: i32) -> Result<(), String> {
    let _dpi = DpiScope::new();
    // SAFETY: plain calls; the thread is per-monitor aware, so these are physical pixels.
    let (vx, vy, vw, vh) = unsafe {
        (
            GetSystemMetrics(SM_XVIRTUALSCREEN),
            GetSystemMetrics(SM_YVIRTUALSCREEN),
            GetSystemMetrics(SM_CXVIRTUALSCREEN),
            GetSystemMetrics(SM_CYVIRTUALSCREEN),
        )
    };
    if vw <= 0 || vh <= 0 {
        return warp(x, y);
    }
    let input = INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 {
            mi: MOUSEINPUT {
                dx: normalize(x, vx, vw),
                dy: normalize(y, vy, vh),
                mouseData: 0,
                dwFlags: MOUSEEVENTF_MOVE | MOUSEEVENTF_ABSOLUTE | MOUSEEVENTF_VIRTUALDESK,
                time: 0,
                dwExtraInfo: 0,
            },
        },
    };
    // SAFETY: one fully initialised INPUT.
    let sent = unsafe { SendInput(&[input], size_of::<INPUT>() as i32) };
    if sent != 1 {
        // Blocked (secure desktop, an admin window in front): place it the old way so the pointer still follows.
        return warp(x, y);
    }
    note(x, y);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::normalize;

    /// Windows' mapping of an absolute coordinate back to a pixel of the virtual desktop.
    fn back(n: i32, origin: i32, len: i32) -> i32 {
        origin + (i64::from(n) * i64::from(len) / 65_536) as i32
    }

    #[test]
    fn every_pixel_maps_back_to_itself() {
        for (origin, len) in [(0, 1920), (-1920, 3840), (0, 2560), (-1280, 4480), (0, 1366), (0, 2048), (100, 7)] {
            for v in origin..origin + len {
                assert_eq!(back(normalize(v, origin, len), origin, len), v, "pixel {v} of {len} at {origin}");
            }
        }
    }

    #[test]
    fn points_off_the_desktop_are_clamped_to_its_edge() {
        assert_eq!(normalize(-50, 0, 1920), normalize(0, 0, 1920));
        assert_eq!(normalize(5000, 0, 1920), normalize(1919, 0, 1920));
    }
}
