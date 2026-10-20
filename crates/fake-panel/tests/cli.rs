//! Runs the `fake-panel` binary and checks its machine-readable startup line.
#![allow(clippy::unwrap_used, clippy::expect_used)]

mod common;

use std::io::{BufRead as _, BufReader};
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

struct Kill(std::process::Child);

impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[tokio::test]
async fn binary_prints_listening_line_and_serves() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_fake-panel"))
        .args([
            "--port",
            "0",
            "--media",
            "never",
            "--quirks",
            "stream-host,slow,no-slow",
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .unwrap();
    let stdout = child.stdout.take().unwrap();
    let guard = Kill(child);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let line = rx.recv_timeout(Duration::from_secs(20)).unwrap();
    let base = line
        .trim()
        .strip_prefix("LISTENING ")
        .unwrap_or_else(|| panic!("unexpected first line {line:?}"))
        .to_owned();
    assert!(base.starts_with("http://127.0.0.1:"), "{base}");
    let port: u16 = base.rsplit(':').next().unwrap().parse().unwrap();
    assert_ne!(port, 0);

    let login = common::get_json(&format!(
        "{base}/player_api.php?username=demo&password=demo"
    ))
    .await;
    assert_eq!(login["user_info"]["status"], "Active");
    assert_eq!(login["server_info"]["url"], "localhost");
    assert_eq!(login["server_info"]["port"], port.to_string());
    drop(guard);
}

#[test]
fn binary_rejects_unknown_quirk() {
    let out = Command::new(env!("CARGO_BIN_EXE_fake-panel"))
        .args(["--media", "never", "--quirks", "definitely-not-a-quirk"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(!out.status.success());
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("unknown quirk"), "{err}");
    assert!(out.stdout.is_empty());
}
