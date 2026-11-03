//! Offline executable fixture, never packaged or used by the application.
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::process::{Command, Stdio};
use std::time::Duration;

fn read(input: &mut impl Read) -> Option<Value> {
    let mut prefix = [0; 4];
    input.read_exact(&mut prefix).ok()?;
    let length = u32::from_le_bytes(prefix) as usize;
    assert!(length <= 512 * 1024);
    let mut body = vec![0; length];
    input.read_exact(&mut body).ok()?;
    serde_json::from_slice(&body).ok()
}
fn write(output: &mut (impl Write + ?Sized), value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    output
        .write_all(&(body.len() as u32).to_le_bytes())
        .unwrap();
    output.write_all(&body).unwrap();
    output.flush().unwrap();
}

fn fault_diagnostics(code: Option<&str>) -> Value {
    json!({
        "primary_failure":code.map(|code| json!({
            "code":code,"stage":"preparing","observed_at_unix_ms":11,
            "controller_epoch":7,"job_id":null,"navigation_ready":false,"last_player_observation":null,
            "script_pending":false,"network_pending":false,"response_pending":false,"trigger_pending":false,
            "navigation_failure":null
        })),"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
    })
}

fn browser_failure_frames(output: &mut impl Write, mode: &str) {
    if mode == "browser_denial_before" {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":1,"diagnostics":fault_diagnostics(Some("authentication_denied"))}),
        );
    }
    if mode == "browser_paused_exit0" {
        write(
            output,
            &json!({"kind":"Paused","code":"browser_process_failed"}),
        );
    } else if mode != "browser_stopped_exit0" {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":2,"diagnostics":fault_diagnostics(Some("browser_process_failed"))}),
        );
    }
    if mode == "browser_denial_after" {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":3,"diagnostics":fault_diagnostics(Some("authentication_denied"))}),
        );
    }
    write(
        output,
        &json!({"kind":"Pulse","native_progress":4,"diagnostics":fault_diagnostics(None)}),
    );
    if mode == "browser_pulse_hang" {
        std::thread::sleep(Duration::from_secs(30));
    } else {
        let code = (mode == "browser_stopped_exit0").then_some("browser_process_failed");
        write(
            output,
            &json!({"kind":"Stopped","cleanup_confirmed":true,"diagnostics":fault_diagnostics(code)}),
        );
    }
}

fn cleanup_after_success(output: &mut impl Write, mode: &str) {
    if mode.contains("denial_before") {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":0,"diagnostics":fault_diagnostics(Some("authentication_denied"))}),
        );
    }
    let mut report = fault_diagnostics(None);
    let mut cleanup = json!({
        "no_browser_cleanup_required":false,"controller_epoch":7,
        "expected_browser_pid":100,"observed_exit_pid":100,"observed_exit_kind":1,
        "normal_exit_observed":false,"close_hresult":0,"first_detach_failure":null,
        "unsubscribe_hresult":0,"elapsed_ms":5000,"confirmed":false
    });
    if mode.contains("mismatch") {
        cleanup["observed_exit_pid"] = json!(101);
    }
    if mode.contains("zero") {
        cleanup["expected_browser_pid"] = json!(0);
        cleanup["observed_exit_pid"] = json!(0);
    }
    if mode.contains("missing") {
        cleanup["observed_exit_pid"] = Value::Null;
    }
    if mode.contains("normal") {
        cleanup["observed_exit_kind"] = json!(0);
    }
    if mode.contains("unknown") {
        cleanup["observed_exit_kind"] = json!(99);
    }
    if mode.contains("contradictory") {
        cleanup["normal_exit_observed"] = json!(true);
    }
    report["cleanup_diagnostics"] = cleanup;
    if mode.contains("pulse") {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":1,"diagnostics":report}),
        );
        write(
            output,
            &json!({"kind":"Pulse","native_progress":2,"diagnostics":fault_diagnostics(None)}),
        );
    }
    if mode.contains("denial_after") {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":3,"diagnostics":fault_diagnostics(Some("authentication_denied"))}),
        );
    }
    if mode.contains("paused") {
        write(
            output,
            &json!({"kind":"Paused","code":"browser_process_failed"}),
        );
    }
    write(
        output,
        &json!({"kind":"Stopped","cleanup_confirmed":false,"diagnostics":report}),
    );
}

fn cleanup_only_frames(output: &mut impl Write, mode: &str) {
    let source = [
        "authentication_denied",
        "integrity_denied",
        "challenge_denied",
        "rate_limited",
        "source_errors",
    ]
    .into_iter()
    .find(|code| mode.contains(code));
    let source_barrier = |output: &mut dyn Write| {
        if let Some(code) = source {
            // A generic pausing barrier is intentionally sent without a primary
            // diagnostic and may be dropped from the bounded status queue.
            write(output, &json!({"kind":"Paused","code":code}));
        }
    };
    if mode.contains("before") {
        source_barrier(output);
    }
    let mut report = fault_diagnostics(None);
    report["cleanup_diagnostics"] = json!({
        "no_browser_cleanup_required":false,"controller_epoch":7,
        "expected_browser_pid":100,"observed_exit_pid":null,"observed_exit_kind":null,
        "normal_exit_observed":false,"close_hresult":0,"first_detach_failure":null,
        "unsubscribe_hresult":0,"elapsed_ms":5050,"confirmed":false
    });
    report["host_termination"] = json!({
        "reason":"cleanup_unconfirmed","observed_at_unix_ms":12,"controller_epoch":7,"job_id":null
    });
    for operation in ["close", "detach", "unsubscribe"] {
        if mode.contains(operation) {
            let cleanup = &mut report["cleanup_diagnostics"];
            cleanup["observed_exit_pid"] = json!(100);
            cleanup["observed_exit_kind"] = json!(0);
            cleanup["normal_exit_observed"] = json!(true);
            match operation {
                "close" => cleanup["close_hresult"] = json!(-7),
                "detach" => {
                    cleanup["first_detach_failure"] =
                        json!({"operation":"network_disable_failed","hresult":-8});
                }
                _ => cleanup["unsubscribe_hresult"] = json!(-9),
            }
        }
    }
    if !mode.contains("terminal_only") {
        write(
            output,
            &json!({"kind":"Pulse","native_progress":1,"diagnostics":report}),
        );
        write(
            output,
            &json!({"kind":"Paused","code":"controller_cleanup_unconfirmed"}),
        );
        write(
            output,
            &json!({"kind":"Pulse","native_progress":2,"diagnostics":fault_diagnostics(None)}),
        );
    }
    if mode.contains("after") {
        source_barrier(output);
    }
    if mode.contains("hang") {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    write(
        output,
        &json!({"kind":"Stopped","cleanup_confirmed":false,"diagnostics":report}),
    );
}

#[cfg(windows)]
fn access_violation() -> ! {
    #[link(name = "kernel32")]
    extern "system" {
        fn SetErrorMode(mode: u32) -> u32;
        fn RaiseException(code: u32, flags: u32, count: u32, arguments: *const usize);
    }
    unsafe {
        SetErrorMode(3);
        RaiseException(0xC000_0005, 0, 0, std::ptr::null());
    }
    unreachable!("unhandled native exception returned");
}
fn main() {
    let mode = std::env::args().nth(1).unwrap();
    if mode == "descendant" || mode == "never_reads" {
        std::thread::sleep(Duration::from_secs(30));
        return;
    }
    let mut input = std::io::stdin();
    let mut output = std::io::stdout();
    let start = read(&mut input).unwrap();
    assert_eq!(start["kind"], "Start");
    if mode == "descendant_owner" || mode == "descendant_owner_wait" {
        // The exact Job is the owner; exiting before this child is intentional.
        #[allow(clippy::zombie_processes)]
        let child = Command::new(std::env::current_exe().unwrap())
            .arg("descendant")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        write(&mut output, &json!({"kind":"Ready"}));
        if mode == "descendant_owner_wait" {
            write(&mut output, &json!({"descendant":child.id()}));
            std::thread::sleep(Duration::from_secs(30));
            return;
        }
        std::thread::sleep(Duration::from_millis(300));
        return;
    }
    if mode == "unexpected_exit" {
        std::process::exit(23);
    }
    if mode.starts_with("browser_") && mode != "browser_completed_exit0" {
        browser_failure_frames(&mut output, &mode);
        return;
    }
    #[cfg(windows)]
    if matches!(
        mode.as_str(),
        "denial_pulse_then_av" | "source_denied_pulse_then_av" | "denial_stopped_then_av"
    ) {
        let code = if mode == "source_denied_pulse_then_av" {
            "source_denied"
        } else {
            "authentication_denied"
        };
        let diagnostics = json!({
            "primary_failure":{
                "code":code,"stage":"preparing","observed_at_unix_ms":11,
                "controller_epoch":7,"job_id":null,"navigation_ready":false,"last_player_observation":null,
                "script_pending":false,"network_pending":false,"response_pending":false,"trigger_pending":false,
                "navigation_failure":null
            },"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
        });
        if mode == "denial_stopped_then_av" {
            write(
                &mut output,
                &json!({"kind":"Stopped","cleanup_confirmed":true,"diagnostics":diagnostics}),
            );
        } else {
            // No Paused or Completed frame is ever sent. A later empty pulse
            // must not erase the fully received source denial before the AV.
            write(
                &mut output,
                &json!({"kind":"Pulse","native_progress":1,"diagnostics":diagnostics}),
            );
            write(
                &mut output,
                &json!({"kind":"Pulse","native_progress":2,"diagnostics":{
                    "primary_failure":null,"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
                }}),
            );
        }
        access_violation();
    }
    if mode == "stopped_then_exit" {
        write(
            &mut output,
            &json!({"kind":"Stopped","cleanup_confirmed":true,"diagnostics":{
                "primary_failure":{
                    "code":"authentication_denied","stage":"preparing","observed_at_unix_ms":11,
                    "controller_epoch":7,"job_id":null,"navigation_ready":false,"last_player_observation":null,
                    "script_pending":false,"network_pending":false,"response_pending":false,"trigger_pending":false,
                    "navigation_failure":null
                },"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
            }}),
        );
        std::process::exit(23);
    }
    #[cfg(windows)]
    if mode == "access_violation" {
        access_violation();
    }
    #[cfg(windows)]
    if mode == "sentinel" {
        #[link(name = "kernel32")]
        extern "system" {
            fn SetEvent(event: *mut std::ffi::c_void) -> i32;
        }
        let event: usize = std::env::args().nth(2).unwrap().parse().unwrap();
        unsafe {
            SetEvent(event as *mut std::ffi::c_void);
        }
        write(&mut output, &json!({"kind":"Ready"}));
        return;
    }
    if mode == "stalled_progress" {
        loop {
            write(
                &mut output,
                &json!({"kind":"Pulse","native_progress":0,"diagnostics":{
                    "primary_failure":null,"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
                }}),
            );
            std::thread::sleep(Duration::from_millis(25));
        }
    }
    if mode == "oversized" {
        output
            .write_all(&(512u32 * 1024 + 1).to_le_bytes())
            .unwrap();
        output.flush().unwrap();
        return;
    }
    if mode == "malformed" {
        output.write_all(b"\x02\x00\x00\x00{}").unwrap();
        output.flush().unwrap();
        return;
    }
    if mode == "partial" {
        output
            .write_all(b"\x64\x00\x00\x00{\"kind\":\"Completed\"")
            .unwrap();
        output.flush().unwrap();
        return;
    }
    write(&mut output, &json!({"kind":"Ready"}));
    while let Some(frame) = read(&mut input) {
        match frame["kind"].as_str().unwrap() {
            "Control" if frame["shutdown"] == true && mode == "shutdown_ignored" => {}
            "Control" if frame["shutdown"] == true => {
                write(
                    &mut output,
                    &json!({"kind":"Stopped","cleanup_confirmed":true,"diagnostics":{
                        "primary_failure":null,"cleanup_diagnostics":null,"host_termination":null,"window_dismissal":null,"worker":null
                    }}),
                );
                return;
            }
            "Sample"
                if mode == "success_then_exit"
                    || mode == "success_then_browser_exit0"
                    || mode.starts_with("success_then_failed_cleanup")
                    || mode.starts_with("success_then_cleanup_") =>
            {
                write(
                    &mut output,
                    &json!({"kind":"Completed","ticket":frame["ticket"],"job_id":frame["job_id"],"channel":frame["channel"],"outcome":{
                        "kind":"Sample","sample":{
                            "received_at_unix_ms":123456,"reported_chatters":1,
                            "members":[{"account":"offline_account","roles":32}],
                            "category_counts":[0,0,0,0,0,1],"http_status":200,
                            "provenance":{
                                "request_after_trigger_verified":true,"request_after_trigger_ms":1.0,
                                "request_to_response_ms":2.0,"request_to_finished_ms":3.0,
                                "from_disk_cache":false,"from_service_worker":false,"from_prefetch_cache":false,
                                "served_from_cache_event":false,"encoded_bytes":100,"raw_encoded_data_length":100.0,
                                "request_monotonic_seconds":1.0,"response_monotonic_seconds":1.002,"finished_monotonic_seconds":1.003,
                                "finished_minus_response_ms":1.0,"timing_order":"finish_at_or_after_response_event",
                                "transport":"network_response","server_freshness":"unknown"
                            }
                        }
                    }}),
                );
                if mode == "success_then_browser_exit0" {
                    browser_failure_frames(&mut output, &mode);
                    return;
                }
                if mode.starts_with("success_then_failed_cleanup")
                    || mode.starts_with("success_then_cleanup_")
                {
                    if mode.starts_with("success_then_cleanup_only_") {
                        cleanup_only_frames(&mut output, &mode);
                        return;
                    }
                    cleanup_after_success(&mut output, &mode);
                    return;
                }
                std::process::exit(23);
            }
            "Sample" if mode == "browser_completed_exit0" => {
                write(
                    &mut output,
                    &json!({"kind":"Completed","ticket":frame["ticket"],
                    "job_id":frame["job_id"],"channel":frame["channel"],"outcome":{
                        "kind":"Failure","code":"browser_process_failed","pauses_community":false
                    }}),
                );
                write(
                    &mut output,
                    &json!({"kind":"Stopped","cleanup_confirmed":true,"diagnostics":fault_diagnostics(None)}),
                );
                return;
            }
            "Sample" if mode == "cleanup_completed_exit0" => {
                write(
                    &mut output,
                    &json!({"kind":"Completed","ticket":frame["ticket"],
                    "job_id":frame["job_id"],"channel":frame["channel"],"outcome":{
                        "kind":"Failure","code":"cleanup_unconfirmed","pauses_community":false
                    }}),
                );
                cleanup_only_frames(&mut output, "terminal_only");
                return;
            }
            "Sample" if mode == "wrong_ticket" => {
                write(
                    &mut output,
                    &json!({"kind":"Completed","ticket":frame["ticket"].as_u64().unwrap()+1,
                    "job_id":frame["job_id"],"channel":frame["channel"],"outcome":{
                        "kind":"Failure","code":"operator_cancelled","pauses_community":false
                    }}),
                );
                return;
            }
            _ => {}
        }
    }
}
