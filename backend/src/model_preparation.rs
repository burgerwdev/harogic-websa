//! Fair, bounded cold preparation. Receiver execution stays per owner thread.
use serde_json::{json, Value};
use std::collections::VecDeque;
use std::sync::{Condvar, LazyLock, Mutex};
use std::time::{Duration, Instant};
#[derive(Default)]
struct State {
    next: u64,
    queue: VecDeque<u64>,
    active: usize,
    completed: u64,
    failed: u64,
    cancelled: u64,
    wall_ns: u64,
    max_ns: u64,
    observed_rss_growth: u64,
}
type Scheduler = (Mutex<State>, Condvar);
static STATE: LazyLock<Scheduler> =
    LazyLock::new(|| (Mutex::new(State::default()), Condvar::new()));
struct WaitingTicket<'a> {
    scheduler: &'a Scheduler,
    ticket: Option<u64>,
}
impl Drop for WaitingTicket<'_> {
    fn drop(&mut self) {
        let Some(ticket) = self.ticket.take() else { return; };
        let (mutex, wake) = self.scheduler;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = state.queue.iter().position(|&queued| queued == ticket) {
            state.queue.remove(index);
            state.cancelled += 1;
            wake.notify_all();
        }
    }
}
pub(crate) struct Preparation<'a> {
    scheduler: &'a Scheduler,
    began: Instant,
    rss: Option<u64>,
    done: bool,
}
impl Preparation<'static> {
    pub fn acquire() -> Result<Self, String> {
        Self::acquire_with(
            &STATE,
            crate::host_resources::fresh_snapshot,
            Duration::from_secs(30),
        )
    }
}
impl<'a> Preparation<'a> {
    fn acquire_with(
        scheduler: &'a Scheduler,
        mut observe: impl FnMut() -> Value,
        timeout: Duration,
    ) -> Result<Self, String> {
        let (mutex, wake) = scheduler;
        // Declare the ticket before the mutex guard so unwinding always releases
        // the lock before the ticket removes itself from the waiting queue.
        let mut waiting_ticket = WaitingTicket { scheduler, ticket: None };
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        if state.queue.len() >= 64 {
            return Err(
                "model preparation mailbox full; retry after queued preparations finish".into(),
            );
        }
        let ticket = state.next;
        state.next += 1;
        state.queue.push_back(ticket);
        waiting_ticket.ticket = Some(ticket);
        let waiting = Instant::now();
        loop {
            if waiting.elapsed() > timeout {
                drop(state);
                return Err(
                    "model preparation queue exceeded 30 seconds; no replacement call was started"
                        .into(),
                );
            }
            if state.queue.front() == Some(&ticket) {
                // Do not hold the scheduler while reading /proc and cgroups;
                // active preparations must be able to finish and release it.
                drop(state);
                let host = observe();
                let resources = host["preparation_parallelism"].as_u64().unwrap_or(1) as usize;
                let available = host["available_memory_bytes"].as_u64().unwrap_or(0);
                state = mutex.lock().unwrap_or_else(|e| e.into_inner());
                // Unknown first-use memory is measured with one preparation.
                // RSS growth is a concurrency estimate, not a model-size or
                // receiver admission limit. Even low headroom permits one.
                let memory = if state.observed_rss_growth == 0 || available == 0 {
                    1
                } else {
                    (available / state.observed_rss_growth).max(1) as usize
                };
                let concurrency = resources.min(memory).max(1);
                if waiting.elapsed() <= timeout
                    && state.queue.front() == Some(&ticket)
                    && state.active < concurrency
                {
                    state.queue.pop_front();
                    state.active += 1;
                    waiting_ticket.ticket = None;
                    wake.notify_all();
                    return Ok(Self {
                        scheduler,
                        began: Instant::now(),
                        rss: host["rss_bytes"].as_u64(),
                        done: false,
                    });
                }
            }
            state = wake
                .wait_timeout(state, Duration::from_millis(50))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
    }
    pub fn finish(&mut self, success: bool) {
        if self.done {
            return;
        }
        let rss = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|text| {
                text.lines().find_map(|line| {
                    line.strip_prefix("VmRSS:")
                        .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
                })
            })
            .and_then(|value| value.checked_mul(1024));
        self.finish_with_rss(success, rss);
    }
    fn finish_with_rss(&mut self, success: bool, rss: Option<u64>) {
        if self.done {
            return;
        }
        self.done = true;
        let elapsed = self.began.elapsed().as_nanos().min(u64::MAX as u128) as u64;
        let (mutex, wake) = self.scheduler;
        let mut state = mutex.lock().unwrap_or_else(|e| e.into_inner());
        state.active -= 1;
        if success {
            state.completed += 1;
        } else {
            state.failed += 1;
        }
        state.wall_ns += elapsed;
        state.max_ns = state.max_ns.max(elapsed);
        if let (Some(before), Some(after)) = (self.rss, rss) {
            state.observed_rss_growth = state.observed_rss_growth.max(after.saturating_sub(before));
        }
        wake.notify_all();
    }
}
impl Drop for Preparation<'_> {
    fn drop(&mut self) {
        self.finish(false);
    }
}
pub fn snapshot() -> Value {
    let state = STATE.0.lock().unwrap_or_else(|e| e.into_inner());
    json!({"queued":state.queue.len(),"active":state.active,"succeeded":state.completed,"failed":state.failed,
        "cancelled":state.cancelled,"wall_ns":state.wall_ns,"max_ns":state.max_ns,
        "observed_process_rss_growth_bytes":state.observed_rss_growth,"rss_precision":"process_growth_during_preparation; not exclusive model weights",
        "max_queued":64,"max_queue_seconds":30})
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn queued_preparation_refreshes_resources_and_rss_before_starting() {
        let scheduler = (
            Mutex::new(State {
                active: 4,
                observed_rss_growth: 100,
                ..State::default()
            }),
            Condvar::new(),
        );
        let mut observations = 0;
        let permit = Preparation::acquire_with(
            &scheduler,
            || {
                observations += 1;
                // Other preparations finish, then available memory shrinks before
                // this ticket can start. Only the third observation permits it.
                if observations == 2 {
                    scheduler.0.lock().unwrap().active = 1;
                }
                let available = if observations == 2 { 100 } else { 400 };
                json!({"preparation_parallelism":4,"available_memory_bytes":available,
                "rss_bytes":observations*1000})
            },
            Duration::from_millis(500),
        )
        .expect("headroom is available on the third observation");
        assert_eq!(observations, 3);
        assert_eq!(
            permit.rss,
            Some(3000),
            "RSS growth starts at grant, not enqueue"
        );
        assert_eq!(scheduler.0.lock().unwrap().active, 2);
        drop(permit);
        let state = scheduler.0.lock().unwrap();
        assert_eq!(state.active, 1);
        assert!(state.queue.is_empty());
        assert_eq!(state.failed, 1);
    }

    #[test]
    fn timed_out_ticket_is_removed_and_next_preparation_can_start() {
        let scheduler = (
            Mutex::new(State {
                active: 1,
                ..State::default()
            }),
            Condvar::new(),
        );
        let resources =
            || json!({"preparation_parallelism":1,"available_memory_bytes":0,"rss_bytes":0});
        assert!(Preparation::acquire_with(&scheduler, resources, Duration::ZERO).is_err());
        {
            let mut state = scheduler.0.lock().unwrap();
            assert!(state.queue.is_empty());
            assert_eq!(state.cancelled, 1);
            state.active = 0;
        }
        let mut permit =
            Preparation::acquire_with(&scheduler, resources, Duration::from_secs(1)).unwrap();
        permit.finish(true);
        permit.finish(false);
        drop(permit);
        let state = scheduler.0.lock().unwrap();
        assert_eq!(state.active, 0);
        assert_eq!(state.completed, 1);
        assert_eq!(state.failed, 0);
    }

    #[test]
    fn panicking_resource_observation_releases_queued_ticket_and_can_retry() {
        let scheduler = (Mutex::new(State::default()), Condvar::new());
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _permit = Preparation::acquire_with(
                &scheduler,
                || panic!("injected resource observation failure"),
                Duration::from_secs(1),
            );
        }));
        assert!(result.is_err(), "the caller still observes the original panic");
        {
            let state = scheduler.0.lock().unwrap();
            assert!(state.queue.is_empty(), "an unwound owner cannot retain a ticket");
            assert_eq!(state.active, 0);
            assert_eq!(state.cancelled, 1);
        }
        let mut retry = Preparation::acquire_with(
            &scheduler,
            || json!({"preparation_parallelism":1,"available_memory_bytes":0}),
            Duration::from_secs(1),
        )
        .expect("a later owner must be able to prepare after the failed observation");
        retry.finish_with_rss(true, None);
        let state = scheduler.0.lock().unwrap();
        assert!(state.queue.is_empty());
        assert_eq!(state.active, 0);
        assert_eq!(state.completed, 1);
    }

    #[test]
    fn unavailable_rss_is_not_measured_as_zero_bytes() {
        for (before, after, expected) in [
            (None, Some(5000), 0),
            (Some(5000), None, 0),
            (Some(5000), Some(4000), 0),
            (Some(5000), Some(6000), 1000),
        ] {
            let scheduler = (Mutex::new(State::default()), Condvar::new());
            let mut permit = Preparation::acquire_with(
                &scheduler,
                || json!({"preparation_parallelism":1,"rss_bytes":before}),
                Duration::from_secs(1),
            )
            .unwrap();
            permit.finish_with_rss(true, after);
            assert_eq!(scheduler.0.lock().unwrap().observed_rss_growth, expected);
        }
    }
}
