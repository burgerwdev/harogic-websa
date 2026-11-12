use std::time::{Duration, Instant};

// Preserve the previous maximum sustained duty cycle (compute + 10x cooldown),
// with 20 ms of CPU credit for a short batch. CPU debt is never discarded on
// cancel, clear or a new selection. Reception-pressure admission still wins.
const DUTY:f64=1.0/11.0;
const BURST:f64=0.020;
pub(super) struct FeatureBudget {credit:f64,at:Instant}
impl Default for FeatureBudget {
    fn default()->Self {Self{credit:BURST,at:Instant::now()}}
}
impl FeatureBudget {
    fn balance(&self,now:Instant)->f64 {(self.credit+now.saturating_duration_since(self.at).as_secs_f64()*DUTY).min(BURST)}
    pub(super) fn wait(&self,now:Instant)->Duration {Duration::from_secs_f64((-self.balance(now)/DUTY).max(0.0))}
    pub(super) fn charge(&mut self,seconds:f64,now:Instant){self.credit=self.balance(now)-seconds;self.at=now;}
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn short_batch_uses_burst_but_excess_cpu_must_be_repaid(){
        let mut budget=FeatureBudget::default();let at=budget.at;
        budget.charge(0.008,at);budget.charge(0.008,at);
        assert_eq!(budget.wait(at),Duration::ZERO);
        budget.charge(0.014,at);
        assert!((budget.wait(at).as_secs_f64()-0.11).abs()<1e-8);
        assert_eq!(budget.wait(at+Duration::from_millis(111)),Duration::ZERO);
        budget.charge(0.05,at+Duration::from_secs(100));
        assert!((budget.wait(at+Duration::from_secs(100)).as_secs_f64()-0.33).abs()<1e-8);
    }
}
