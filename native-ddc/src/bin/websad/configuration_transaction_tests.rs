mod configuration_transactions {
    use super::*;

    fn rejecting_swp_state() -> DevState {
        let source = FAKE_SWP_SHIM.replace(
            "if (fake_config_status) return fake_config_status;",
            "return -7;",
        );
        let mut state = hardware_state(Some(compile_fake_swp_source("reject_configuration", &source)));
        state.fetch_sweep();
        state
    }

    #[test]
    fn rejected_swp_commands_keep_request_actual_and_running_profile() {
        let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
        let mut state = rejecting_swp_state();
        let old = state.status();
        let old_params = state.swp_params();
        let versions = (state.config_version, state.freq_version);
        let opens = state.swp_open_count;
        let requests = [
            json!({"cmd":"SET_FREQ", "center":200e6, "span":2e6}),
            json!({"cmd":"SET_POINTS", "points":1001}),
            json!({"cmd":"SET_RBW", "mode":"manual", "rbw":30000.0}),
            json!({"cmd":"SET_VBW", "mode":"manual", "vbw":1000.0}),
            json!({"cmd":"SET_SWEEP", "mode":1, "time":0.2}),
            json!({"cmd":"SET_SPUR", "mode":"standard"}),
            json!({"cmd":"SET_WINDOW", "window":2}),
            json!({"cmd":"SET_DETECTOR", "mode":"pos_peak"}),
            json!({"cmd":"SET_FRAMEDET", "mode":"average"}),
            json!({"cmd":"SET_TRACEGRID", "trace_align":"center", "points_strategy":"accuracy"}),
            json!({"cmd":"SET_REF", "ref":-35.0}),
            json!({"cmd":"SET_AMP", "atten":10, "preamp":1, "ifgain":3}),
            json!({"cmd":"SET_REFCK", "mode":"external"}),
            json!({"cmd":"SET_REFCKOUT", "on":true}),
            json!({"cmd":"SET_PRESET"}),
        ];
        for request in requests {
            let cmd = request["cmd"].as_str().unwrap();
            let error = dispatch(&mut state, cmd, &request).unwrap_err();
            assert_eq!(error.code, "device_error", "{cmd}: {}", error.message);
            assert!(state.swp_params() == old_params, "{cmd}: requested profile changed");
            let status = state.status();
            assert_eq!(status["req"], old["req"], "{cmd}: request changed");
            assert_eq!(status["actual"], old["actual"], "{cmd}: readback changed");
            assert_eq!(status["amp"], old["amp"], "{cmd}: front end changed");
            assert_eq!((state.config_version, state.freq_version), versions, "{cmd}");
            assert!(state.swp_hw.is_some(), "{cmd}: owner dropped");
            assert!(state.swp_retry_at.is_none(), "{cmd}: parameter rejection scheduled reopen");
            assert!(state.link_up, "{cmd}: parameter rejection marked link lost");
            let (axis, power) = state.fetch_sweep();
            assert!(axis.is_none(), "{cmd}: rejected change republished an axis");
            assert_eq!(decode_header(&power).0, "POWR", "{cmd}: stream stopped");
            assert_eq!(state.swp_open_count, opens, "{cmd}: recreated handle");
        }
    }

    #[test]
    fn rejected_rta_commands_keep_request_actual_and_running_profile() {
        let source = FAKE_RTA_SHIM
            .replace("permits{0}", "permits{10000}")
            .replace("if (r->trigger_level_dbm == -123.0)", "if (true)");
        let mut state = hardware_state(Some(compile_fake_swp_source("reject_rta_configuration", &source)));
        state.swp.mode = "rta".into();
        state.step_rta();
        let old = state.status();
        let versions = (state.config_version, state.freq_version);
        let old_request = state.rta_params();
        let old_trigger = old["req"]["rta"]["trigger_actual"].clone();
        for request in [
            json!({"cmd":"SET_RTA", "center":200e6, "span":10e6}),
            json!({"cmd":"SET_RBW", "mode":"manual", "rbw":30000.0}),
            json!({"cmd":"SET_VBW", "mode":"manual", "vbw":1000.0}),
            json!({"cmd":"SET_SWEEP", "mode":1, "time":0.2}),
            json!({"cmd":"SET_REF", "ref":-35.0}),
            json!({"cmd":"SET_AMP", "atten":10, "preamp":1, "ifgain":3}),
            json!({"cmd":"SET_REFCK", "mode":"external"}),
            json!({"cmd":"SET_REFCKOUT", "on":true}),
            json!({"cmd":"SET_PRESET"}),
        ] {
            let cmd = request["cmd"].as_str().unwrap();
            let error = dispatch(&mut state, cmd, &request).unwrap_err();
            assert_eq!(error.code, "device_error", "{cmd}: {}", error.message);
            let status = state.status();
            let mut expected = old["req"].clone();
            // The fake's rejected reconfig restarts the old capture: packets keep
            // flowing and only live capture metadata advances.
            expected["rta"]["trigger_actual"] = status["req"]["rta"]["trigger_actual"].clone();
            assert_eq!(status["req"], expected, "{cmd}: request changed");
            assert!(state.rta_params() == old_request, "{cmd}: requested profile changed");
            for field in ["source", "edge", "level", "safetime", "delay", "pretime", "acqtime",
                "retrigger", "retriggerperiod", "out", "outpolarity"] {
                assert_eq!(status["req"]["rta"]["trigger_actual"][field], old_trigger[field],
                    "{cmd}: applied trigger {field} changed");
            }
            assert_eq!(status["actual"], old["actual"], "{cmd}: readback changed");
            assert_eq!(status["amp"], old["amp"], "{cmd}: front end changed");
            assert_eq!((state.config_version, state.freq_version), versions, "{cmd}");
            assert!(state.rta_retry_at.is_none(), "{cmd}: parameter rejection scheduled reopen");
            assert!(state.link_up, "{cmd}: parameter rejection marked link lost");
            let frame = state.rta_hw.as_ref().expect("retained RTA owner").next().unwrap();
            assert_eq!(frame.geometry.start, old["actual"]["start"].as_f64().unwrap());
            assert_eq!(frame.geometry.stop, old["actual"]["stop"].as_f64().unwrap());
            assert_eq!(frame.freq.first().copied(), Some(frame.geometry.start));
            assert_eq!(frame.freq.last().copied(), Some(frame.geometry.stop));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn rejected_swp_configuration_keeps_http_and_ws_request_identity() {
        let _guard = TEST_SWP_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let _ = std::fs::remove_file("/tmp/websa_fake_swp_fail.txt");
        let (app, url) = start_ws_server().await;
        {
            let mut state = app.state.lock().unwrap();
            *state = rejecting_swp_state();
        }
        let mut ws = connect_ws(&url).await;
        // The display socket replays the last axis once on connect.
        let _ = next_ws(&mut ws).await;
        wait_for_client(&app).await;
        for request_id in [json!("acq-rejected"), json!(42), json!(false), Value::Null] {
            let request = json!({"cmd":"SET_FREQ", "center":200e6, "span":2e6, "request_id":request_id});
            let before = app.state.lock().unwrap().status();
            let response = api_config(State(app.clone()), HeaderMap::new(), request.to_string()).await;
            assert_eq!(response.status(), StatusCode::BAD_REQUEST);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["code"], "device_error");
            assert_eq!(body["response_to"], "SET_FREQ");
            assert_eq!(body.get("request_id"), Some(&request_id));
            ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
            let reply = next_text(&mut ws).await;
            assert_eq!(reply["cmd"], "ERROR");
            assert_eq!(reply["code"], "device_error");
            assert_eq!(reply["response_to"], "SET_FREQ");
            assert_eq!(reply.get("request_id"), Some(&request_id));
            let mut state = app.state.lock().unwrap();
            let after = state.status();
            assert_eq!(after["req"], before["req"]);
            assert_eq!(after["actual"], before["actual"]);
            assert_eq!(app.epoch.load(Ordering::Acquire), 0);
            assert_eq!(decode_header(&state.fetch_sweep().1).0, "POWR");
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn parsed_command_success_echoes_scalar_identity_without_leaking_to_publisher() {
        let (app, url) = start_ws_server().await;
        {
            let (golden, schema) = load_golden();
            *app.state.lock().unwrap() = DevState::new(golden, schema);
        }
        let mut ws = connect_ws(&url).await;
        wait_for_client(&app).await;
        for request_id in [json!("acq-success"), json!(7), json!(true), Value::Null] {
            let request = json!({"cmd":"SET_FREQ", "center":200e6, "span":2e6, "request_id":request_id});
            let response = api_config(State(app.clone()), HeaderMap::new(), request.to_string()).await;
            assert_eq!(response.status(), StatusCode::OK);
            let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["response_to"], "SET_FREQ");
            assert_eq!(body.get("request_id"), Some(&request_id));
            ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
            let reply = next_text(&mut ws).await;
            assert_eq!(reply["cmd"], "STATUS");
            assert_eq!(reply["response_to"], "SET_FREQ");
            assert_eq!(reply.get("request_id"), Some(&request_id));
        }
        ws.send(WsMessage::Text(json!({"cmd":"STATUS", "request_id":"poll"}).to_string().into())).await.unwrap();
        assert_eq!(next_text(&mut ws).await["request_id"], "poll");
        for request_id in [json!({"nested":1}), json!([1])] {
            ws.send(WsMessage::Text(json!({"cmd":"STATUS", "request_id":request_id}).to_string().into())).await.unwrap();
            assert!(next_text(&mut ws).await.get("request_id").is_none());
        }
        assert!(app.state.lock().unwrap().status().get("request_id").is_none());
    }
    #[tokio::test(flavor = "current_thread")]
    async fn am_sync_http_and_ws_confirm_partial_state_and_reject_wrong_types() {
        let (app, url) = start_ws_server().await;
        {
            let (golden, schema) = load_golden();
            let mut state = app.state.lock().unwrap();
            *state = DevState::new(golden, schema);
            state.swp.mode = "sdr".into();
            dispatch(&mut state,"SET_SDR_CHANNEL",&json!({"mode":"am","volume":0.35,
                "nr":true,"nr_strength":0.4,"agc":false,"squelch":-130.0})).unwrap();
        }
        let mut ws = connect_ws(&url).await;
        wait_for_client(&app).await;
        let initial = app.state.lock().unwrap().channel_configs();
        for enabled in [true,false] {
            let request=json!({"cmd":"SET_SDR_CHANNEL","channel":1,"am_sync":enabled,
                "request_id":format!("am-sync-{enabled}")});
            let response=api_config(State(app.clone()),HeaderMap::new(),request.to_string()).await;
            assert_eq!(response.status(),StatusCode::OK);
            let bytes=axum::body::to_bytes(response.into_body(),usize::MAX).await.unwrap();
            let body:Value=serde_json::from_slice(&bytes).unwrap();
            assert_eq!(body["sdr"]["channels"][0]["am_sync"],enabled);
            assert_eq!(body["sdr"]["am_sync"],enabled);
            assert_eq!(body["request_id"],request["request_id"]);
            ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
            let reply=next_text(&mut ws).await;
            assert_eq!(reply["cmd"],"STATUS");
            assert_eq!(reply["sdr"]["channels"][0]["am_sync"],enabled);
            let mut expected=initial.clone();
            expected[0].am_sync=enabled;
            expected[0].am_sync_generation=if enabled {1} else {2};
            assert_eq!(app.state.lock().unwrap().channel_configs(),expected);
        }
        for invalid in [json!("false"),json!(0),Value::Null] {
            let before=app.state.lock().unwrap().channel_configs();
            let request=json!({"cmd":"SET_SDR_CHANNEL","am_sync":invalid,"volume":1.0,"request_id":"bad-am-sync"});
            let response=api_config(State(app.clone()),HeaderMap::new(),request.to_string()).await;
            assert_eq!(response.status(),StatusCode::BAD_REQUEST);
            ws.send(WsMessage::Text(request.to_string().into())).await.unwrap();
            let reply=next_text(&mut ws).await;
            assert_eq!(reply["cmd"],"ERROR");
            assert_eq!(reply["code"],"bad_parameter");
            assert_eq!(reply["request_id"],"bad-am-sync");
            assert_eq!(app.state.lock().unwrap().channel_configs(),before);
        }
    }
}
