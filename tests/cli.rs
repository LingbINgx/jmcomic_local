use std::{path::Path, process::Command};

fn command(base: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_jmcomic_local"));
    command.arg("--config").arg(base.join("config.toml"));
    command
}

#[test]
fn cli_preview_doctor_status_and_errors_are_offline() {
    let temp = tempfile::tempdir().unwrap();
    let base = temp.path();
    std::fs::write(base.join("config.toml"), "download_dir = 'custom'\n").unwrap();
    std::fs::write(base.join("queue.txt"), "\u{feff}JM0001，1\n2 # note").unwrap();
    let output = command(base)
        .args(["--dry-run", "--file"])
        .arg(base.join("queue.txt"))
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("JM1, JM2"));
    assert!(text.contains(base.join("custom").to_str().unwrap()));
    assert!(!base.join("custom").exists());
    assert!(!base.join("data").exists());
    assert!(
        command(base)
            .arg("--doctor")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert!(
        command(base)
            .arg("--status")
            .output()
            .unwrap()
            .status
            .success()
    );
    assert_eq!(
        command(base)
            .args(["--dry-run", "JM0"])
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
    std::fs::write(base.join("config.toml"), "attempts = true").unwrap();
    assert_eq!(
        command(base)
            .arg("--doctor")
            .output()
            .unwrap()
            .status
            .code(),
        Some(2)
    );
}
