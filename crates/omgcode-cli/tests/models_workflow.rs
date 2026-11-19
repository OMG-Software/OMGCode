//! `omgcode models` lists the local server's models (ADR-0066).
//!
//! These tests open no store, so children need no spawn gate.

use serde_json::{Value, json};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::process::{Command, Output, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

const FIXTURE_TIMEOUT: Duration = Duration::from_secs(8);

/// Accept one request within the fixture's own deadline, answer it with
/// `body`, and return the request head.
fn serve_once(body: &'static str) -> (String, JoinHandle<Result<String, String>>) {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind private fixture");
    listener.set_nonblocking(true).expect("bounded accept");
    let endpoint = format!("http://{}/v1", listener.local_addr().expect("address"));
    let worker = thread::spawn(move || {
        let deadline = Instant::now() + FIXTURE_TIMEOUT;
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if Instant::now() >= deadline {
                        return Err("timed out waiting for the CLI".to_owned());
                    }
                    thread::sleep(Duration::from_millis(2));
                }
                Err(error) => return Err(error.to_string()),
            }
        };
        stream
            .set_nonblocking(false)
            .map_err(|error| error.to_string())?;
        stream
            .set_read_timeout(Some(FIXTURE_TIMEOUT))
            .map_err(|error| error.to_string())?;
        let mut head = Vec::new();
        let mut buffer = [0_u8; 1024];
        while !head.windows(4).any(|window| window == b"\r\n\r\n") {
            if head.len() > 16 * 1024 || Instant::now() >= deadline {
                return Err("request head too long or too slow".to_owned());
            }
            let count = stream
                .read(&mut buffer)
                .map_err(|error| error.to_string())?;
            if count == 0 {
                return Err("the CLI closed before its head ended".to_owned());
            }
            head.extend_from_slice(&buffer[..count]);
        }
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        stream
            .write_all(response.as_bytes())
            .map_err(|error| error.to_string())?;
        String::from_utf8(head).map_err(|error| error.to_string())
    });
    (endpoint, worker)
}

fn run_models(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_omgcode"))
        .env_remove("OMGCODE_LM_TOKEN")
        .arg("models")
        .args(args)
        .stdin(Stdio::null())
        .output()
        .expect("run OMGCode models command")
}

#[test]
fn lists_the_endpoint_models_sorted_with_the_cloud_tag() {
    let (endpoint, worker) = serve_once(
        r#"{"object":"list","data":[{"id":"lfm2.5:latest","object":"model"},{"id":"kimi-k3:cloud","object":"model"}]}"#,
    );
    let output = run_models(&["--endpoint", &endpoint]);
    let head = worker.join().expect("fixture thread").expect("fixture");
    assert!(
        output.status.success(),
        "stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result: Value = serde_json::from_slice(&output.stdout).expect("JSON result");
    assert_eq!(
        result,
        json!({
            "endpoint": endpoint,
            "models": [
                {"id": "kimi-k3:cloud", "cloud_tag": true},
                {"id": "lfm2.5:latest", "cloud_tag": false},
            ],
        })
    );
    assert!(head.starts_with("GET /v1/models HTTP/1.1\r\n"), "{head}");
    assert!(
        !head.to_ascii_lowercase().contains("authorization"),
        "{head}"
    );
}

#[test]
fn a_refused_endpoint_is_a_provider_error_before_any_request() {
    let output = run_models(&["--endpoint", "http://192.0.2.1:1234/v1"]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "provider");
    assert_eq!(
        error["error"]["message"],
        "invalid local model configuration"
    );
}

#[test]
fn a_malformed_list_is_refused() {
    let (endpoint, worker) = serve_once(r#"{"data":[{"id":"a"},{"id":"a"}]}"#);
    let output = run_models(&["--endpoint", &endpoint]);
    worker.join().expect("fixture thread").expect("fixture");
    assert!(!output.status.success());
    let error: Value = serde_json::from_slice(&output.stderr).expect("JSON error");
    assert_eq!(error["error"]["code"], "provider");
    assert_eq!(error["error"]["message"], "invalid local model response");
}

#[test]
fn endpoint_and_profile_cannot_be_combined() {
    let output = run_models(&[
        "--endpoint",
        "http://127.0.0.1:1234/v1",
        "--provider-profile",
        "local",
    ]);
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
