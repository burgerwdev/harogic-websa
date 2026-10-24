//! Offline, bounded two-worker PFB feasibility test. No device or application wiring.
use super::*;
use std::collections::BTreeMap;
use std::sync::{mpsc::sync_channel, Arc, Mutex};
use std::thread;
use std::time::Instant;

const FS: f64 = 62_500_000.0;
const PACKET: usize = 16_240;
const BLOCKS: usize = 120;

fn fixture() -> Vec<i16> {
    let mut seed = 20261019u32;
    (0..BLOCKS * PACKET * 2)
        .map(|_| {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            (seed >> 8) as i16
        })
        .collect()
}

#[test]
fn two_workers_preserve_continuous_phase_and_measure_total_cost() {
    let iq = fixture();
    let bin_count: usize = std::env::var("WEBSA_PFB_BINS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(4);
    assert!(bin_count == 4 || bin_count == 32); // offline cases; no product channel limit
    let bins: Vec<u32> = if bin_count == 4 {
        vec![0, 5, 31, 127]
    } else {
        (0..32).collect()
    };
    let mut reference = Pfb::new(FS, 2).unwrap();
    if !reference.avx2 {
        return;
    }
    reference.fir_kernel = 1;
    let mut expected = BTreeMap::new();
    for index in 0..BLOCKS + 8 {
        let block = index % BLOCKS;
        let input = &iq[block * PACKET * 2..(block + 1) * PACKET * 2];
        let mut out = vec![0f32; 2 * bins.len() * (PACKET / reference.hop + 2)];
        let count = reference.process(input, &bins, &mut out);
        if index < 8 || index >= BLOCKS - 4 {
            out.truncate(2 * count * bins.len());
            expected.insert(index, out);
        }
    }

    let repeats = std::env::var("WEBSA_PFB_REPEATS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(if cfg!(debug_assertions) { 1 } else { 12 });
    assert!((1..=200).contains(&repeats));
    let in_flight_limit: usize = std::env::var("WEBSA_PFB_INFLIGHT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(2);
    assert!((2..=3).contains(&in_flight_limit)); // offline scheduling comparison, not a product limit
    let total = BLOCKS * repeats;
    let mut feeder = Pfb::new(FS, 2).unwrap();
    feeder.fir_kernel = 1;
    let (task_tx, task_rx) = sync_channel::<(usize, Pfb)>(2);
    let task_rx = Arc::new(Mutex::new(task_rx));
    let (result_tx, result_rx) = sync_channel::<(usize, usize, Vec<f32>, f64)>(2);
    let mut pending = BTreeMap::new();
    let mut next = 0;
    let mut received = 0;
    let mut delivered_groups = 0usize;
    let mut wait_s = 0.0;
    let mut clone_s = 0.0;
    let mut advance_s = 0.0;
    let mut send_s = 0.0;
    let mut worker_ms = Vec::new();
    let mut checksum = 0.0f64;
    let mut max_pending = 0;
    let elapsed = thread::scope(|scope| {
        for _ in 0..2 {
            let receiver = task_rx.clone();
            let sender = result_tx.clone();
            let source = &iq;
            let bins = &bins;
            scope.spawn(move || loop {
                let task = receiver.lock().unwrap().recv();
                let Ok((index, mut pfb)) = task else { break };
                let block = index % BLOCKS;
                let input = &source[block * PACKET * 2..(block + 1) * PACKET * 2];
                let started = Instant::now();
                let mut output = vec![0f32; 2 * bins.len() * (PACKET / pfb.hop + 2)];
                let groups = pfb.process(input, &bins, &mut output);
                let work_ms = started.elapsed().as_secs_f64() * 1000.0;
                output.truncate(2 * groups * bins.len());
                sender.send((index, groups, output, work_ms)).unwrap();
            });
        }
        drop(result_tx);
        let started = Instant::now();
        let mut in_flight = 0;
        let mut accept = |(index, groups, output, ms): (usize, usize, Vec<f32>, f64)| {
            worker_ms.push(ms);
            pending.insert(index, (groups, output));
            max_pending = max_pending.max(pending.len());
            while let Some((count, data)) = pending.remove(&next) {
                if let Some(reference) = expected.get(&next) {
                    assert_eq!(&data, reference, "packet={next}");
                }
                assert!(count > 0);
                delivered_groups += count;
                checksum += data[0] as f64;
                next += 1;
            }
            next
        };
        let mut delivered_up_to = 0usize;
        for index in 0..total {
            // Bound undelivered order, not just unread worker results: seq 1 can finish before 0.
            while index - delivered_up_to >= in_flight_limit {
                let waiting = Instant::now();
                delivered_up_to = accept(result_rx.recv().unwrap());
                wait_s += waiting.elapsed().as_secs_f64();
                received += 1;
                in_flight -= 1;
            }
            let input = &iq[(index % BLOCKS) * PACKET * 2..(index % BLOCKS + 1) * PACKET * 2];
            let snapshot_at = Instant::now();
            let snapshot = feeder.clone();
            clone_s += snapshot_at.elapsed().as_secs_f64();
            let advance_at = Instant::now();
            feeder.process(input, &[], &mut []);
            advance_s += advance_at.elapsed().as_secs_f64();
            let send_at = Instant::now();
            task_tx.send((index, snapshot)).unwrap();
            send_s += send_at.elapsed().as_secs_f64();
            in_flight += 1;
            if in_flight == in_flight_limit {
                let waiting = Instant::now();
                delivered_up_to = accept(result_rx.recv().unwrap());
                wait_s += waiting.elapsed().as_secs_f64();
                received += 1;
                in_flight -= 1;
            }
        }
        drop(task_tx);
        while received < total {
            accept(result_rx.recv().unwrap());
            received += 1;
        }
        started.elapsed().as_secs_f64()
    });
    assert_eq!(received, total);
    assert_eq!(next, total);
    assert!(pending.is_empty());
    assert_eq!(delivered_groups as u64, feeder.output_groups);
    assert!(max_pending <= in_flight_limit);
    assert!(checksum.is_finite());
    worker_ms.sort_by(f64::total_cmp);
    let p99 = worker_ms[((worker_ms.len() - 1) as f64 * 0.99).round() as usize];
    let msps = (total * PACKET) as f64 / elapsed / 1e6;
    println!("two-worker continuous PFB (bins={bin_count}, in_flight={in_flight_limit}, queues=2+2): {msps:.2} MSps; worker p50={:.3} p99={p99:.3} ms; clone={:.3} advance={:.3} send={:.3} ms/packet; producer result wait={wait_s:.3}s; pending_max={max_pending}; packets={total}",
             worker_ms[worker_ms.len() / 2], 1000.0*clone_s/total as f64,
             1000.0*advance_s/total as f64, 1000.0*send_s/total as f64);
}
