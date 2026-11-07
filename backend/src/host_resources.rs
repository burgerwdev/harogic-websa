//! Process-visible resources, including inherited fractional CPU quotas.
//! These observations schedule auxiliary preparation; they never cap receivers.
use std::collections::BTreeSet;
use std::path::{Path,PathBuf};
use std::sync::{Mutex,LazyLock};
use std::time::Instant;
use serde_json::{json,Value};

fn cpus(text:&str)->Option<BTreeSet<usize>> {
    let mut set=BTreeSet::new();
    for range in text.trim().split(',').filter(|part|!part.is_empty()) {
        let mut parts=range.split('-');let first=parts.next()?.parse::<usize>().ok()?;
        let last=parts.next().map(|part|part.parse().ok()).unwrap_or(Some(first))?;
        if parts.next().is_some() || last<first || last>1_048_576 {return None;}
        set.extend(first..=last);
    }
    Some(set)
}
fn affinity()->Option<BTreeSet<usize>> {
    #[cfg(target_os="linux")] {
        unsafe extern "C" {fn sched_getaffinity(pid:i32,size:usize,mask:*mut u8)->i32;}
        let mut mask=[0_u8;8192];
        if unsafe {sched_getaffinity(0,mask.len(),mask.as_mut_ptr())}!=0 {return None;}
        return Some(mask.iter().enumerate().flat_map(|(byte,value)|(0..8).filter_map(move|bit|
            (value&(1<<bit)!=0).then_some(byte*8+bit))).collect());
    }
    #[cfg(not(target_os="linux"))] {None}
}
fn number(path:impl AsRef<Path>)->Option<u64> {std::fs::read_to_string(path).ok()?.trim().parse().ok()}
fn quota(text:&str)->Option<f64> {
    let mut parts=text.split_whitespace();let amount=parts.next()?;
    let period=parts.next()?.parse::<f64>().ok()?;
    if amount=="max" || period<=0.0 {return None;}
    let quota=amount.parse::<f64>().ok()?/period;(quota>0.0&&quota.is_finite()).then_some(quota)
}
fn cgroup_location()->Option<(PathBuf,PathBuf)> {
    let group=std::fs::read_to_string("/proc/self/cgroup").ok()?.lines()
        .find_map(|line|line.strip_prefix("0::").map(str::to_owned))?;
    let mounts=std::fs::read_to_string("/proc/self/mountinfo").ok()?;
    cgroup_location_from(&group,&mounts)
}
fn cgroup_location_from(group:&str,mounts:&str)->Option<(PathBuf,PathBuf)> {
    // Decode backslashes last so a resulting escape-like path is not decoded twice.
    let decode=|field:&str|PathBuf::from(field.replace("\\040"," ").replace("\\011","\t")
        .replace("\\012","\n").replace("\\134","\\"));
    for line in mounts.lines() {
        let (before,after)=line.split_once(" - ")?;
        if after.split_whitespace().next()!=Some("cgroup2") {continue;}
        let fields:Vec<_>=before.split_whitespace().collect();
        let root=decode(fields.get(3)?);
        let mount=decode(fields.get(4)?);
        let Ok(relative)=Path::new(group).strip_prefix(&root) else {continue;};
        return Some((mount.join(relative),mount));
    }
    None
}
fn hierarchy(leaf:&Path,mount:&Path)->Vec<PathBuf> {
    let mut result=Vec::new();let mut current=leaf;
    for _ in 0..128 {
        if !current.starts_with(mount) {break;}
        result.push(current.to_owned());if current==mount {break;}
        let Some(parent)=current.parent() else {break;};current=parent;
    }
    result
}
fn cpu_seconds()->Option<f64> {
    let stat=std::fs::read_to_string("/proc/self/stat").ok()?;
    let fields:Vec<_>=stat.rsplit_once(") ")?.1.split_whitespace().collect();
    unsafe extern "C" {fn sysconf(name:i32)->i64;}
    let ticks=unsafe {sysconf(2)};if ticks<=0 {return None;}
    Some((fields.get(11)?.parse::<u64>().ok()?+fields.get(12)?.parse::<u64>().ok()?) as f64/ticks as f64)
}
fn kibibytes(text:&str,name:&str)->Option<u64> {
    text.lines().find_map(|line|line.strip_prefix(name))?.split_whitespace().next()?.parse::<u64>().ok().map(|value|value*1024)
}
fn observe()->Value {
    let mut errors=Vec::new();
    let affinity=affinity().unwrap_or_else(|| {errors.push("affinity unavailable; using available_parallelism");
        (0..std::thread::available_parallelism().map_or(1,usize::from)).collect()});
    let location=cgroup_location();
    let paths=location.as_ref().map(|(leaf,mount)|hierarchy(leaf,mount)).unwrap_or_default();
    if paths.is_empty() {errors.push("cgroup v2 hierarchy unavailable; quota unknown");}
    let cpuset=paths.iter().find_map(|path|std::fs::read_to_string(path.join("cpuset.cpus.effective")).ok().and_then(|text|cpus(&text)).filter(|set|!set.is_empty()));
    let allowed:BTreeSet<_>=cpuset.as_ref().map_or_else(||affinity.clone(),|cpuset|affinity.intersection(cpuset).copied().collect());
    let quotas:Vec<_>=paths.iter().filter_map(|path|std::fs::read_to_string(path.join("cpu.max")).ok()
        .and_then(|text|quota(&text)).map(|value|json!({"cgroup":path.display().to_string(),"cores":value}))).collect();
    let quota=quotas.iter().filter_map(|row|row["cores"].as_f64()).reduce(f64::min);
    let effective=quota.unwrap_or(allowed.len() as f64).min(allowed.len() as f64).max(0.001);
    let status=std::fs::read_to_string("/proc/self/status").unwrap_or_default();
    let memory=std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
    let headroom=paths.iter().filter_map(|path|Some(number(path.join("memory.max"))?.saturating_sub(number(path.join("memory.current"))?))).min();
    let memory_available=kibibytes(&memory,"MemAvailable:").into_iter().chain(headroom).min();
    let host_cpu=std::fs::read_to_string("/proc/stat").ok().and_then(|text| {
        let fields:Vec<u64>=text.lines().next()?.split_whitespace().skip(1).take(8).map(str::parse).collect::<Result<_,_>>().ok()?;
        Some(json!({"total_ticks":fields.iter().sum::<u64>(),"idle_ticks":fields.get(3)?+fields.get(4)?}))
    });
    json!({"affinity_cpus":affinity,"effective_cpuset":cpuset,"allowed_cpus":allowed,
        "hierarchical_cpu_quotas":quotas,"quota_cores":quota,"effective_cpu_cores":effective,
        "cgroup":location.map(|(leaf,_)|leaf.display().to_string()),"degraded_reasons":errors,
        "rss_bytes":kibibytes(&status,"VmRSS:"),"peak_rss_bytes":kibibytes(&status,"VmHWM:"),
        "swap_bytes":kibibytes(&status,"VmSwap:"),"available_memory_bytes":memory_available,
        "process_cpu_seconds":cpu_seconds(),"host_cpu_ticks":host_cpu,
        "host_loadavg":std::fs::read_to_string("/proc/loadavg").ok().map(|text|text.split_whitespace().take(3).map(str::to_owned).collect::<Vec<_>>()),
        "host_load_precision":"host aggregate; cpuset-specific competitor CPU time is not directly observable",
        "receiver_limit_from_cpu":null})
}
static SNAPSHOT:LazyLock<Mutex<Option<(Instant,Value)>>>=LazyLock::new(||Mutex::new(None));
pub fn snapshot()->Value {
    snapshot_with_max_age(std::time::Duration::from_secs(1))
}
/// Preparation may have waited behind other model loads. Its start decision
/// and RSS baseline must observe the host again, even inside the reporting TTL.
pub(crate) fn fresh_snapshot()->Value {
    snapshot_with_max_age(std::time::Duration::ZERO)
}
fn snapshot_with_max_age(max_age:std::time::Duration)->Value {
    let mut previous=SNAPSHOT.lock().unwrap_or_else(|e|e.into_inner());
    if let Some((at,value))=&*previous {if at.elapsed()<max_age {return value.clone();}}
    let mut current=observe();
    let used=previous.as_ref().and_then(|(at,old)|Some((current["process_cpu_seconds"].as_f64()?-
        old["process_cpu_seconds"].as_f64()?)/at.elapsed().as_secs_f64()));
    current["measured_process_cpu_cores"]=json!(used);
    let host_busy=previous.as_ref().and_then(|(_,old)| {
        let total=current["host_cpu_ticks"]["total_ticks"].as_u64()?.checked_sub(old["host_cpu_ticks"]["total_ticks"].as_u64()?)?;
        let idle=current["host_cpu_ticks"]["idle_ticks"].as_u64()?.checked_sub(old["host_cpu_ticks"]["idle_ticks"].as_u64()?)?;
        (total>0).then(||(total.saturating_sub(idle)) as f64/total as f64)
    });
    current["host_cpu_busy_fraction"]=json!(host_busy);
    let capacity=current["effective_cpu_cores"].as_f64().unwrap_or(1.0);
    let spare=(capacity-used.unwrap_or(0.0)).min(capacity*(1.0-host_busy.unwrap_or(0.0))).max(1.0);
    current["preparation_parallelism"]=json!(spare.floor().min(capacity.ceil()).max(1.0) as usize);
    *previous=Some((Instant::now(),current.clone()));current
}
pub fn background_current_thread() {
    #[cfg(target_os="linux")] {
        unsafe extern "C" {fn setpriority(which:i32,who:u32,priority:i32)->i32;}
        // Linux who=0 affects the calling thread. No elevated scheduling rights.
        let _=unsafe {setpriority(0,0,5)};
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fractional_and_inherited_quotas_do_not_round_up_cpu_time() {
        assert_eq!(quota("50000 100000"),Some(0.5));assert_eq!(quota("150000 100000"),Some(1.5));
        assert_eq!(quota("max 100000"),None);assert_eq!(quota("100 0"),None);
        assert_eq!(cpus("0-3,7,9-10").unwrap().len(),7);
        assert_eq!(hierarchy(Path::new("/cg/a/b"),Path::new("/cg")),vec![PathBuf::from("/cg/a/b"),PathBuf::from("/cg/a"),PathBuf::from("/cg")]);
    }
    #[test]
    fn cgroup_location_skips_unrelated_subtree_mounts() {
        let mounts="39 25 0:33 /tenant-b /isolated rw - cgroup2 cgroup rw\n\
                    40 25 0:33 / /sys/fs/cgroup rw - cgroup2 cgroup rw\n";
        assert_eq!(cgroup_location_from("/tenant-a/job",mounts),
            Some((PathBuf::from("/sys/fs/cgroup/tenant-a/job"),PathBuf::from("/sys/fs/cgroup"))));
    }
    #[test]
    fn cgroup_location_decodes_mountinfo_path_escapes_once() {
        // mountinfo escapes space, tab, newline and backslash in path fields.
        // A literal backslash followed by 040 must not become a space.
        for (encoded,decoded) in [("\\040"," "),("\\011","\t"),("\\012","\n"),("\\134","\\"),("\\134040","\\040")] {
            let mounts=format!("39 25 0:33 / /sys/cgroup{encoded}mount rw - cgroup2 cgroup rw\n");
            let mount=PathBuf::from(format!("/sys/cgroup{decoded}mount"));
            assert_eq!(cgroup_location_from("/work",&mounts),
                Some((mount.join("work"),mount)),"mountinfo escape {encoded}");
        }
    }
    #[test]
    fn reports_the_current_process_without_creating_a_receiver_cap() {
        let value=snapshot();println!("host_resources={value}");
        assert!(value["effective_cpu_cores"].as_f64().unwrap()>0.0);assert!(value["receiver_limit_from_cpu"].is_null());
        assert!(value["rss_bytes"].as_u64().unwrap()>0);
    }
}
