//! The signals that must reach the cleanup handler.
//!
//! Its own test binary on purpose: `ctrlc::set_handler` may be called once per process, and a
//! regression here kills the process outright rather than failing an assertion — which is the
//! whole point. SIGHUP with no handler installed terminates by default, so if the `termination`
//! feature is ever dropped from the ctrlc dependency this test dies instead of passing, and it
//! takes nothing else with it.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

static FIRED: AtomicUsize = AtomicUsize::new(0);

/// Waits for the handler thread, which runs independently of the one that raised the signal.
fn wait_for_handler(count: usize) -> bool {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if FIRED.load(Ordering::SeqCst) >= count {
            return true;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    false
}

#[test]
fn closing_the_terminal_reaches_the_cleanup_handler() {
    ctrlc::set_handler(|| {
        FIRED.fetch_add(1, Ordering::SeqCst);
    })
    .expect("handler must install");

    // What closing a terminal sends. Without it handled, a sandbox outlives the CLI that
    // started it: no `compose down`, no session close, a container left running.
    unsafe { libc::raise(libc::SIGHUP) };
    assert!(wait_for_handler(1), "SIGHUP must reach the handler, not kill the process");

    // What a system shutdown and `kill` send.
    unsafe { libc::raise(libc::SIGTERM) };
    assert!(wait_for_handler(2), "SIGTERM must reach the handler too");

    // And the one that always worked.
    unsafe { libc::raise(libc::SIGINT) };
    assert!(wait_for_handler(3), "SIGINT must still reach the handler");
}
