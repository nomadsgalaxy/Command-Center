//! The guard every callback from C goes through (FreeRDP's in rdp.rs and clipboard/cliprdr.rs,
//! libvncclient's in vnc.rs). Since Rust 1.81 a panic that unwinds into C aborts the process, and
//! that takes every panel down with it, not just the session whose callback broke. So each
//! callback's body runs in guard(): a panic is caught, logged with the callback's name, and the
//! callback returns its own failure value instead. The session then fails or reconnects the way it
//! would for any other error.
//!
//! A caught panic also marks this thread's session as failed (session_failed), and the session
//! loops in rdp.rs and vnc.rs end the session then, so it reconnects from a clean start instead of
//! going on with whatever state the panic left. Each callback's panic is logged once per session.
//!
//! The callbacks don't unwrap locks either. A lock poisoned by an earlier caught panic still hands
//! out its state (lock()), because the alternative is a second panic in every callback after it.
//! Where that state could be half-changed and is shared by every session (the clipboard's HUB and
//! CHANS), lock_or_reset() starts it over instead.
use std::cell::{Cell, RefCell};
use std::io::Write;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{Mutex, MutexGuard};

thread_local! {
    static GUARDED: Cell<u32> = const { Cell::new(0) };
    /// A callback on this thread (its session's thread) panicked since the session began.
    static FAILED: Cell<bool> = const { Cell::new(false) };
    /// The callbacks whose panic this session already logged.
    static SAID: RefCell<Vec<&'static str>> = const { RefCell::new(Vec::new()) };
}

/// Runs a callback's body. If it panics, this logs it with `name` (once per session), marks the
/// session failed, and returns `fail`.
pub fn guard<R>(name: &'static str, fail: R, body: impl FnOnce() -> R) -> R {
    caught(name, fail, body, true)
}

/// guard's catch, marking the session failed or not, and saying which in its one log line.
fn caught<R>(name: &'static str, fail: R, body: impl FnOnce() -> R, fails: bool) -> R {
    GUARDED.set(GUARDED.get() + 1);
    let r = catch_unwind(AssertUnwindSafe(body));
    GUARDED.set(GUARDED.get() - 1);
    r.unwrap_or_else(|e| {
        if fails {
            FAILED.set(true);
        }
        let first = SAID.with_borrow_mut(|s| {
            !s.contains(&name) && {
                s.push(name);
                true
            }
        });
        if first {
            let why = e.downcast_ref::<&str>().copied().or_else(|| e.downcast_ref::<String>().map(String::as_str)).unwrap_or("?");
            // writeln, not eprintln: a closed stderr mustn't panic again in here
            let then = if fails { "the callback failed, and its session reconnects" } else { "it was only logging, so the session carries on" };
            let _ = writeln!(std::io::stderr(), "{name}: panicked ({why}); {then}");
        }
        fail
    })
}

/// guard() for work beside a callback's job, like its logging: a panic in it is logged once the
/// same way, but the session carries on, so the job itself still runs after it.
pub fn aside(name: &'static str, body: impl FnOnce()) {
    caught(name, (), body, false)
}

/// Whether a callback on this thread panicked since new_session(). Its session should end.
pub fn session_failed() -> bool {
    FAILED.get()
}

/// A session starts on this thread: nothing has failed, and nothing has been logged yet.
pub fn new_session() {
    FAILED.set(false);
    SAID.with_borrow_mut(Vec::clear);
}

/// Whether this thread is inside guard(), so a panic here gets caught. The panic hook uses it to
/// keep the laser, which it otherwise gives back because the process is about to end.
pub fn guarded() -> bool {
    GUARDED.get() > 0
}

/// Locks m, and takes its state even if a caught panic poisoned it. For a lock whose state a panic
/// can't leave half-changed.
pub fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// Locks m. If a caught panic poisoned it, reset puts its state back to a known start first, and
/// the lock is clean again.
pub fn lock_or_reset<T>(m: &Mutex<T>, reset: impl FnOnce(&mut T)) -> MutexGuard<'_, T> {
    match m.lock() {
        Ok(g) => g,
        Err(e) => {
            let mut g = e.into_inner();
            reset(&mut g);
            m.clear_poison();
            let _ = writeln!(std::io::stderr(), "a lock was poisoned by a panic; its state started over");
            g
        }
    }
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

    /// A caught panic fails this thread's session until the next one starts.
    #[test]
    fn a_panic_fails_the_session() {
        new_session();
        assert!(!session_failed());
        let _ = guard("ends", (), || panic!("once"));
        let _ = guard("ends", (), || panic!("twice, logged once"));
        assert!(session_failed());
        new_session();
        assert!(!session_failed());
    }

    #[test]
    fn a_panic_aside_is_said_once_and_fails_nothing() {
        new_session();
        aside("logs", || panic!("a logging bug"));
        aside("logs", || panic!("again"));
        assert!(!session_failed(), "the session carries on");
        assert_eq!(SAID.with_borrow(|s| s.iter().filter(|n| **n == "logs").count()), 1, "said once");
        let _ = guard("ends", (), || panic!("a real one"));
        aside("logs", || ());
        assert!(session_failed(), "and it doesn't undo a real failure either");
        new_session();
    }

    #[test]
    fn a_poisoned_shared_lock_starts_over() {
        let m = Mutex::new(vec![1, 2, 3]);
        let _ = guard("poisons", (), || {
            let mut g = m.lock().unwrap();
            g.push(4);
            panic!("halfway");
        });
        assert!(m.is_poisoned());
        assert!(lock_or_reset(&m, |v| v.clear()).is_empty(), "reset");
        assert!(!m.is_poisoned(), "and clean again");
        lock_or_reset(&m, |_| unreachable!()).push(9);
        assert_eq!(*lock(&m), [9]);
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
