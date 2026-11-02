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
fn write(output: &mut impl Write, value: &Value) {
    let body = serde_json::to_vec(value).unwrap();
    output
        .write_all(&(body.len() as u32).to_le_bytes())
        .unwrap();
    output.write_all(&body).unwrap();
    output.flush().unwrap();
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
            "Sample" if mode == "success_then_exit" => {
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
                std::process::exit(23);
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
