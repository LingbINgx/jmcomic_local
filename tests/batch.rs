use anyhow::{Result, bail};
use jmcomic_local::{
    Interrupted,
    batch::{self, Summary},
    config::Config,
    state::{self, BatchLock},
};
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

fn setup() -> (tempfile::TempDir, Config, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let cfg = Config {
        root: temp.path().join("images"),
        retry_delay: 0,
        ..Config::default()
    };
    let history = temp.path().join("history.json");
    (temp, cfg, history)
}
fn image(root: &Path, id: &str) -> Result<Vec<PathBuf>> {
    let path = root.join(id).join("1/00001.png");
    std::fs::create_dir_all(path.parent().unwrap())?;
    image::RgbImage::new(2, 2).save(&path)?;
    Ok(vec![path])
}
fn ids(input: &[&str]) -> Vec<String> {
    input.iter().map(|s| s.to_string()).collect()
}

#[test]
fn skips_complete_and_restores_missing_or_resized_files() -> Result<()> {
    let (_temp, cfg, history) = setup();
    let cancel = AtomicBool::new(false);
    let task = ids(&["1"]);
    let result = batch::run_batch(&task, &cfg, &history, &cancel, |id, root, _| {
        image(root, id)
    })?;
    assert_eq!(result.complete, 1);
    let result = batch::run_batch(&task, &cfg, &history, &cancel, |_, _, _| {
        panic!("should skip")
    })?;
    assert_eq!(result.skipped, 1);
    let path = cfg.root.join("1/1/00001.png");
    std::fs::remove_file(&path)?;
    assert_eq!(
        batch::run_batch(&task, &cfg, &history, &cancel, |id, root, _| image(
            root, id
        ))?
        .complete,
        1
    );
    std::fs::write(&path, b"short")?;
    assert_eq!(
        batch::run_batch(&task, &cfg, &history, &cancel, |id, root, _| image(
            root, id
        ))?
        .complete,
        1
    );
    Ok(())
}

#[test]
fn retries_and_continues_after_permanent_failure() -> Result<()> {
    let (_temp, cfg, history) = setup();
    let mut calls = Vec::new();
    let result = batch::run_batch(
        &ids(&["1", "2"]),
        &cfg,
        &history,
        &AtomicBool::new(false),
        |id, root, _| {
            calls.push(id.to_owned());
            if id == "1" {
                bail!("simulated partial failure");
            }
            image(root, id)
        },
    )?;
    assert_eq!(
        result,
        Summary {
            complete: 1,
            skipped: 0,
            failed: 1
        }
    );
    assert_eq!(calls, ["1", "1", "2"]);
    assert_eq!(result.exit_code(), 1);
    let state = state::read(&history)?;
    assert_eq!(state.albums["1"].status, "failed");
    assert_eq!(state.albums["2"].status, "complete");
    let mut count = 0;
    let result = batch::run_batch(
        &ids(&["1"]),
        &cfg,
        &history,
        &AtomicBool::new(false),
        |id, root, _| {
            count += 1;
            if count == 1 {
                bail!("transient failure");
            }
            image(root, id)
        },
    )?;
    assert_eq!(result.complete, 1);
    assert_eq!(state::read(&history)?.albums["1"].attempt, 2);
    Ok(())
}

#[test]
fn rejects_missing_empty_outside_and_empty_manifest() -> Result<()> {
    let (temp, cfg, history) = setup();
    std::fs::create_dir_all(&cfg.root)?;
    let outside = temp.path().join("outside.png");
    std::fs::write(&outside, b"outside")?;
    let empty = cfg.root.join("empty.png");
    std::fs::write(&empty, b"")?;
    for paths in [
        vec![],
        vec![outside],
        vec![empty],
        vec![cfg.root.join("missing.png")],
    ] {
        let result = batch::run_batch(
            &ids(&["1"]),
            &cfg,
            &history,
            &AtomicBool::new(false),
            |_, _, _| Ok(paths.clone()),
        )?;
        assert_eq!(result.failed, 1);
        assert_eq!(state::read(&history)?.albums["1"].status, "failed");
    }
    Ok(())
}

#[test]
fn records_interrupt_and_resumes_stale_running_record() -> Result<()> {
    let (_temp, cfg, history) = setup();
    let cancel = AtomicBool::new(false);
    let error = batch::run_batch(&ids(&["1"]), &cfg, &history, &cancel, |_, _, flag| {
        flag.store(true, Ordering::SeqCst);
        bail!("network aborted")
    })
    .unwrap_err();
    assert!(error.is::<Interrupted>());
    let mut saved = state::read(&history)?;
    assert_eq!(saved.albums["1"].status, "interrupted");
    saved.albums.get_mut("1").unwrap().status = "running".into();
    state::save(&history, &saved)?;
    cancel.store(false, Ordering::SeqCst);
    assert_eq!(
        batch::run_batch(&ids(&["1"]), &cfg, &history, &cancel, |id, root, _| image(
            root, id
        ))?
        .complete,
        1
    );
    Ok(())
}

#[test]
fn refuses_concurrent_batch_and_preserves_corrupt_history() -> Result<()> {
    let (_temp, cfg, history) = setup();
    let lock = history.with_extension("lock");
    let guard = BatchLock::acquire(&lock)?;
    assert!(BatchLock::acquire(&lock).is_err());
    assert!(
        batch::run_batch(
            &ids(&["1"]),
            &cfg,
            &history,
            &AtomicBool::new(false),
            |_, _, _| panic!("locked")
        )
        .is_err()
    );
    drop(guard);
    std::fs::write(&history, b"broken")?;
    assert!(
        batch::run_batch(
            &ids(&["1"]),
            &cfg,
            &history,
            &AtomicBool::new(false),
            |_, _, _| panic!("bad state")
        )
        .is_err()
    );
    assert_eq!(std::fs::read_to_string(&history)?, "broken");
    Ok(())
}

#[test]
fn changing_root_invalidates_completion() -> Result<()> {
    let (_temp, mut cfg, history) = setup();
    let cancel = AtomicBool::new(false);
    batch::run_batch(&ids(&["1"]), &cfg, &history, &cancel, |id, root, _| {
        image(root, id)
    })?;
    cfg.root = cfg.root.with_file_name("new-images");
    assert_eq!(
        batch::run_batch(&ids(&["1"]), &cfg, &history, &cancel, |id, root, _| image(
            root, id
        ))?
        .complete,
        1
    );
    Ok(())
}

#[test]
fn reads_python_history_and_rejects_escaping_manifest() -> Result<()> {
    let (_temp, cfg, history) = setup();
    std::fs::write(
        &history,
        r#"{"version":1,"albums":{"1":{"status":"interrupted","root":"/tmp/images","files":{},"attempt":1,"updated_at":"2026-10-17T15:00:00+08:00"}}}"#,
    )?;
    assert_eq!(state::read(&history)?.albums["1"].status, "interrupted");
    let record = state::Record {
        status: "complete".into(),
        root: cfg.root.to_string_lossy().into_owned(),
        files: [("../outside.png".into(), 1)].into(),
        ..Default::default()
    };
    assert!(!state::verified_complete(&record, &cfg.root));
    Ok(())
}
