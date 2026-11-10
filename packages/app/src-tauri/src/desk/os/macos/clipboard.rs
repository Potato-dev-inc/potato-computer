// Behaviour from desktoplab (github.com/v7ren/desktoplab), commit 6e5360e, MIT.
//! The general pasteboard (NSPasteboard) through the Objective-C runtime, declared by hand in `ffi.rs`. Text and one
//! PNG image, like the Windows side. Every call runs inside its own autorelease pool because the link's threads
//! have none.

use std::ffi::{c_void, CStr};
use std::os::raw::c_char;

use super::ffi;
use crate::desk::link::remote::ClipItem;

type Id = *mut c_void;
type Sel = *mut c_void;

const UTF8_ENCODING: usize = 4;
const TEXT_TYPE: &str = "public.utf8-plain-text";
const PNG_TYPE: &str = "public.png";

macro_rules! sel {
    ($name:literal) => {
        ffi::sel_registerName(concat!($name, "\0").as_ptr() as *const c_char)
    };
}

// The runtime's one entry point is cast to the exact signature of each message. SAFETY for all of these: callers
// pass objects of the right class for the selector.
unsafe fn send0(recv: Id, sel: Sel) -> Id {
    let f: unsafe extern "C" fn(Id, Sel) -> Id = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(recv, sel)
}
unsafe fn send0_usize(recv: Id, sel: Sel) -> usize {
    let f: unsafe extern "C" fn(Id, Sel) -> usize = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(recv, sel)
}
unsafe fn send0_ptr(recv: Id, sel: Sel) -> *const u8 {
    let f: unsafe extern "C" fn(Id, Sel) -> *const u8 = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(recv, sel)
}
unsafe fn send1(recv: Id, sel: Sel, a: Id) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, Id) -> Id = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(recv, sel, a)
}
unsafe fn send2_bool(recv: Id, sel: Sel, a: Id, b: Id) -> bool {
    let f: unsafe extern "C" fn(Id, Sel, Id, Id) -> bool = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(recv, sel, a, b)
}

/// Drains the pool when it goes out of scope, whichever way the function returns.
struct Pool(Id);

impl Pool {
    fn new() -> Self {
        // SAFETY: NSAutoreleasePool is part of Foundation, which is linked.
        unsafe {
            let class = ffi::objc_getClass(b"NSAutoreleasePool\0".as_ptr() as *const c_char);
            Pool(send0(send0(class, sel!("alloc")), sel!("init")))
        }
    }
}

impl Drop for Pool {
    fn drop(&mut self) {
        // SAFETY: the pool made in `new`.
        unsafe {
            send0(self.0, sel!("drain"));
        }
    }
}

unsafe fn class(name: &'static str) -> Id {
    // `name` is a literal at every call site; it is copied to get its terminating zero.
    let mut c = name.as_bytes().to_vec();
    c.push(0);
    ffi::objc_getClass(c.as_ptr() as *const c_char)
}

/// An autoreleased NSString.
unsafe fn nsstring(s: &str) -> Id {
    let alloc = send0(class("NSString"), sel!("alloc"));
    let f: unsafe extern "C" fn(Id, Sel, *const u8, usize, usize) -> Id =
        std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    let made = f(alloc, sel!("initWithBytes:length:encoding:"), s.as_ptr(), s.len(), UTF8_ENCODING);
    send0(made, sel!("autorelease"))
}

/// An autoreleased NSData holding a copy of `bytes`.
unsafe fn nsdata(bytes: &[u8]) -> Id {
    let f: unsafe extern "C" fn(Id, Sel, *const u8, usize) -> Id =
        std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
    f(class("NSData"), sel!("dataWithBytes:length:"), bytes.as_ptr(), bytes.len())
}

unsafe fn pasteboard() -> Result<Id, String> {
    let pb = send0(class("NSPasteboard"), sel!("generalPasteboard"));
    if pb.is_null() {
        Err("could not open the clipboard".into())
    } else {
        Ok(pb)
    }
}

pub fn read() -> Result<Vec<ClipItem>, String> {
    let _pool = Pool::new();
    let mut items = Vec::new();
    // SAFETY: `pb` is the shared pasteboard; every object returned is used before the pool drains.
    unsafe {
        let pb = pasteboard()?;
        let text = send1(pb, sel!("stringForType:"), nsstring(TEXT_TYPE));
        if !text.is_null() {
            let utf8 = send0_ptr(text, sel!("UTF8String")) as *const c_char;
            if !utf8.is_null() {
                items.push(ClipItem::Text(CStr::from_ptr(utf8).to_string_lossy().into_owned()));
            }
        }
        let png = send1(pb, sel!("dataForType:"), nsstring(PNG_TYPE));
        if !png.is_null() {
            let len = send0_usize(png, sel!("length"));
            let bytes = send0_ptr(png, sel!("bytes"));
            if len > 0 && !bytes.is_null() {
                items.push(ClipItem::Png(std::slice::from_raw_parts(bytes, len).to_vec()));
            }
        }
    }
    Ok(items)
}

pub fn write(items: &[ClipItem]) -> Result<(), String> {
    let _pool = Pool::new();
    // SAFETY: as in `read`.
    unsafe {
        let pb = pasteboard()?;
        send0_usize(pb, sel!("clearContents"));
        for item in items {
            let ok = match item {
                ClipItem::Text(t) => send2_bool(pb, sel!("setString:forType:"), nsstring(t), nsstring(TEXT_TYPE)),
                ClipItem::Png(b) => send2_bool(pb, sel!("setData:forType:"), nsdata(b), nsstring(PNG_TYPE)),
            };
            if !ok {
                return Err("macOS would not take the copied item".into());
            }
        }
    }
    Ok(())
}

// ---- the drag pasteboard: what Finder (or any app) is dragging right now -------------------------------------

/// `NSPasteboardNameDrag`.
const DRAG_PASTEBOARD: &str = "Apple CFPasteboard drag";
const FILE_URL_TYPE: &str = "public.file-url";

unsafe fn drag_pasteboard() -> Id {
    send1(class("NSPasteboard"), sel!("pasteboardWithName:"), nsstring(DRAG_PASTEBOARD))
}

/// Changes every time a drag starts anywhere (the drag pasteboard is written then). Keeps an old drag's files from
/// being taken for a new one.
pub fn drag_change_count() -> usize {
    let _pool = Pool::new();
    // SAFETY: plain messages to the shared drag pasteboard.
    unsafe {
        let pb = drag_pasteboard();
        if pb.is_null() {
            return 0;
        }
        send0_usize(pb, sel!("changeCount"))
    }
}

/// The files on the drag pasteboard.
pub fn drag_files() -> Vec<std::path::PathBuf> {
    let _pool = Pool::new();
    let mut out = Vec::new();
    // SAFETY: plain messages; every object read here is autoreleased into the pool above.
    unsafe {
        let pb = drag_pasteboard();
        if pb.is_null() {
            return out;
        }
        let items = send0(pb, sel!("pasteboardItems"));
        if items.is_null() {
            return out;
        }
        let count = send0_usize(items, sel!("count"));
        let at: unsafe extern "C" fn(Id, Sel, usize) -> Id = std::mem::transmute(ffi::objc_msgSend as unsafe extern "C" fn());
        for i in 0..count.min(10_000) {
            let item = at(items, sel!("objectAtIndex:"), i);
            if item.is_null() {
                continue;
            }
            let s = send1(item, sel!("stringForType:"), nsstring(FILE_URL_TYPE));
            if s.is_null() {
                continue;
            }
            let url = send1(class("NSURL"), sel!("URLWithString:"), s);
            if url.is_null() {
                continue;
            }
            // Finder hands file reference URLs (file:///.file/id=...): resolve them to a path URL first.
            let resolved = send0(url, sel!("filePathURL"));
            let url = if resolved.is_null() { url } else { resolved };
            let path = send0(url, sel!("path"));
            if path.is_null() {
                continue;
            }
            let utf8 = send0_ptr(path, sel!("UTF8String")) as *const c_char;
            if !utf8.is_null() {
                out.push(std::path::PathBuf::from(CStr::from_ptr(utf8).to_string_lossy().into_owned()));
            }
        }
    }
    out
}
