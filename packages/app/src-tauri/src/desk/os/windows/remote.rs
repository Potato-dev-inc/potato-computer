// Behaviour from desktoplab (github.com/v7ren/desktoplab), commit 6e5360e, MIT.
//! The real Windows side of the desk link: input injection, capture, clipboard. Never constructed in a test build
//! (`link::remote::default_remote`).
//!
//! - **Injection** is `SendInput` for keys, buttons, the wheel and pointer moves. A move is absolute on the virtual
//!   desktop (`cursor::inject_move_to`), so the other computer's acceleration is not applied on top of this one's,
//!   and it is still real mouse input (the pointer shows after typing, hover and drag behave). Every key and button
//!   pressed is remembered and released by `release_injected`.
//! - **UIPI**: Windows drops injected input aimed at a window that runs as admin. Nothing can be done about it from a
//!   normal process; the page says so ("Can't control windows that run as admin.").
//! - **Capture** (`capture.rs`) parks the pointer near the edge it left by (`link::park`) and reports what the person does.

use std::collections::BTreeSet;
use std::mem::size_of;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use desk_proto::hid_to_vk;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    GetAsyncKeyState, GetKeyState, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, INPUT_MOUSE, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY, KEYEVENTF_KEYUP,
    MOUSEEVENTF_HWHEEL, MOUSEEVENTF_LEFTDOWN, MOUSEEVENTF_LEFTUP, MOUSEEVENTF_MIDDLEDOWN, MOUSEEVENTF_MIDDLEUP, MOUSEEVENTF_RIGHTDOWN,
    MOUSEEVENTF_RIGHTUP, MOUSEEVENTF_WHEEL, MOUSEINPUT, VIRTUAL_KEY,
};

use super::{capture, clipboard, cursor, cursor_lock, display, drag};
use crate::desk::host::DisplayInfo;
use crate::desk::link::park::{park_point, PARK_SPAN};
use crate::desk::link::release;
use crate::desk::link::remote::{CaptureSink, ClipItem, DragDone, Inject, PointerSink, RemoteHost};

#[derive(Default)]
struct Held {
    keys: BTreeSet<u16>,
    buttons: BTreeSet<u8>,
}

pub struct WindowsRemote {
    capture: Mutex<Option<capture::CaptureHook>>,
    watch: Mutex<Option<capture::WatchHook>>,
    held: Mutex<Held>,
    /// The screens, read at most every `SCREEN_CACHE` (every injected move needs them).
    screen_cache: Mutex<Option<(Vec<DisplayInfo>, Instant)>>,
}

const SCREEN_CACHE: Duration = Duration::from_millis(500);

impl WindowsRemote {
    /// No side effects: no hook, no key press, no clipboard access.
    pub fn new() -> Self {
        Self { capture: Mutex::new(None), watch: Mutex::new(None), held: Mutex::new(Held::default()), screen_cache: Mutex::new(None) }
    }

    fn screens(&self) -> Result<Vec<DisplayInfo>, String> {
        let mut cache = self.screen_cache.lock().unwrap_or_else(|p| p.into_inner());
        if let Some((list, read)) = cache.as_ref() {
            if read.elapsed() <= SCREEN_CACHE {
                return Ok(list.clone());
            }
        }
        let list = display::list()?;
        *cache = Some((list.clone(), Instant::now()));
        Ok(list)
    }
}

impl Default for WindowsRemote {
    fn default() -> Self {
        Self::new()
    }
}

fn is_extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2D | 0x2E | 0x5B | 0x5C | 0xA3 | 0xA5 | 0x6F)
}

fn send(inputs: &[INPUT]) -> Result<(), String> {
    // SAFETY: `inputs` is a valid slice of fully initialised INPUT structures.
    let sent = unsafe { SendInput(inputs, size_of::<INPUT>() as i32) };
    if sent as usize == inputs.len() {
        Ok(())
    } else {
        Err("Windows refused the input (a window that runs as admin?)".into())
    }
}

fn key_input(vk: u16, down: bool) -> INPUT {
    let mut flags = if down { Default::default() } else { KEYEVENTF_KEYUP };
    if is_extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 { ki: KEYBDINPUT { wVk: VIRTUAL_KEY(vk), wScan: 0, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

fn mouse_input(flags: windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS, data: i32) -> INPUT {
    INPUT {
        r#type: INPUT_MOUSE,
        Anonymous: INPUT_0 { mi: MOUSEINPUT { dx: 0, dy: 0, mouseData: data as u32, dwFlags: flags, time: 0, dwExtraInfo: 0 } },
    }
}

/// The modifier keys the system thinks are down (`GetKeyState`: the state the input queue reports).
fn modifiers_logically_down() -> Vec<u16> {
    // SAFETY: plain call per key.
    release::MODIFIER_VKS.iter().copied().filter(|vk| unsafe { GetKeyState(i32::from(*vk)) } < 0).collect()
}

/// The modifier keys the person is holding right now (`GetAsyncKeyState`).
fn modifiers_physically_down() -> Vec<u16> {
    // SAFETY: plain call per key.
    release::MODIFIER_VKS.iter().copied().filter(|vk| unsafe { GetAsyncKeyState(i32::from(*vk)) } < 0).collect()
}

/// Ends a capture: the hooks go, and the keys that were down when the pointer left and were let go meanwhile are
/// released here, because their release never reached this computer.
fn end_hook(hook: capture::CaptureHook) {
    let (keys, buttons) = hook.stop();
    for vk in keys {
        let _ = send(&[key_input(vk, false)]);
    }
    // A button held when the pointer left (a drag of files) and let go over there. Esc first: if the drag in the
    // app it started in is somehow still alive, the release must not drop the files here.
    if !buttons.is_empty() {
        const VK_ESCAPE: u16 = 0x1B;
        let _ = send(&[key_input(VK_ESCAPE, true), key_input(VK_ESCAPE, false)]);
    }
    for button in buttons {
        let _ = send(&[mouse_input(button_up_flag(button), 0)]);
    }
}

fn button_up_flag(button: u8) -> windows::Win32::UI::Input::KeyboardAndMouse::MOUSE_EVENT_FLAGS {
    match button {
        0 => MOUSEEVENTF_LEFTUP,
        1 => MOUSEEVENTF_RIGHTUP,
        _ => MOUSEEVENTF_MIDDLEUP,
    }
}

fn contains(d: &DisplayInfo, pos: (i32, i32)) -> bool {
    pos.0 >= d.x && pos.0 < d.x + d.width as i32 && pos.1 >= d.y && pos.1 < d.y + d.height as i32
}

/// `p` when it is on a screen, otherwise the nearest point of the nearest screen.
fn onto_a_screen(displays: &[DisplayInfo], p: (i32, i32)) -> (i32, i32) {
    if displays.is_empty() || displays.iter().any(|d| contains(d, p)) {
        return p;
    }
    displays
        .iter()
        .map(|d| (p.0.clamp(d.x, d.x + d.width as i32 - 1), p.1.clamp(d.y, d.y + d.height as i32 - 1)))
        .min_by_key(|c| {
            let (dx, dy) = (i64::from(c.0 - p.0), i64::from(c.1 - p.1));
            dx * dx + dy * dy
        })
        .unwrap_or(p)
}

fn center_of(displays: &[DisplayInfo], pos: (i32, i32)) -> (i32, i32) {
    let d = displays.iter().find(|d| contains(d, pos)).or_else(|| displays.first());
    d.map_or(pos, |d| (d.x + d.width as i32 / 2, d.y + d.height as i32 / 2))
}

impl RemoteHost for WindowsRemote {
    fn displays(&self) -> Result<Vec<DisplayInfo>, String> {
        display::list()
    }

    fn cursor_pos(&self) -> Result<(i32, i32), String> {
        cursor::position()
    }

    fn move_cursor(&self, x: i32, y: i32) -> Result<(), String> {
        cursor::warp(x, y)
    }

    fn buttons_held(&self) -> bool {
        // VK_LBUTTON, VK_RBUTTON, VK_MBUTTON
        // SAFETY: plain calls.
        [0x01, 0x02, 0x04].iter().any(|vk| unsafe { GetAsyncKeyState(*vk) } < 0)
    }

    fn inject(&self, event: Inject) -> Result<(), String> {
        let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
        match event {
            Inject::Key { hid, down } => {
                let vk = hid_to_vk(hid).ok_or("a key this computer does not have")?;
                send(&[key_input(vk, down)])?;
                if down {
                    held.keys.insert(hid);
                } else {
                    held.keys.remove(&hid);
                }
            }
            Inject::Button { button, down } => {
                let flag = match (button, down) {
                    (0, true) => MOUSEEVENTF_LEFTDOWN,
                    (0, false) => MOUSEEVENTF_LEFTUP,
                    (1, true) => MOUSEEVENTF_RIGHTDOWN,
                    (1, false) => MOUSEEVENTF_RIGHTUP,
                    (_, true) => MOUSEEVENTF_MIDDLEDOWN,
                    (_, false) => MOUSEEVENTF_MIDDLEUP,
                };
                send(&[mouse_input(flag, 0)])?;
                if down {
                    held.buttons.insert(button);
                } else {
                    held.buttons.remove(&button);
                }
            }
            Inject::Move { dx, dy } => {
                drop(held);
                let (x, y) = cursor::position()?;
                // Onto a real screen: the virtual desktop has dead corners where screens of different sizes meet.
                let target = onto_a_screen(&self.screens()?, (x.saturating_add(dx), y.saturating_add(dy)));
                cursor::inject_move_to(target.0, target.1)?;
                return Ok(());
            }
            Inject::Wheel { dx, dy } => {
                if dy != 0 {
                    send(&[mouse_input(MOUSEEVENTF_WHEEL, dy)])?;
                }
                if dx != 0 {
                    send(&[mouse_input(MOUSEEVENTF_HWHEEL, dx)])?;
                }
            }
        }
        Ok(())
    }

    fn release_injected(&self) {
        let (keys, buttons) = {
            let mut held = self.held.lock().unwrap_or_else(|p| p.into_inner());
            (std::mem::take(&mut held.keys), std::mem::take(&mut held.buttons))
        };
        for hid in keys {
            if let Some(vk) = hid_to_vk(hid) {
                let _ = send(&[key_input(vk, false)]);
            }
        }
        for button in buttons {
            let _ = send(&[mouse_input(button_up_flag(button), 0)]);
        }
    }

    fn start_capture(&self, sink: CaptureSink, exit: (i32, i32)) -> Result<(), String> {
        let mut slot = self.capture.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Ok(());
        }
        let displays = display::list()?;
        // Park near the exit point, on the monitor it left from. If no monitor holds it, the centre of the monitor
        // the pointer is on.
        let park = match displays.iter().find(|d| contains(d, exit)) {
            Some(d) => park_point(exit, (d.x, d.y, d.width as i32, d.height as i32), PARK_SPAN),
            None => center_of(&displays, cursor::position()?),
        };
        // Remote mode is on before the warp (the hook is installed inside `start`), as DevHop did.
        let hook = capture::start(sink, park, PARK_SPAN)?;
        if let Err(e) = cursor::warp(park.0, park.1) {
            end_hook(hook);
            return Err(e);
        }
        // Clip the PC pointer around the park point (it is hidden later, after the first forwarded motion).
        cursor_lock::engage(park, PARK_SPAN);
        *slot = Some(hook);
        Ok(())
    }

    fn stop_capture(&self) {
        // Clip and hide go first, whatever state the capture is in; safe when nothing is engaged.
        cursor_lock::release();
        let hook = self.capture.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(hook) = hook {
            end_hook(hook);
        }
        // A modifier the system still thinks is down, but the person is not holding, would make every later click
        // here a Ctrl-click or Alt-click. Release those (the same rule the old app applied when remote mode ended).
        for vk in release::modifiers_to_release(&modifiers_logically_down(), &modifiers_physically_down()) {
            let _ = send(&[key_input(vk, false)]);
        }
    }

    fn watch_pointer(&self, sink: PointerSink) -> Result<(), String> {
        let mut slot = self.watch.lock().unwrap_or_else(|p| p.into_inner());
        if slot.is_some() {
            return Ok(());
        }
        *slot = Some(capture::start_watch(sink)?);
        Ok(())
    }

    fn unwatch_pointer(&self) {
        let hook = self.watch.lock().unwrap_or_else(|p| p.into_inner()).take();
        if let Some(hook) = hook {
            hook.stop();
        }
    }

    fn drag_strip(&self, rect: Option<(i32, i32, i32, i32)>) {
        drag::strip(rect);
    }

    fn dragged_files(&self) -> Option<Vec<PathBuf>> {
        drag::seen()
    }

    fn drag_reset(&self) {
        drag::reset();
    }

    fn cancel_local_drag(&self) {
        // Esc ends the drag in the app it started in, without a drop (the capture hook lets injected keys through).
        const VK_ESCAPE: u16 = 0x1B;
        let _ = send(&[key_input(VK_ESCAPE, true), key_input(VK_ESCAPE, false)]);
    }

    fn begin_file_drag(&self, items: Vec<PathBuf>, done: DragDone) -> Result<(), String> {
        drag::begin(items, done)
    }

    fn end_file_drag(&self, drop: bool) {
        drag::end(drop);
        if !drop {
            // Nothing else will move: wake the drag with a move one pixel over and back.
            if let Ok((x, y)) = cursor::position() {
                let _ = cursor::inject_move_to(x + 1, y);
                let _ = cursor::inject_move_to(x, y);
            }
        }
    }

    fn reveal(&self, path: &Path) {
        use std::os::windows::process::CommandExt;
        let _ = std::process::Command::new("explorer.exe").raw_arg(format!("/select,\"{}\"", path.display())).spawn();
    }

    fn clipboard_read(&self) -> Result<Vec<ClipItem>, String> {
        clipboard::read()
    }

    fn clipboard_write(&self, items: &[ClipItem]) -> Result<(), String> {
        clipboard::write(items)
    }
}
