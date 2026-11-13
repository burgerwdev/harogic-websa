use super::*;
use transport::crc;

fn section(table: u8, id: u16, extra: &[u8]) -> Vec<u8> {
    let mut s = vec![table, 0xb0, 0, (id >> 8) as u8, id as u8, 0xc1, 0, 0];
    s.extend_from_slice(extra);
    let size = s.len()+1;
    s[1] |= (size >> 8) as u8; s[2] = size as u8;
    s.extend_from_slice(&crc(&s).to_be_bytes());
    s
}
fn packet(pid: u16, cc: u8, start: bool, payload: &[u8]) -> Vec<u8> {
    assert!(payload.len() <= 184);
    let mut p = vec![0x47, (pid >> 8) as u8 | if start {0x40} else {0}, pid as u8, 0x10 | cc];
    p.extend_from_slice(payload); p.resize(188, 0xff); p
}
fn psi(pid: u16, cc: u8, section: &[u8]) -> Vec<u8> {
    let mut payload = vec![0]; payload.extend_from_slice(section); packet(pid, cc, true, &payload)
}
fn pat() -> Vec<u8> { section(0, 1, &[0, 1, 0xe1, 0]) }
fn pmt() -> Vec<u8> { section(2, 1, &[0xe1, 1, 0xf0, 0, 0x42, 0xe1, 1, 0xf0, 0, 0x03, 0xe1, 2, 0xf0, 0]) }

#[test]
fn fragmented_transport_discovers_programs_and_checks_crc() {
    let mut transport = Transport::default();
    let mut bad = pat(); bad[8] ^= 1;
    transport.push(&psi(0, 0, &bad));
    assert!(transport.programs.is_empty());
    let mut input = psi(0, 1, &pat()); input.extend(psi(256, 0, &pmt()));
    let name = "长沙电视";
    let encoded = encoding_rs::GBK.encode(name).0;
    let mut descriptor = vec![1, 0, (encoded.len()+1) as u8, 0x13]; descriptor.extend_from_slice(&encoded);
    let mut payload = vec![0, 1, 0xff, 0, 1, 0xfc, 0x80, (descriptor.len()+2) as u8, 0x48, descriptor.len() as u8];
    payload.extend(descriptor);
    input.extend(psi(17, 0, &section(0x42, 1, &payload)));
    for chunk in input.chunks(13) { transport.push(chunk); }
    let program = &transport.programs[&1];
    assert_eq!(program.name, name);
    assert_eq!((program.video.as_str(), program.audio.as_str()), ("AVS+", "MPEG audio"));
    assert_eq!(transport.continuity_errors, 0);
}

#[test]
fn section_continuation_gap_never_invents_programs() {
    let mut transport = Transport::default();
    let mut entries = Vec::new();
    for id in 1..=50u16 { entries.extend_from_slice(&id.to_be_bytes()); entries.extend_from_slice(&(0xe100+id).to_be_bytes()); }
    let section = section(0, 1, &entries);
    let mut first = vec![0]; first.extend_from_slice(&section[..183]);
    transport.push(&packet(0, 0, true, &first));
    transport.push(&packet(0, 2, false, &section[183..]));
    assert!(transport.programs.is_empty()); assert_eq!(transport.continuity_errors, 1);
    transport.push(&packet(0, 3, true, &first));
    transport.push(&packet(0, 4, false, &section[183..]));
    assert_eq!(transport.programs.len(), 50);
    transport.push(&psi(0, 5, &pat()));
    assert_eq!(transport.programs.len(), 1);
}

#[test]
fn duplicate_null_and_discontinuous_packets_are_not_false_errors() {
    let mut transport = Transport::default();
    let p = packet(257, 7, false, &[0; 40]);
    transport.push(&p); transport.push(&p);
    transport.push(&packet(8191, 14, false, &[])); transport.push(&packet(8191, 1, false, &[]));
    let mut discontinuity = packet(257, 2, false, &[]);
    discontinuity[3] = 0x32; discontinuity[4] = 1; discontinuity[5] = 0x80;
    transport.push(&discontinuity);
    transport.push(&packet(257, 3, false, &[]));
    assert_eq!(transport.continuity_errors, 0);
    let mut malformed = packet(257, 4, false, &[]); malformed[3] = 0x34; malformed[4] = 184;
    transport.push(&malformed); assert_eq!(transport.errors, 1);
}

#[test]
fn history_subscription_is_ordered_bounded_and_closed() {
    let session = Session::new();
    let bytes = packet(8191, 0, false, &[0;184]).repeat(256);
    for _ in 0..200 { session.push(&bytes); }
    let data = session.data.lock().unwrap();
    assert!(data.bytes <= HISTORY_BYTES);
    assert_eq!(data.bytes, data.history.iter().map(Bytes::len).sum::<usize>());
    let mut rx = session.tx.subscribe(); drop(data);
    session.push(&bytes);
    for chunk in bytes.chunks(TRANSPORT_BATCH) { assert_eq!(rx.try_recv().unwrap().as_ref(), chunk); }
    let tail = packet(8191, 0, false, &[0;184]);
    for byte in &tail { session.push(&[*byte]); }
    assert!(rx.try_recv().is_err());
    session.close();
    assert_eq!(rx.try_recv().unwrap().as_ref(), tail);
    assert!(rx.try_recv().unwrap().is_empty());
    assert!(!session.live.load(Ordering::Acquire));
}

#[test]
fn dtmb_rate_plan_keeps_the_entire_television_channel() {
    let plan = crate::rate_plan::RatePlan::new(15_625_000.0, "dtmb", 8_000_000.0).unwrap();
    assert_eq!(plan.decimate, 1);
    assert_eq!(plan.decoder_rate, Some(15_625_000.0));
    assert_eq!(plan.stopband_hz, 5_000_000.0);
    for input in [31_250_000.0, 62_500_000.0] {
        let wide = crate::rate_plan::RatePlan::new(input, "dtmb", 8_000_000.0).unwrap();
        assert_eq!(wide.output_rate, 15_625_000.0);
        assert_eq!(wide.decoder_rate, Some(wide.output_rate));
    }
    assert!(crate::rate_plan::RatePlan::new(7_812_500.0, "dtmb", 8_000_000.0).is_err());
}

#[test]
#[ignore = "requires WEBSA_DTMB_TEST_TS with a captured 498 MHz multiplex"]
fn recorded_multiplex_has_decodable_program_and_clean_continuity() {
    let bytes = std::fs::read(std::env::var("WEBSA_DTMB_TEST_TS").unwrap()).unwrap();
    let mut transport = Transport::default();
    for chunk in bytes.chunks(1237) { transport.push(chunk); }
    assert!(transport.packets > 1000);
    assert_eq!(transport.errors, 0); assert_eq!(transport.continuity_errors, 0);
    assert_eq!(transport.sync_losses, 0);
    assert!(transport.programs.values().any(|p| p.video == "AVS+" && !p.audio.is_empty() && !p.scrambled));
    eprintln!("{}", serde_json::to_string(&transport.programs).unwrap());
}
