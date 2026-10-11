//! The native UI uses the installed binary and never needs a scripting runtime.
use std::{
    fs,
    io::Write,
    process::{Command, Stdio},
};

fn internal_dialog(request: &str) -> (std::process::Output, tempfile::TempDir) {
    let temp = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_uniloader"))
        .args([
            "--config",
            temp.path()
                .join("must-not-be-created/config.toml")
                .to_str()
                .unwrap(),
            "__dialog",
        ])
        .env("HOME", temp.path())
        .env("XDG_CONFIG_HOME", temp.path().join("config"))
        .env("XDG_DATA_HOME", temp.path().join("data"))
        // The binary must not search PATH for an interpreter or companion helper.
        .env("PATH", temp.path().join("empty-path"))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(request.as_bytes())
        .unwrap();
    (child.wait_with_output().unwrap(), temp)
}

#[test]
fn no_display_is_pending_without_config_or_external_runtime() {
    for request in [
        r#"{"mode":"destination","projects":[],"component":"part"}"#,
        r#"{"mode":"compare","incoming":"(footprint A)","candidates":[]}"#,
        r#"{"mode":"file","title":"model","paths":["/a.step","/b.step"]}"#,
    ] {
        let (output, temp) = internal_dialog(request);
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&output.stdout).unwrap(),
            serde_json::Value::Null
        );
        assert_eq!(
            fs::read_dir(temp.path()).unwrap().count(),
            0,
            "native helper must not load config or write state"
        );
    }
}

#[test]
fn malformed_internal_request_is_rejected_without_writes() {
    let (output, temp) = internal_dialog("{bad JSON");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invalid native dialog request"));
    assert_eq!(fs::read_dir(temp.path()).unwrap().count(), 0);
}

#[test]
fn internal_window_command_is_not_a_user_command() {
    let output = Command::new(env!("CARGO_BIN_EXE_uniloader"))
        .arg("--help")
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(!String::from_utf8_lossy(&output.stdout).contains("__dialog"));
}
