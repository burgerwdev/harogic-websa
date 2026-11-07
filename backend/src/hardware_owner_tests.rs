//! Controllable owner stalls: release every gate before asserting so regressions
//! fail without leaving a test worker or a vendor call running.
use super::*;
use std::{sync::mpsc::Receiver, time::Instant};

pub(crate) fn isolated(name: &str) -> bool {
    const CHILD: &str = "WEBSA_TEST_HARDWARE_OWNER_ISOLATED";
    if std::env::var(CHILD).as_deref() == Ok(name) {
        return true;
    }
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([name, "--exact", "--test-threads=1"])
        .env(CHILD, name)
        .status()
        .unwrap();
    assert!(status.success(), "isolated owner contract failed: {name}");
    false
}

pub(crate) fn full_mailbox<C>(
    queue: Receiver<C>,
    request: impl FnOnce() -> (bool, bool) + Send + 'static,
) {
    let (done, completed) = mpsc::sync_channel(1);
    let caller = std::thread::spawn(move || {
        done.send(request()).unwrap();
    });
    let result = completed.recv_timeout(Duration::from_secs(1));
    // A blocking SyncSender::send can now finish, including on the failing code.
    drop(queue);
    caller.join().unwrap();
    let (timed_out, stopped) = result.expect("request deadline did not include the full mailbox");
    assert!(
        timed_out,
        "a full mailbox must reject the request at its deadline"
    );
    assert!(
        stopped,
        "timed-out ownership must not accept later SDK commands"
    );
}

pub(crate) fn blocked_owner<T: Send + 'static>(
    make: impl FnOnce(JoinHandle<()>, Arc<AtomicBool>) -> T,
) {
    assert!(owner_slot_free().is_ok());
    let stop = Arc::new(AtomicBool::new(false));
    let retired = Arc::new(AtomicBool::new(false));
    let retired_on_owner = retired.clone();
    let (release, blocked) = mpsc::sync_channel(1);
    let owner = std::thread::spawn(move || {
        blocked.recv().unwrap();
        retired_on_owner.store(true, Ordering::Release);
    });
    let hardware = make(owner, stop.clone());
    let (done, completed) = mpsc::sync_channel(1);
    let caller = std::thread::spawn(move || {
        drop(hardware);
        done.send(()).unwrap();
    });
    let result = completed.recv_timeout(Duration::from_secs(1));
    let replacement_excluded = owner_slot_free().is_err();
    let shutdown_excluded = finish_sdk_session().is_err();
    let handoff_excluded = release_sdk_session().is_err();
    let retired_while_blocked = retired.load(Ordering::Acquire);
    release.send(()).unwrap();
    caller.join().unwrap();
    wait_for_owner();
    assert!(result.is_ok(), "teardown waited for the blocked SDK owner");
    assert!(stop.load(Ordering::Acquire));
    assert!(
        replacement_excluded && shutdown_excluded && handoff_excluded,
        "parked ownership must exclude replacement, shutdown and physical handoff"
    );
    assert!(!retired_while_blocked && retired.load(Ordering::Acquire));
}

pub(crate) fn wait_for_owner() {
    let deadline = Instant::now() + Duration::from_secs(2);
    while owner_slot_free().is_err() {
        assert!(Instant::now() < deadline, "released owner did not retire");
        std::thread::sleep(Duration::from_millis(1));
    }
}
