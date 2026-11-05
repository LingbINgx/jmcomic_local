use crate::{
    Interrupted, check_cancel,
    config::Config,
    state::{self, BatchLock, Record},
    wait_cancel,
};
use anyhow::Result;
use chrono::Local;
use std::{
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Summary {
    pub complete: usize,
    pub skipped: usize,
    pub failed: usize,
}
impl Summary {
    pub fn exit_code(&self) -> i32 {
        i32::from(self.failed > 0)
    }
}

fn now() -> String {
    Local::now().to_rfc3339()
}

pub fn run_batch(
    ids: &[String],
    cfg: &Config,
    state_path: &Path,
    cancel: &AtomicBool,
    mut fetch: impl FnMut(&str, &Path, &AtomicBool) -> Result<Vec<PathBuf>>,
) -> Result<Summary> {
    std::fs::create_dir_all(&cfg.root)?;
    let root = cfg.root.canonicalize()?;
    let _lock = BatchLock::acquire(&state_path.with_extension("lock"))?;
    let _root_lock = BatchLock::acquire(&root.join(".jmcomic.lock"))?;
    let mut state = state::read(state_path)?;
    let mut counts = Summary::default();
    for (index, aid) in ids.iter().enumerate() {
        check_cancel(cancel)?;
        if state
            .albums
            .get(aid)
            .is_some_and(|old| state::verified_complete(old, &root))
        {
            println!("[{}/{}] JM{aid}：文件完整，跳过。", index + 1, ids.len());
            counts.skipped += 1;
            continue;
        }
        state.albums.insert(
            aid.clone(),
            Record {
                status: "running".into(),
                root: root.to_string_lossy().into_owned(),
                ..Record::default()
            },
        );
        for attempt in 1..=cfg.attempts {
            let record = state.albums.get_mut(aid).unwrap();
            record.status = "running".into();
            record.attempt = attempt;
            record.updated_at = now();
            state::save(state_path, &state)?;
            println!(
                "[{}/{}] JM{aid}：开始下载（{attempt}/{}）。",
                index + 1,
                ids.len(),
                cfg.attempts
            );
            let result = check_cancel(cancel)
                .and_then(|_| fetch(aid, &root, cancel))
                .and_then(|paths| {
                    check_cancel(cancel)?;
                    state::manifest(&root, paths)
                });
            let record = state.albums.get_mut(aid).unwrap();
            record.updated_at = now();
            match result {
                Ok(files) => {
                    println!("JM{aid}：完成，共 {} 张图片。", files.len());
                    record.status = "complete".into();
                    record.files = files;
                    record.error = None;
                    counts.complete += 1;
                    state::save(state_path, &state)?;
                    break;
                }
                Err(err) => {
                    if err.is::<Interrupted>() || cancel.load(Ordering::SeqCst) {
                        record.status = "interrupted".into();
                        record.error = None;
                        state::save(state_path, &state)?;
                        return Err(Interrupted.into());
                    }
                    record.status = "failed".into();
                    record.error = Some(format!("{err:#}"));
                    eprintln!("JM{aid}：失败，{err:#}");
                    state::save(state_path, &state)?;
                    if attempt == cfg.attempts {
                        counts.failed += 1;
                    } else {
                        println!("等待 {} 秒后重试……", cfg.retry_delay);
                        if let Err(err) = wait_cancel(cfg.retry_delay, cancel) {
                            let record = state.albums.get_mut(aid).unwrap();
                            record.status = "interrupted".into();
                            record.updated_at = now();
                            state::save(state_path, &state)?;
                            return Err(err);
                        }
                    }
                }
            }
        }
    }
    println!(
        "\n本次结果：完成 {}，跳过 {}，失败 {}。",
        counts.complete, counts.skipped, counts.failed
    );
    println!("保存位置：{}", root.display());
    if counts.failed > 0 {
        println!("重新运行同一批 ID 即可重试；完整任务会跳过。");
    }
    Ok(counts)
}
