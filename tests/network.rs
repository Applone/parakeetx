use std::{io::{Read, Write}, net::{TcpListener, TcpStream}, sync::{Arc, atomic::{AtomicBool, Ordering}}, thread, time::{Duration, Instant}};

use serde_json::{Value, json};
use parakeetx::{download, settings::Settings, summary};

fn read_request(stream: &mut TcpStream) -> (String, Value) {
    stream.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    let mut bytes = Vec::new();
    let mut buffer = [0u8; 4096];
    let header_end = loop {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") { break index + 4; }
    };
    let header = String::from_utf8_lossy(&bytes[..header_end]).into_owned();
    let length = header.lines().find_map(|line| line.to_lowercase().strip_prefix("content-length:").map(|value| value.trim().parse::<usize>().unwrap())).unwrap_or(0);
    while bytes.len() < header_end + length {
        let count = stream.read(&mut buffer).unwrap();
        assert!(count > 0);
        bytes.extend_from_slice(&buffer[..count]);
    }
    let body = if length == 0 { Value::Null } else { serde_json::from_slice(&bytes[header_end..header_end + length]).unwrap() };
    (header, body)
}

fn response(stream: &mut TcpStream, status: &str, body: &[u8]) {
    write!(stream, "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", body.len()).unwrap();
    stream.write_all(body).unwrap();
}

#[test]
fn compatible_summary_api_receives_prompt_language_model_and_auth() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let (header, body) = read_request(&mut stream);
        assert!(header.starts_with("POST /v1/chat/completions"));
        assert!(header.to_lowercase().contains("authorization: bearer test-key"));
        assert_eq!(body["model"], "test-local-model");
        assert!(body["messages"][0]["content"].as_str().unwrap().contains("Russian"));
        assert!(body["messages"][1]["content"].as_str().unwrap().contains("Lesson content"));
        response(&mut stream, "200 OK", &serde_json::to_vec(&json!({"choices":[{"message":{"content":"# Notes\n\nKey idea."},"finish_reason":"stop"}]})).unwrap());
    });
    let settings = Settings { summary_base_url: base, summary_model: "test-local-model".into(), summary_language: "Russian".into(), ..Default::default() };
    let result = summary::summarize(&settings, "test-key", "Lesson content", &AtomicBool::new(false), |_| {}).unwrap();
    assert!(result.starts_with("# Notes"));
    server.join().unwrap();
}

#[test]
fn long_summaries_merge_in_bounded_passes() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let base = format!("http://{}/v1", listener.local_addr().unwrap());
    let transcript = (0..12).map(|index| format!("Part {index} {}", "detail ".repeat(100))).collect::<Vec<_>>().join("\n");
    let parts = summary::chunk(&transcript, 2000).len();
    assert!(parts > 1);
    let server = thread::spawn(move || {
        for _ in 0..parts + 1 {
            let (mut stream, _) = listener.accept().unwrap();
            let (_, body) = read_request(&mut stream);
            assert!(body["messages"][1]["content"].as_str().unwrap().chars().count() <= 2300);
            response(&mut stream, "200 OK", br##"{"choices":[{"message":{"content":"Concise lesson notes."}}]}"##);
        }
    });
    let settings = Settings { summary_base_url: base, summary_model: "local".into(), summary_chunk_chars: 2000, ..Default::default() };
    assert_eq!(summary::summarize(&settings, "", &transcript, &AtomicBool::new(false), |_| {}).unwrap(), "Concise lesson notes.");
    server.join().unwrap();
}

#[test]
fn api_errors_redact_credentials_and_truncated_completions_are_rejected() {
    for (status, body) in [
        ("401 Unauthorized", json!({"error":{"message":"invalid test-secret"}})),
        ("200 OK", json!({"choices":[{"message":{"content":"Incomplete"},"finish_reason":"length"}]})),
        ("200 OK", json!({"choices":[{"message":{"content":null}}]})),
    ] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let base = format!("http://{}/v1", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            response(&mut stream, status, &serde_json::to_vec(&body).unwrap());
        });
        let settings = Settings { summary_base_url: base, summary_model: "local".into(), ..Default::default() };
        let error = summary::summarize(&settings, "test-secret", "A lesson", &AtomicBool::new(false), |_| {}).unwrap_err();
        assert!(!format!("{error:#}").contains("test-secret"));
        server.join().unwrap();
    }
}

#[test]
fn downloads_verify_checksums_and_preserve_existing_files_on_failure() {
    let directory = tempfile::tempdir().unwrap();
    let destination = directory.path().join("model.bin");
    std::fs::write(&destination, b"existing-model").unwrap();
    for valid in [false, true] {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let url = format!("http://{}/model", listener.local_addr().unwrap());
        let server = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            read_request(&mut stream);
            response(&mut stream, "200 OK", if valid { b"abc" } else { b"invalid-model" });
        });
        let result = download::fetch(&url, &destination, Some("a9993e364706816aba3e25717850c26c9cd0d89d"), &AtomicBool::new(false), |_| {});
        assert_eq!(result.is_ok(), valid);
        assert_eq!(std::fs::read(&destination).unwrap(), if valid { b"abc".to_vec() } else { b"existing-model".to_vec() });
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        server.join().unwrap();
    }
}

#[test]
fn stalled_download_can_be_cancelled_without_installing_partial_data() {
    let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
    let url = format!("http://{}/model", listener.local_addr().unwrap());
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    let server = thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        read_request(&mut stream);
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: 50000\r\n\r\nabc").unwrap();
        thread::sleep(Duration::from_millis(100));
        signal.store(true, Ordering::Relaxed);
        thread::sleep(Duration::from_millis(300));
    });
    let directory = tempfile::tempdir().unwrap();
    let start = Instant::now();
    assert!(download::fetch(&url, &directory.path().join("model.bin"), None, &cancel, |_| {}).is_err());
    assert!(start.elapsed() < Duration::from_secs(2));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 0);
    server.join().unwrap();
}
