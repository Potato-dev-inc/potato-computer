//! The operating-system side of My computers. Windows and macOS have real hosts; everything else gets a stub that
//! says "not supported yet". Test builds always get the stub, so no test can ever reach the real PC.

use std::sync::Arc;

use super::host::DeskHost;

pub mod stub;
#[cfg(windows)]
#[cfg_attr(test, allow(dead_code))]
pub mod windows;
#[cfg(target_os = "macos")]
#[cfg_attr(test, allow(dead_code))]
pub mod macos;

/// The host the app uses. In a test build this is the stub on every OS.
pub fn default_host() -> Arc<dyn DeskHost> {
    #[cfg(all(windows, not(test)))]
    {
        Arc::new(windows::WindowsHost::new())
    }
    #[cfg(all(target_os = "macos", not(test)))]
    {
        Arc::new(macos::MacosHost::new())
    }
    #[cfg(not(any(all(windows, not(test)), all(target_os = "macos", not(test)))))]
    {
        Arc::new(stub::StubHost)
    }
}

/// Raises the calling thread's priority: for the threads every key and pointer move passes through (the hub, the
/// capture hook). A busy computer then delays the pointer less. Never fails; a refusal leaves the priority as it was.
pub fn boost_thread() {
    #[cfg(windows)]
    {
        use ::windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_HIGHEST};
        // SAFETY: plain call on the current thread's pseudo handle.
        unsafe {
            let _ = SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST);
        }
    }
    #[cfg(target_os = "macos")]
    {
        extern "C" {
            fn pthread_set_qos_class_self_np(qos: u32, relative_priority: i32) -> i32;
        }
        const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
        // SAFETY: plain call on the current thread.
        unsafe {
            let _ = pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
        }
    }
}
