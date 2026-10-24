mod auto_recovery {
    use super::*;

    /// These controls are persistent: fetching more frames cannot clear overflow.
    /// Only a reconfiguration whose *readback* reaches threshold can do so.
    fn controls(threshold: f64, ceiling: f64, config_status: i32) {
        #[link(name = "dl")]
        unsafe extern "C" {
            fn dlopen(path: *const std::ffi::c_char, flags: i32) -> *mut std::ffi::c_void;
            fn dlsym(handle: *mut std::ffi::c_void, symbol: *const std::ffi::c_char) -> *mut std::ffi::c_void;
        }
        // Pin the library for this process so controls survive until the owner opens.
        static HANDLE: std::sync::LazyLock<usize> = std::sync::LazyLock::new(|| {
            let path = std::ffi::CString::new(compile_fake_swp_shim()).unwrap();
            let handle = unsafe { dlopen(path.as_ptr(), 2) };
            assert!(!handle.is_null());
            handle as usize
        });
        let handle = *HANDLE as *mut std::ffi::c_void;
        unsafe {
            let symbol = dlsym(handle, b"san90_swp_fake_auto_controls_v1\0".as_ptr().cast());
            assert!(!symbol.is_null());
            let set: unsafe extern "C" fn(f64, f64, i32) = std::mem::transmute(symbol);
            set(threshold, ceiling, config_status);
        }
    }

    struct Fixture { _guard: std::sync::MutexGuard<'static, ()> }

    impl Fixture {
        fn new() -> Self {
            let guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
            controls(f64::NEG_INFINITY, f64::INFINITY, 0);
            Self { _guard: guard }
        }

        fn state(&self, reference: f64) -> DevState {
            let mut state = hardware_state(Some(compile_fake_swp_shim()));
            state.swp.mode = "std".into();
            state.swp.center = 100e6;
            state.swp.span = 246_900.0;
            state.swp.ref_level = reference;
            state
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            controls(f64::NEG_INFINITY, f64::INFINITY, 0);
            let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
        }
    }

    fn adjusting(state: &DevState) -> bool {
        state.auto.view()["adjusting"].as_bool().unwrap()
    }

    fn result(state: &DevState) -> String {
        state.auto.view()["result"].as_str().unwrap().to_owned()
    }

    fn tick(state: &mut DevState) -> Vec<u8> {
        let (_, power) = state.fetch_sweep();
        state.advance_auto();
        power
    }

    fn finish(state: &mut DevState) {
        for _ in 0..32 {
            if !adjusting(state) { return; }
            tick(state);
        }
        panic!("Auto did not terminate: {}", state.auto.view());
    }

    #[test]
    fn auto_without_history_waits_for_fresh_spectrum_and_verifies_fit() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-50.0);
        assert!(state.auto.last_peak.is_none());
        state.auto_scale(None, false);
        assert!(adjusting(&state));
        let before = state.swp.ref_level;
        state.advance_auto();
        assert_eq!(state.swp.ref_level, before, "no observation cannot drive a step");
        assert!(adjusting(&state));
        assert!(!tick(&mut state).is_empty());
        assert!(adjusting(&state), "a fit must await its own fresh verification spectrum");
        assert_ne!(state.swp.ref_level, before, "fresh carrier requires a fit");
        finish(&mut state);
        assert_eq!(result(&state), "applied");
        assert_eq!(state.status_warning, 0);
        assert_eq!(state.auto.last_peak, Some(-30.0));
    }

    #[test]
    fn auto_without_history_recovers_persistent_overflow_before_fitting() {
        let fixture = Fixture::new();
        controls(-15.0, f64::INFINITY, 0);
        let mut state = fixture.state(-45.0);
        assert!(state.auto.last_peak.is_none());
        state.auto_scale(None, false);
        assert!(tick(&mut state).is_empty());
        assert!(adjusting(&state));
        assert!(state.swp.ref_level > -45.0);
        finish(&mut state);
        assert_eq!(result(&state), "applied");
        assert!(state.actual["ref"].as_f64().unwrap() >= -15.0);
        assert_eq!(state.auto.last_peak, Some(-30.0), "completion requires a fresh clean trace");
    }

    #[test]
    fn auto_consumes_each_fresh_overflow_observation_only_once() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-45.0);
        controls(-15.0, f64::INFINITY, 0);
        state.auto_scale(None, false);
        assert!(tick(&mut state).is_empty());
        let reference = state.swp.ref_level;
        let stats = fake_swp_stats();
        let sequence = state.auto.view()["seq"].clone();
        let pending = state.auto.view()["pending"].as_f64().unwrap();
        assert_eq!(pending, reference);
        for _ in 0..6 { state.advance_auto(); }
        assert!(adjusting(&state));
        assert_eq!(state.swp.ref_level, reference);
        assert_eq!(fake_swp_stats(), stats);
        assert_eq!(state.auto.view()["seq"], sequence);
        assert!(tick(&mut state).is_empty());
        assert!(state.swp.ref_level > reference, "only the next acquisition permits another step");
        finish(&mut state);
        assert_eq!(result(&state), "applied");
    }

    #[test]
    fn auto_cached_clean_spectrum_does_not_finish_a_new_request() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-20.0);
        assert!(!state.fetch_sweep().1.is_empty());
        state.auto_scale(None, false);
        for _ in 0..4 { state.advance_auto(); }
        assert!(adjusting(&state), "cached clean spectrum is older than the click");
        finish(&mut state);
        assert!(matches!(result(&state).as_str(), "ok" | "applied"));
    }

    #[test]
    fn auto_stale_peak_cannot_hide_current_overflow() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-40.0);
        assert!(!state.fetch_sweep().1.is_empty());
        assert_eq!(state.auto.last_peak, Some(-30.0));
        controls(-15.0, f64::INFINITY, 0);
        assert!(state.fetch_sweep().1.is_empty());
        assert_eq!(state.status_warning, -12);
        state.auto_scale(None, false);
        tick(&mut state);
        assert!(adjusting(&state));
        assert!(state.swp.ref_level > -40.0, "current overflow must supersede old clean power");
        finish(&mut state);
        assert_eq!(result(&state), "applied");
        assert!(state.actual["ref"].as_f64().unwrap() >= -15.0);
    }

    #[test]
    fn manual_reference_never_recovers_persistent_overflow_without_auto() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-40.0);
        state.fetch_sweep();
        dispatch(&mut state, "SET_REF", &json!({"ref": -45.0})).unwrap();
        controls(-10.0, f64::INFINITY, 0);
        let version = state.config_version;
        let stats = fake_swp_stats();
        for _ in 0..12 {
            assert!(tick(&mut state).is_empty());
            assert_eq!(state.swp.ref_level, -45.0);
            assert_eq!(state.actual["ref"], -47.5);
            assert!(!adjusting(&state));
        }
        assert_eq!(state.status_warning, -12);
        assert_eq!(state.config_version, version);
        assert_eq!(fake_swp_stats(), stats);
    }

    #[test]
    fn auto_multistep_recovery_uses_actual_ref_and_never_lowers_recovered_level() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-45.0);
        controls(-15.0, f64::INFINITY, 0);
        assert!(state.fetch_sweep().1.is_empty());
        state.auto_scale(None, false);
        let mut steps = 0;
        let mut previous_actual = state.actual["ref"].as_f64().unwrap();
        for _ in 0..16 {
            let old_request = state.swp.ref_level;
            let power = tick(&mut state);
            let actual = state.actual["ref"].as_f64().unwrap();
            if state.swp.ref_level != old_request {
                assert_eq!(state.swp.ref_level, previous_actual + 10.0,
                    "overflow steps start at hardware readback, not prior request");
                assert!(actual > previous_actual);
                steps += 1;
            }
            previous_actual = actual;
            if !power.is_empty() {
                assert!(actual >= -15.0);
                break;
            }
            assert!(adjusting(&state));
        }
        assert!(steps >= 3, "fixture must require multiple hardware steps");
        let recovered = state.actual["ref"].as_f64().unwrap();
        finish(&mut state);
        assert_eq!(result(&state), "applied");
        assert!(state.actual["ref"].as_f64().unwrap() >= recovered,
            "clean display fit must not lower recovered hardware reference");
        assert_eq!(state.status_warning, 0);
    }

    #[test]
    fn auto_overflow_stops_at_device_reference_bound() {
        let fixture = Fixture::new();
        let mut state = fixture.state(20.0);
        controls(100.0, f64::INFINITY, 0);
        state.fetch_sweep();
        let maximum = state.caps()["ref_max"].as_f64().unwrap();
        state.auto_scale(None, false);
        for _ in 0..32 {
            tick(&mut state);
            assert!(state.swp.ref_level <= maximum);
            if !adjusting(&state) { break; }
        }
        assert!(!adjusting(&state));
        assert_eq!(result(&state), "limit");
        assert_eq!(state.status_warning, -12);
        let stopped = (state.swp.ref_level, fake_swp_stats());
        for _ in 0..3 { tick(&mut state); }
        assert_eq!((state.swp.ref_level, fake_swp_stats()), stopped);
    }

    #[test]
    fn auto_overflow_above_device_cap_never_lowers_manual_hardware_reference() {
        let fixture = Fixture::new();
        let mut state = fixture.state(40.0);
        state.golden["caps"]["ref_max"] = json!(30.0);
        controls(100.0, f64::INFINITY, 0);
        state.fetch_sweep();
        let actual = state.actual["ref"].clone();
        let stats = fake_swp_stats();
        state.auto_scale(None, false);
        tick(&mut state);
        assert_eq!(result(&state), "limit");
        assert!(!adjusting(&state));
        assert_eq!(state.swp.ref_level, 40.0);
        assert_eq!(state.actual["ref"], actual);
        assert_eq!(fake_swp_stats(), stats, "overflow cannot be corrected by lowering above-cap Ref");
    }

    #[test]
    fn auto_clamped_hardware_readback_stalls_instead_of_reporting_success() {
        let fixture = Fixture::new();
        controls(-10.0, -42.5, 0);
        let mut state = fixture.state(-40.0);
        state.fetch_sweep();
        state.auto_scale(None, false);
        finish(&mut state);
        assert_eq!(result(&state), "stalled");
        assert_eq!(state.actual["ref"], -42.5);
        assert_eq!(state.status_warning, -12);
        assert!(state.fetch_sweep().1.is_empty(), "overflow cannot clear by exhausting a counter");
    }

    #[test]
    fn auto_configuration_failure_is_terminal_and_preserves_warning() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-40.0);
        controls(-10.0, f64::INFINITY, 0);
        state.fetch_sweep();
        let actual = state.actual["ref"].clone();
        controls(-10.0, f64::INFINITY, -7);
        state.auto_scale(None, false);
        finish(&mut state);
        assert_eq!(result(&state), "failed");
        assert_eq!(state.actual["ref"], actual);
        assert_eq!(state.status_warning, -12);
        let stats = fake_swp_stats();
        state.advance_auto();
        assert_eq!(fake_swp_stats(), stats, "failed session cannot retry configuration");
    }

    #[test]
    fn auto_manual_reference_retune_and_mode_change_cancel_pending_recovery() {
        let fixture = Fixture::new();
        for (cmd, data) in [
            ("SET_REF", json!({"ref": -25.0})),
            ("SET_FREQ", json!({"center": 101e6, "span": 1e6})),
            ("SET_MODE", json!({"mode": "harmonic"})),
        ] {
            let mut state = fixture.state(-40.0);
            controls(-10.0, f64::INFINITY, 0);
            state.fetch_sweep();
            state.auto_scale(None, false);
            tick(&mut state);
            assert!(adjusting(&state));
            dispatch(&mut state, cmd, &data).unwrap();
            assert!(!adjusting(&state), "{cmd} must cancel pending correction");
            assert_eq!(result(&state), "cancelled", "{cmd}");
            let reference = state.swp.ref_level;
            let stats = fake_swp_stats();
            for _ in 0..3 { state.advance_auto(); }
            assert_eq!(state.swp.ref_level, reference);
            assert_eq!(fake_swp_stats(), stats);
        }
    }

    #[test]
    fn auto_trigger_wait_never_raises_reference_and_can_resume_on_fresh_power() {
        let fixture = Fixture::new();
        for warning in [-10, -11] {
            let mut state = fixture.state(-20.0);
            state.fetch_sweep();
            state.auto_scale(None, false);
            let stats = fake_swp_stats();
            for _ in 0..4 {
                std::fs::write("/tmp/websa_fake_swp_fail.txt", format!("1 {warning}")).unwrap();
                assert!(tick(&mut state).is_empty());
                assert!(adjusting(&state));
                assert_eq!(state.swp.ref_level, -20.0);
                assert_eq!(fake_swp_stats(), stats);
            }
            std::fs::remove_file("/tmp/websa_fake_swp_fail.txt").unwrap();
            finish(&mut state);
            assert!(matches!(result(&state).as_str(), "ok" | "applied"));
        }
    }

    #[test]
    fn auto_relock_warnings_wait_for_clean_acquisition_before_fitting() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-50.0);
        state.auto_scale(None, false);
        let reference = state.swp.ref_level;
        for warning in [-36, -37, -38, -39] {
            state.note_auto_acquisition(warning);
            state.advance_auto();
            assert!(adjusting(&state), "warning {warning} must keep Auto pending");
            assert_ne!(result(&state), "failed", "warning {warning}");
            assert_eq!(state.swp.ref_level, reference, "warning {warning}");
        }
        assert!(!tick(&mut state).is_empty());
        assert!(adjusting(&state), "a fit must await fresh verification");
        assert_ne!(state.swp.ref_level, reference, "clean acquisition must resume fitting");
        finish(&mut state);
        assert!(!adjusting(&state));
        assert_eq!(result(&state), "applied");
    }

    #[test]
    fn auto_trigger_wait_timeout_finishes_without_reference_change() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-20.0);
        state.fetch_sweep();
        state.auto_scale(None, false);
        std::fs::write("/tmp/websa_fake_swp_fail.txt", "1 -10").unwrap();
        tick(&mut state);
        state.auto.session.as_mut().unwrap().deadline = std::time::Instant::now() - Duration::from_secs(1);
        state.advance_auto();
        assert!(!adjusting(&state));
        assert_eq!(result(&state), "no_data");
        assert_eq!(state.swp.ref_level, -20.0);
    }

    #[test]
    fn auto_pending_step_timeout_clears_target_without_claiming_application() {
        let fixture = Fixture::new();
        let mut state = fixture.state(-45.0);
        controls(-15.0, f64::INFINITY, 0);
        state.auto_scale(None, false);
        assert!(tick(&mut state).is_empty());
        assert!(state.auto.view()["pending"].is_number());
        assert!(adjusting(&state));
        let stats = fake_swp_stats();
        state.auto.session.as_mut().unwrap().deadline = std::time::Instant::now() - Duration::from_secs(1);
        state.advance_auto();
        assert!(!adjusting(&state));
        assert_eq!(result(&state), "timeout");
        assert!(state.auto.view()["target"].is_null());
        assert!(state.auto.view()["pending"].is_null());
        assert_eq!(fake_swp_stats(), stats);
        assert_eq!(state.status_warning, -12);
    }

    #[test]
    fn clean_sdr_auto_entries_fit_fresh_power_without_hardware_reconfiguration() {
        let fixture = Fixture::new();
        for (cmd, data) in [
            ("AUTO_SCALE", json!({"display_only": true, "current_ref": -80.0})),
            ("SET_REF", json!({"mode": "auto", "ref": -80.0})),
        ] {
            let mut state = fixture.state(-40.0);
            state.swp.mode = "sdr".into();
            state.sdr_ref = -35.0;
            state.sdr_actual = json!({"ref": -37.5, "atten": 14, "preamp": 1, "ifgain": 3});
            let actual = state.sdr_actual.clone();
            let version = state.config_version;
            let stats = fake_swp_stats();
            dispatch(&mut state, cmd, &data).unwrap();
            assert!(adjusting(&state));
            // The SDR presentation path observes only a fresh spectrum; fitting it
            // must never attempt IQS reconfiguration (the shim has no IQS ABI).
            state.note_auto_acquisition(0);
            state.observe_peak(&[-95.0, -90.0, -30.0, -90.0], None);
            state.advance_auto();
            assert!(adjusting(&state), "display placement also waits for fresh verification");
            state.note_auto_acquisition(0);
            state.observe_peak(&[-95.0, -90.0, -30.0, -90.0], None);
            state.advance_auto();
            assert!(!adjusting(&state));
            assert_eq!(result(&state), "applied");
            assert!(state.auto.view()["target"].as_f64().unwrap() >= -20.0);
            assert_eq!(state.sdr_ref, -35.0);
            assert_eq!(state.sdr_actual, actual);
            assert_eq!(state.config_version, version);
            assert_eq!(fake_swp_stats(), stats);
            assert!(state.ddc.is_none() && state.pipeline.is_none());
        }
    }
}
