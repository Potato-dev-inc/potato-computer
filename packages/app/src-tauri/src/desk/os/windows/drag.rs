//! Files dragged across, the Windows side.
//!
//! - **Out**: Windows does not say what is being dragged, except to a window the drag passes over. While a button
//!   is held near an edge facing a linked computer, a strip window (topmost, almost fully transparent, never
//!   activated) covers that edge and is an OLE drop target: a drag of files that reaches it is read (`CF_HDROP`)
//!   and remembered. A drop on the strip itself does nothing. The strip goes when the button is let go.
//! - **In**: `begin` starts a real `DoDragDrop` of staged files on a thread of its own. The data object is the
//!   shell's own for those files (what Explorer would offer), so every drop target takes it. The drop source does
//!   not look at the mouse buttons (the person's hand is on the other computer): it drops or cancels when the hub
//!   says so (`end`), which a mouse event then wakes.
//!
//! Everything here runs on its own threads with their own message loops; the hub only posts to them.

use std::ffi::c_void;
use std::path::PathBuf;
use std::sync::atomic::{AtomicIsize, AtomicU8, Ordering};
use std::sync::{Mutex, OnceLock};

use windows::core::{implement, Ref, HRESULT, PCWSTR};
use windows::Win32::Foundation::{
    DRAGDROP_S_CANCEL, DRAGDROP_S_DROP, DRAGDROP_S_USEDEFAULTCURSORS, HWND, LPARAM, LRESULT, POINTL, S_OK, WPARAM,
};
use windows::Win32::System::Com::{CoTaskMemFree, IDataObject, DVASPECT_CONTENT, FORMATETC, TYMED_HGLOBAL};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::System::Ole::{
    DoDragDrop, IDropSource, IDropSource_Impl, IDropTarget, IDropTarget_Impl, OleInitialize, RegisterDragDrop, ReleaseStgMedium,
    RevokeDragDrop, CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_NONE,
};
use windows::Win32::System::SystemServices::MODIFIERKEYS_FLAGS;
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{DragQueryFileW, SHCreateShellItemArrayFromIDLists, SHParseDisplayName, BHID_DataObject, HDROP};
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DispatchMessageW, GetMessageW, PostMessageW, RegisterClassW, SetLayeredWindowAttributes,
    SetWindowPos, ShowWindow, TranslateMessage, HWND_TOPMOST, LWA_ALPHA, MSG, SWP_NOACTIVATE, SWP_SHOWWINDOW, SW_HIDE, WM_APP,
    WNDCLASSW, WS_EX_LAYERED, WS_EX_NOACTIVATE, WS_EX_TOOLWINDOW, WS_EX_TOPMOST, WS_POPUP,
};

use super::dpi::DpiScope;
use crate::desk::link::remote::DragDone;

const WM_STRIP: u32 = WM_APP + 41;

// ---- out: the strip ---------------------------------------------------------------------------------------

/// Where the strip should be (`None`: hidden). Read by the strip thread when it is poked.
static STRIP_AT: Mutex<Option<(i32, i32, i32, i32)>> = Mutex::new(None);
/// Files seen in a drag over the strip, since the last reset.
static SEEN: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);
/// The strip window (0 until its thread made it).
static STRIP_HWND: AtomicIsize = AtomicIsize::new(0);
static STRIP_THREAD: OnceLock<()> = OnceLock::new();

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

/// Shows the strip at `rect` (desktop pixels) or hides it.
pub fn strip(rect: Option<(i32, i32, i32, i32)>) {
    *lock(&STRIP_AT) = rect;
    if rect.is_some() {
        STRIP_THREAD.get_or_init(|| {
            let _ = std::thread::Builder::new().name("desk-drag-strip".into()).spawn(strip_thread);
        });
    }
    let hwnd = STRIP_HWND.load(Ordering::SeqCst);
    if hwnd != 0 {
        // SAFETY: posting to a window this module made.
        unsafe {
            let _ = PostMessageW(Some(HWND(hwnd as *mut c_void)), WM_STRIP, WPARAM(0), LPARAM(0));
        }
    }
}

/// The files of the drag that passed over the strip, if any.
pub fn seen() -> Option<Vec<PathBuf>> {
    lock(&SEEN).clone()
}

/// Forgets the drag and hides the strip.
pub fn reset() {
    *lock(&SEEN) = None;
    strip(None);
}

fn place_strip(hwnd: HWND) {
    let at = *lock(&STRIP_AT);
    // SAFETY: plain calls on our own window.
    unsafe {
        match at {
            Some((x, y, w, h)) => {
                let _ = SetWindowPos(hwnd, Some(HWND_TOPMOST), x, y, w, h, SWP_NOACTIVATE | SWP_SHOWWINDOW);
            }
            None => {
                let _ = ShowWindow(hwnd, SW_HIDE);
            }
        }
    }
}

unsafe extern "system" fn strip_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    if msg == WM_STRIP {
        place_strip(hwnd);
        return LRESULT(0);
    }
    DefWindowProcW(hwnd, msg, wparam, lparam)
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}

fn strip_thread() {
    let _dpi = DpiScope::new();
    let class = wide("PmDeskDragStrip");
    // SAFETY: a window class and window owned by this thread, an OLE drop target registered on it, and the message
    // loop they need. Everything lives as long as the thread (the app's lifetime).
    unsafe {
        if OleInitialize(None).is_err() {
            tracing::warn!("desk: OLE did not start, dragging files out will not work");
            return;
        }
        let Ok(module) = GetModuleHandleW(None) else { return };
        let wc = WNDCLASSW { lpfnWndProc: Some(strip_proc), hInstance: module.into(), lpszClassName: PCWSTR(class.as_ptr()), ..Default::default() };
        RegisterClassW(&wc);
        let Ok(hwnd) = CreateWindowExW(
            WS_EX_LAYERED | WS_EX_TOPMOST | WS_EX_TOOLWINDOW | WS_EX_NOACTIVATE,
            PCWSTR(class.as_ptr()),
            PCWSTR(class.as_ptr()),
            WS_POPUP,
            0,
            0,
            1,
            1,
            None,
            None,
            Some(module.into()),
            None,
        ) else {
            return;
        };
        // Alpha 1 of 255: invisible, but still a window the drag can be over (alpha 0 would let it fall through).
        let _ = SetLayeredWindowAttributes(hwnd, windows::Win32::Foundation::COLORREF(0), 1, LWA_ALPHA);
        let target: IDropTarget = StripTarget.into();
        if RegisterDragDrop(hwnd, &target).is_err() {
            tracing::warn!("desk: could not watch the screen edge for dragged files");
        }
        STRIP_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
        place_strip(hwnd);
        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).0 > 0 {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = RevokeDragDrop(hwnd);
    }
}

/// The paths in a data object's `CF_HDROP`, if it has one.
fn hdrop_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let fmt = FORMATETC { cfFormat: CF_HDROP.0, ptd: std::ptr::null_mut(), dwAspect: DVASPECT_CONTENT.0, lindex: -1, tymed: TYMED_HGLOBAL.0 as u32 };
    // SAFETY: a standard GetData for CF_HDROP; the medium is released below.
    unsafe {
        let mut medium = data.GetData(&fmt).ok()?;
        let hdrop = HDROP(medium.u.hGlobal.0);
        let count = DragQueryFileW(hdrop, u32::MAX, None);
        let mut out = Vec::new();
        for i in 0..count.min(10_000) {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            out.push(PathBuf::from(String::from_utf16_lossy(&buf[..got.min(len)])));
        }
        ReleaseStgMedium(&mut medium);
        (!out.is_empty()).then_some(out)
    }
}

#[implement(IDropTarget)]
struct StripTarget;

impl IDropTarget_Impl for StripTarget_Impl {
    fn DragEnter(&self, data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        let files = data.ok().ok().and_then(hdrop_paths);
        let any = files.is_some();
        if any {
            *lock(&SEEN) = files;
        }
        if !effect.is_null() {
            // SAFETY: OLE hands a valid out pointer.
            unsafe { *effect = if any { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        }
        Ok(())
    }

    fn DragOver(&self, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        if !effect.is_null() {
            let any = lock(&SEEN).is_some();
            // SAFETY: OLE hands a valid out pointer.
            unsafe { *effect = if any { DROPEFFECT_COPY } else { DROPEFFECT_NONE } };
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        Ok(())
    }

    fn Drop(&self, _data: Ref<IDataObject>, _keys: MODIFIERKEYS_FLAGS, _pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        // Dropped on the strip without crossing: nothing happens to the files.
        if !effect.is_null() {
            // SAFETY: OLE hands a valid out pointer.
            unsafe { *effect = DROPEFFECT_NONE };
        }
        Ok(())
    }
}

// ---- in: a real drag of staged files ----------------------------------------------------------------------

const RUNNING: u8 = 0;
const DROP: u8 = 1;
const CANCEL: u8 = 2;
const IDLE: u8 = 3;
static STATE: AtomicU8 = AtomicU8::new(IDLE);

#[implement(IDropSource)]
struct HubSource;

impl IDropSource_Impl for HubSource_Impl {
    fn QueryContinueDrag(&self, escape: windows::core::BOOL, _keys: MODIFIERKEYS_FLAGS) -> HRESULT {
        match STATE.load(Ordering::SeqCst) {
            DROP => DRAGDROP_S_DROP,
            CANCEL => DRAGDROP_S_CANCEL,
            _ if escape.as_bool() => DRAGDROP_S_CANCEL,
            _ => S_OK,
        }
    }

    fn GiveFeedback(&self, _effect: DROPEFFECT) -> HRESULT {
        DRAGDROP_S_USEDEFAULTCURSORS
    }
}

/// Starts a drag of `items` under the pointer. `done(accepted)` runs when it ends.
pub fn begin(items: Vec<PathBuf>, done: DragDone) -> Result<(), String> {
    if STATE.compare_exchange(IDLE, RUNNING, Ordering::SeqCst, Ordering::SeqCst).is_err() {
        return Err("a drag is already running here".into());
    }
    let spawned = std::thread::Builder::new().name("desk-drag-in".into()).spawn(move || {
        let accepted = run_drag(&items);
        STATE.store(IDLE, Ordering::SeqCst);
        done(accepted);
    });
    if spawned.is_err() {
        STATE.store(IDLE, Ordering::SeqCst);
        return Err("could not start the drag".into());
    }
    Ok(())
}

/// Drops (`drop`) or cancels the running drag. The caller wakes it with a mouse event (the button up it injects).
pub fn end(drop: bool) {
    let _ = STATE.compare_exchange(RUNNING, if drop { DROP } else { CANCEL }, Ordering::SeqCst, Ordering::SeqCst);
}

fn run_drag(items: &[PathBuf]) -> bool {
    let _dpi = DpiScope::new();
    // SAFETY: OLE on this thread; the item ids are freed after the shell made its data object from them.
    unsafe {
        if OleInitialize(None).is_err() {
            return false;
        }
        let mut pidls: Vec<*mut ITEMIDLIST> = Vec::new();
        for item in items {
            let w = wide(&item.to_string_lossy());
            let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
            if SHParseDisplayName(PCWSTR(w.as_ptr()), None, &mut pidl, 0, None).is_ok() && !pidl.is_null() {
                pidls.push(pidl);
            }
        }
        let data: Option<IDataObject> = if pidls.is_empty() {
            None
        } else {
            let consts: Vec<*const ITEMIDLIST> = pidls.iter().map(|p| *p as *const ITEMIDLIST).collect();
            SHCreateShellItemArrayFromIDLists(&consts).ok().and_then(|arr| arr.BindToHandler(None, &BHID_DataObject).ok())
        };
        for p in pidls {
            CoTaskMemFree(Some(p as *const c_void));
        }
        let Some(data) = data else { return false };
        let source: IDropSource = HubSource.into();
        let mut effect = DROPEFFECT_NONE;
        let hr = DoDragDrop(&data, &source, DROPEFFECT_COPY, &mut effect);
        hr == DRAGDROP_S_DROP && effect != DROPEFFECT_NONE
    }
}
