//! Fixed-memory nanosecond timings. Quantiles use nearest rank and return the
//! inclusive histogram bucket upper bound: 0, 1, 3, 7, …, u64::MAX ns.
//! No observations are sampled or discarded; counters and totals saturate at
//! u64::MAX, and durations above u64::MAX ns are clamped to that value.
use std::time::Duration;

#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub struct TimingSnapshot {
    pub count: u64,
    pub total_ns: u64,
    pub max_ns: u64,
    pub p50_ns: u64,
    pub p99_ns: u64,
}

#[derive(Clone, Debug)]
pub struct Timing {
    bins: [u64; 65],
    count: u64,
    total_ns: u64,
    max_ns: u64,
}

impl Default for Timing {
    fn default() -> Self {
        Self { bins: [0; 65], count: 0, total_ns: 0, max_ns: 0 }
    }
}

impl Timing {
    pub fn observe(&mut self, elapsed: Duration) {
        let ns = elapsed.as_nanos().min(u64::MAX as u128) as u64;
        let bucket = (u64::BITS - ns.leading_zeros()) as usize;
        self.bins[bucket] = self.bins[bucket].saturating_add(1);
        self.count = self.count.saturating_add(1);
        self.total_ns = self.total_ns.saturating_add(ns);
        self.max_ns = self.max_ns.max(ns);
    }

    /// Merge fixed histograms without reconstructing samples or allocating.
    pub fn merge(&mut self, other:&Self) {
        for (a,b) in self.bins.iter_mut().zip(other.bins) {*a=a.saturating_add(b);}
        self.count=self.count.saturating_add(other.count);
        self.total_ns=self.total_ns.saturating_add(other.total_ns);
        self.max_ns=self.max_ns.max(other.max_ns);
    }

    pub fn snapshot(&self) -> TimingSnapshot {
        let quantile = |percent: u64| {
            if self.count == 0 { return 0; }
            let rank = (self.count / 100) * percent
                + ((self.count % 100) * percent).div_ceil(100);
            let mut count = 0u64;
            for (bucket, observations) in self.bins.iter().enumerate() {
                count = count.saturating_add(*observations);
                if count >= rank {
                    return if bucket == 64 { u64::MAX } else { (1u64 << bucket) - 1 };
                }
            }
            u64::MAX
        };
        TimingSnapshot {
            count: self.count,
            total_ns: self.total_ns,
            max_ns: self.max_ns,
            p50_ns: quantile(50),
            p99_ns: quantile(99),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quantiles_cover_zero_and_every_bucket_boundary() {
        assert_eq!(Timing::default().snapshot().p99_ns, 0);
        for ns in [0, 1, 2, 3, 4, 7, 8, 1u64 << 63, u64::MAX] {
            let mut timing = Timing::default();
            timing.observe(Duration::from_nanos(ns));
            let snapshot = timing.snapshot();
            let expected = if ns == 0 { 0 } else { u64::MAX >> ns.leading_zeros() };
            assert_eq!(snapshot.p50_ns, expected);
            assert_eq!(snapshot.p99_ns, expected);
            assert_eq!(snapshot.max_ns, ns);
        }
    }

    #[test]
    fn all_observations_contribute_to_totals_and_nearest_rank() {
        let mut timing = Timing::default();
        for ns in 0..100 { timing.observe(Duration::from_nanos(ns)); }
        let snapshot = timing.snapshot();
        assert_eq!(snapshot.count, 100);
        assert_eq!(snapshot.total_ns, 4950);
        assert_eq!(snapshot.max_ns, 99);
        assert_eq!(snapshot.p50_ns, 63);
        assert_eq!(snapshot.p99_ns, 127);
        timing.observe(Duration::from_nanos(u64::MAX));
        let snapshot = timing.snapshot();
        assert_eq!(snapshot.count, 101);
        assert_eq!(snapshot.total_ns, u64::MAX);
        assert_eq!(snapshot.max_ns, u64::MAX);
        assert_eq!(snapshot.p99_ns, 127);
    }
}

/// Execution time on this thread, excluding scheduling and blocking waits.
pub fn thread_cpu_ns() -> Option<u64> {
    #[cfg(target_os = "linux")]
    {
        let mut time = libc::timespec { tv_sec: 0, tv_nsec: 0 };
        (unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut time) } == 0)
            .then(|| (time.tv_sec as u64).saturating_mul(1_000_000_000).saturating_add(time.tv_nsec as u64))
    }
    #[cfg(not(target_os = "linux"))]
    { None }
}

/// Opt-in scheduling attribution. Called only for sampled diagnostic jobs;
/// fixed stack storage keeps this out of the ordinary allocation path.
#[derive(Clone, Copy, Default, Debug, serde::Serialize)]
pub struct ThreadSchedule {
    pub runnable_wait_ns: u64,
    pub voluntary_switches: u64,
    pub involuntary_switches: u64,
    pub minor_faults: u64,
}
pub fn thread_schedule() -> Option<ThreadSchedule> {
    #[cfg(target_os = "linux")]
    {
        use std::io::Read;
        let mut bytes=[0u8;128];
        let count=std::fs::File::open("/proc/thread-self/schedstat").ok()?.read(&mut bytes).ok()?;
        let wait=std::str::from_utf8(&bytes[..count]).ok()?.split_whitespace().nth(1)?.parse().ok()?;
        let mut usage=std::mem::MaybeUninit::<libc::rusage>::uninit();
        if unsafe {libc::getrusage(libc::RUSAGE_THREAD,usage.as_mut_ptr())} != 0 {return None;}
        let usage=unsafe {usage.assume_init()};
        Some(ThreadSchedule {runnable_wait_ns:wait,voluntary_switches:usage.ru_nvcsw as u64,
            involuntary_switches:usage.ru_nivcsw as u64,minor_faults:usage.ru_minflt as u64})
    }
    #[cfg(not(target_os = "linux"))]
    { None }
}
impl ThreadSchedule {
    pub fn since(self, before: Self) -> Self {
        Self {runnable_wait_ns:self.runnable_wait_ns.saturating_sub(before.runnable_wait_ns),
            voluntary_switches:self.voluntary_switches.saturating_sub(before.voluntary_switches),
            involuntary_switches:self.involuntary_switches.saturating_sub(before.involuntary_switches),
            minor_faults:self.minor_faults.saturating_sub(before.minor_faults)}
    }
}
