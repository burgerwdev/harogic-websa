//! Offline throughput and serial-reference contract for ordered coarse-bin IQ output (no SDK).
use rustfft::{num_complex::Complex, FftPlanner};
use std::time::{Duration, Instant};
use websa_channelizer::{
    pfb_close, pfb_geometry, pfb_open_config, pfb_process, FineChannel, FineSpec, Pfb,
    PfbStream, MAX_IN_FLIGHT,
};

const N: usize = 16240;
const FS: f64 = 62_500_000.0;
const USAGE: &str = "usage: pfb_output_bench [seconds] [1..128 bins] [--fine|--fused [decimate]] [--taps 4|8|16]\n       pfb_output_bench --contract\n       pfb_output_bench --help\n--taps defaults to 16; append it after throughput arguments\n--contract checks 31.25/62.5 MS/s with taps 4/8/16, 32 consecutive bins and reordered positive/negative bins against serial kernel0 (absolute error <= 0.001); no throughput timing";

struct SerialReference(*mut Pfb);

impl Drop for SerialReference {
    fn drop(&mut self) {
        unsafe { pfb_close(self.0) };
    }
}

fn contract() -> Result<(), String> {
    let mut seed = 20261019u32;
    let sizes = [1, 17, 31, 32, 33, 63, 64, 65, 4093, N, 7, N - 1, 121, N, 27];
    let packets: Vec<Vec<i16>> = sizes
        .into_iter()
        .cycle()
        .take(sizes.len() * 3)
        .map(|n| {
            (0..2 * n)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 8) as i16
                })
                .collect()
        })
        .collect();
    for (fs, taps) in [31_250_000.0, 62_500_000.0]
        .into_iter()
        .flat_map(|fs| [4usize, 8, 16].into_iter().map(move |taps| (fs, taps)))
    {
        for selection in ["consecutive", "signed-reordered"] {
            let reference = SerialReference(pfb_open_config(fs, 2, taps as u32, 0));
            if reference.0.is_null() {
                return Err(format!("contract fs={fs}: serial kernel0 unavailable"));
            }
            let (mut m, mut hop, mut rate, mut delay) = (0, 0, 0.0, 0.0);
            let status = unsafe {
                pfb_geometry(reference.0, &mut m, &mut hop, &mut rate, &mut delay)
            };
            if status != 0 || hop == 0 || m != 2 * hop || !delay.is_finite() || delay <= 0.0 {
                return Err(format!("contract fs={fs}: invalid reference geometry"));
            }
            let bins: Vec<u32> = if selection == "consecutive" {
                (0..32).collect()
            } else {
                vec![m - 1, 5, 0, m - 7, 1, m / 2 - 1, m / 2 + 1]
            };
            let epoch = 7;
            let mut stream = PfbStream::with_taps(fs, N, bins.clone(), epoch, taps)
                .ok_or_else(|| format!("contract fs={fs}: PFB stream unavailable"))?;
            if stream.bins() != bins.as_slice()
                || stream.bin_count() != m as usize
                || stream.epoch() != epoch
                || stream.sample_rate != rate
                || rate != fs / hop as f64
                || stream.delay_samples != delay
            {
                return Err(format!("contract fs={fs}: stream geometry/clock mismatch"));
            }
            let mut expected = Vec::with_capacity(packets.len());
            let mut input_index = 0u64;
            for iq in &packets {
                let n = iq.len() / 2;
                let mut output = vec![f32::NAN; 2 * bins.len() * (n / hop as usize + 1)];
                let groups = unsafe {
                    pfb_process(
                        reference.0,
                        iq.as_ptr(),
                        n as i32,
                        bins.as_ptr(),
                        bins.len() as i32,
                        output.as_mut_ptr(),
                        (output.len() / 2) as i32,
                    )
                };
                let clock_groups = (input_index + n as u64) / hop as u64
                    - input_index / hop as u64;
                if groups < 0 || groups as u64 != clock_groups {
                    return Err(format!("contract fs={fs}: reference group clock mismatch"));
                }
                output.truncate(groups as usize * bins.len() * 2);
                expected.push((input_index, groups as u32, output));
                input_index += n as u64;
            }
            let mut delivered = 0usize;
            let mut delivered_groups = 0u64;
            let mut components = 0u64;
            let mut max_error = 0.0f32;
            let mut drain = |stream: &mut PfbStream| -> Result<bool, String> {
                let Some(item) = stream.take() else {
                    return Ok(false);
                };
                let Some((index, groups, values)) = expected.get(delivered) else {
                    return Err(format!("contract fs={fs}: unexpected output packet"));
                };
                if (item.seq, item.stamp, item.input_index, item.epoch, item.groups)
                    != (delivered as u64, 1000 + index * 2, *index, epoch, *groups)
                    || item.iq.len() != values.len()
                    || item.iq.len() != item.groups as usize * bins.len() * 2
                    || delivered_groups != index / hop as u64
                {
                    return Err(format!(
                        "contract fs={fs} bins={selection} packet={delivered}: metadata/order/clock mismatch"
                    ));
                }
                for (component, (&actual, &want)) in item.iq.iter().zip(values).enumerate() {
                    let error = (actual - want).abs();
                    if !actual.is_finite() || !want.is_finite() || error > 0.001 {
                        return Err(format!(
                            "contract fs={fs} bins={selection} packet={delivered} component={component}: actual={actual} reference={want} error={error}"
                        ));
                    }
                    max_error = max_error.max(error);
                }
                components += item.iq.len() as u64;
                delivered_groups += item.groups as u64;
                delivered += 1;
                Ok(true)
            };
            for (seq, iq) in packets.iter().enumerate() {
                let stamp = 1000 + expected[seq].0 * 2;
                loop {
                    match stream.submit(seq as u64, stamp, iq) {
                        Ok(()) => break,
                        Err(-2) => {
                            if !drain(&mut stream)? {
                                return Err(format!("contract fs={fs}: backpressure without output"));
                            }
                        }
                        Err(status) => {
                            return Err(format!("contract fs={fs}: submit status={status}"));
                        }
                    }
                }
                while drain(&mut stream)? {}
            }
            stream.finish().map_err(str::to_owned)?;
            while drain(&mut stream)? {}
            let stats = stream.stats();
            if delivered != packets.len()
                || delivered_groups != input_index / hop as u64
                || components != delivered_groups * bins.len() as u64 * 2
                || stats.submitted != delivered as u64
                || stats.completed != delivered as u64
                || stats.groups != delivered_groups
                || stats.task_capacity != MAX_IN_FLIGHT as u32
                || stats.result_capacity != MAX_IN_FLIGHT as u32
                || stats.max_pending > MAX_IN_FLIGHT as u32
                || stats.max_ready > MAX_IN_FLIGHT as u32
            {
                return Err(format!("contract fs={fs} bins={selection}: missing/unbounded output"));
            }
            println!(
                "contract fs={fs:.0} m={m} hop={hop} taps={taps} bins={selection} selected={} packets={delivered} groups={delivered_groups} components={components} max_error={max_error:.9} finite=true metadata=true ordered=true clock=true",
                bins.len()
            );
        }
    }
    Ok(())
}

fn run(seconds: f64, bin_count: u32, fine: bool, fused: bool, decimate: i32, taps: usize) -> Result<(), String> {
    if !seconds.is_finite() || seconds <= 0.0 || !(1..=128).contains(&bin_count) {
        return Err(USAGE.into());
    }
    let target = (FS * seconds / N as f64).round() as usize;
    let bins: Vec<u32> = (0..bin_count).collect();
    let mut stream = PfbStream::with_taps(FS, N, bins.clone(), 7, taps).ok_or("PFB stream unavailable")?;
    let mut channels: Vec<FineChannel> = if fine {
        bins.iter()
            .map(|&bin| {
                FineChannel::new(
                    FineSpec {
                        id: bin as u64 + 1,
                        epoch: 7,
                        first_seq: 0,
                        bin,
                        offset_hz: 0.0,
                        decimate,
                        coarse_delay_samples: stream.delay_samples,
                        fused_fir: fused,
                    },
                    &bins,
                    stream.sample_rate,
                    stream.bin_count() / 2,
                    N / 64 + 2,
                )
                .ok_or("fine DDC unavailable")
            })
            .collect::<Result<_, _>>()?
    } else {
        Vec::new()
    };
    let mut fine_indices = vec![0u64; channels.len()];
    let mut seed = 20261019u32;
    let blocks: Vec<Vec<i16>> = (0..120)
        .map(|_| {
            (0..2 * N)
                .map(|_| {
                    seed ^= seed << 13;
                    seed ^= seed >> 17;
                    seed ^= seed << 5;
                    (seed >> 8) as i16
                })
                .collect()
        })
        .collect();
    let mut planner = FftPlanner::<f32>::new();
    let fft = planner.plan_fft_forward(2048);
    let mut spectrum = vec![Complex::<f32>::default(); 2048];
    let mut next_fft = Instant::now();
    let start = Instant::now();
    let mut frames = 0;
    let mut delivered = 0u64;
    let mut groups = 0u64;
    let mut output_bytes = 0u64;
    let mut fine_bytes = 0u64;
    let mut checksum = 0f64;
    let mut worker_ns = 0u64;
    let mut worker_max_ns = 0u64;
    let mut fine_ns = 0u64;
    let mut drain = |stream: &mut PfbStream| -> bool {
        let Some(item) = stream.take() else {
            return false;
        };
        assert_eq!(
            (item.seq, item.stamp, item.input_index, item.epoch),
            (
                delivered,
                1000 + delivered * (N * 2) as u64,
                delivered * N as u64,
                7
            )
        );
        assert_eq!(item.iq.len(), item.groups as usize * bin_count as usize * 2);
        delivered += 1;
        groups += item.groups as u64;
        output_bytes += (item.iq.len() * 4) as u64;
        worker_ns += item.process_ns;
        worker_max_ns = worker_max_ns.max(item.process_ns);
        checksum += item.iq.first().copied().unwrap_or_default() as f64;
        let fine_started = Instant::now();
        for (channel, index) in channels.iter_mut().zip(&mut fine_indices) {
            let result = channel
                .process(&item)
                .expect("fine channel lost time/epoch");
            assert_eq!(result.output_index, *index);
            *index += (result.iq.len() / 2) as u64;
            fine_bytes += (result.iq.len() * 4) as u64;
        }
        fine_ns += fine_started.elapsed().as_nanos() as u64;
        true
    };
    for seq in 0..target {
        let iq = &blocks[seq % blocks.len()];
        if Instant::now() >= next_fft {
            for (i, x) in spectrum.iter_mut().enumerate() {
                let window = 0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / 2047.0).cos();
                x.re = iq[2 * (N - 2048 + i)] as f32 * window;
                x.im = iq[2 * (N - 2048 + i) + 1] as f32 * window;
            }
            fft.process(&mut spectrum);
            frames += 1;
            next_fft = Instant::now() + Duration::from_millis(50);
        }
        loop {
            match stream.submit(seq as u64, 1000 + seq as u64 * (N * 2) as u64, iq) {
                Ok(()) => break,
                Err(-2) if drain(&mut stream) => (),
                Err(status) => return Err(format!("PFB submit status={status}")),
            }
        }
        while drain(&mut stream) {}
    }
    stream.finish().map_err(|_| "PFB finish failed")?;
    while drain(&mut stream) {}
    let elapsed = start.elapsed().as_secs_f64();
    let stats = stream.stats();
    if delivered != target as u64
        || stats.submitted != delivered
        || stats.completed != delivered
        || stats.groups != groups
        || stats.task_capacity != MAX_IN_FLIGHT as u32
        || stats.result_capacity != MAX_IN_FLIGHT as u32
        || stats.max_pending > MAX_IN_FLIGHT as u32
        || stats.max_ready > MAX_IN_FLIGHT as u32
    {
        return Err("unbounded or missing coarse-bin IQ".into());
    }
    let rate = target as f64 * N as f64 / elapsed / 1e6;
    println!(
        "bins={bin_count} taps={taps} fine={fine} fused={fused} decimate={decimate} packets={target} msps={rate:.3} fps={:.2} groups={groups} \
              output_mb={:.1} fine_mb={:.1} fine_ms_per_packet={:.4} checksum={checksum:.3} ordered={delivered} worker_mean_ms={:.4} worker_max_ms={:.4} \
              task_capacity={} result_capacity={} pending_max={} ready_max={}",
        frames as f64 / elapsed,
        output_bytes as f64 / 1e6,
        fine_bytes as f64 / 1e6,
        fine_ns as f64 / delivered as f64 / 1e6,
        worker_ns as f64 / delivered as f64 / 1e6,
        worker_max_ns as f64 / 1e6,
        stats.task_capacity,
        stats.result_capacity,
        stats.max_pending,
        stats.max_ready
    );
    Ok(())
}

fn main() {
    let args: Vec<_> = std::env::args().collect();
    if args.len() == 2 && args[1] == "--help" {
        println!("{USAGE}");
        return;
    }
    let result = (|| -> Result<(), String> {
        if args.len() == 2 && args[1] == "--contract" {
            return contract();
        }
        let (args, taps) = if args.len() >= 3 && args[args.len() - 2] == "--taps" {
            let taps = args[args.len() - 1]
                .parse::<usize>()
                .map_err(|e| format!("{e}\n{USAGE}"))?;
            if !matches!(taps, 4 | 8 | 16) {
                return Err(USAGE.into());
            }
            (&args[..args.len() - 2], taps)
        } else {
            (&args[..], 16)
        };
        if args.len() > 5
            || (args.len() >= 4 && !["--fine", "--fused"].contains(&args[3].as_str()))
            || args[1..].iter().any(|arg| arg == "--contract" || arg == "--help")
        {
            return Err(USAGE.into());
        }
        let seconds = args
            .get(1)
            .map_or("3", String::as_str)
            .parse::<f64>()
            .map_err(|e| format!("{e}\n{USAGE}"))?;
        let bins = args
            .get(2)
            .map_or("32", String::as_str)
            .parse::<u32>()
            .map_err(|e| format!("{e}\n{USAGE}"))?;
        let decimate = args
            .get(4)
            .map_or("12", String::as_str)
            .parse::<i32>()
            .map_err(|e| format!("{e}\n{USAGE}"))?;
        run(
            seconds,
            bins,
            args.len() >= 4,
            args.get(3).is_some_and(|s| s == "--fused"),
            decimate,
            taps,
        )
    })();
    if let Err(e) = result {
        eprintln!("output bench failed: {e}");
        std::process::exit(1);
    }
}
