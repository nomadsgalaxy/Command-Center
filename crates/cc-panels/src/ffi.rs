//! The guard every callback from C goes through (FreeRDP's in rdp.rs and clipboard/cliprdr.rs,
//! libvncclient's in vnc.rs). Since Rust 1.81 a panic that unwinds into C aborts the process, and
//! that takes every panel down with it, not just the session whose callback broke. So each
//! callback's body runs in guard(): a panic is caught, logged with the callback's name, and the
//! callback returns its own failure value instead. The session then fails or reconnects the way it
//! would for any other error.
//!
//! The callbacks don't unwrap locks either. A lock poisoned by an earlier caught panic still hands
//! out its state (lock()), because the alternative is a second panic in every callback after it.
use std::cell::Cell;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard};

thread_local! {
    static GUARDED: Cell<u32> = const { Cell::new(0) };
}

/// Runs a callback's body. If it panics, this logs it with `name` and returns `fail`.
pub fn guard<R>(name: &str, fail: R, body: impl FnOnce() -> R) -> R {
    GUARDED.set(GUARDED.get() + 1);
    let r = catch_unwind(AssertUnwindSafe(body));
    GUARDED.set(GUARDED.get() - 1);
    r.unwrap_or_else(|e| {
        let why = e.downcast_ref::<&str>().copied().or_else(|| e.downcast_ref::<String>().map(String::as_str)).unwrap_or("?");
        eprintln!("{name}: panicked ({why}); the callback failed instead, and cc-panels carries on");
        fail
    })
}

/// Whether this thread is inside guard(), so a panic here gets caught. The panic hook uses it to
/// keep the laser, which it otherwise gives back because the process is about to end.
pub fn guarded() -> bool {
    GUARDED.get() > 0
}

/// Locks m, and takes its state even if a caught panic poisoned it.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A callback the way C calls one: through an extern "C" fn pointer. Its body panics.
    unsafe extern "C" fn breaks(x: i32) -> i32 {
        guard("breaks", -1, || if x > 0 { panic!("a bad packet") } else { x })
    }

    #[test]
    fn a_panicking_callback_fails_and_the_process_lives() {
        let f: unsafe extern "C" fn(i32) -> i32 = breaks;
        assert_eq!(unsafe { f(1) }, -1, "the failure value");
        assert_eq!(unsafe { f(0) }, 0, "a body that doesn't panic returns its own value");
        assert!(!guarded());
    }

    #[test]
    fn a_poisoned_lock_still_opens() {
        let m = Mutex::new(5);
        let _ = guard("poisons", (), || {
            let _g = m.lock().unwrap();
            panic!("while holding it");
        });
        assert!(m.is_poisoned());
        *lock(&m) += 1;
        assert_eq!(*lock(&m), 6);
    }
}
