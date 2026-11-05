use anyhow::{Context, Result, bail};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Component, Path, PathBuf},
};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Record {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub root: String,
    #[serde(default)]
    pub files: BTreeMap<String, u64>,
    #[serde(default)]
    pub attempt: u64,
    #[serde(default)]
    pub updated_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct State {
    pub version: u32,
    pub albums: BTreeMap<String, Record>,
}
impl Default for State {
    fn default() -> Self {
        Self {
            version: 1,
            albums: BTreeMap::new(),
        }
    }
}

pub fn read(path: &Path) -> Result<State> {
    let data = match fs::read(path) {
        Ok(data) => data,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(State::default()),
        Err(e) => return Err(e.into()),
    };
    let state: State = serde_json::from_slice(&data)
        .context("下载记录格式异常，请保留 history.json 并检查文件")?;
    if state.version != 1 {
        bail!("下载记录版本不受支持：{}", state.version);
    }
    Ok(state)
}

pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(data)?;
    temp.as_file().sync_all()?;
    temp.persist(path).map_err(|e| e.error)?;
    Ok(())
}

pub fn save(path: &Path, state: &State) -> Result<()> {
    let mut data = serde_json::to_vec_pretty(state)?;
    data.push(b'\n');
    atomic_write(path, &data)
}

pub struct BatchLock {
    _file: File,
}
impl BatchLock {
    pub fn acquire(path: &Path) -> Result<Self> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        file.try_lock_exclusive()
            .context("已有下载任务运行中，请等待它完成后再启动")?;
        Ok(Self { _file: file })
    }
}

pub fn safe_relative(name: &Path) -> bool {
    !name.as_os_str().is_empty() && name.components().all(|p| matches!(p, Component::Normal(_)))
}

pub fn verified_complete(record: &Record, root: &Path) -> bool {
    record.status == "complete"
        && Path::new(&record.root) == root
        && !record.files.is_empty()
        && record.files.iter().all(|(name, size)| {
            if *size == 0 || !safe_relative(Path::new(name)) {
                return false;
            }
            let Ok(target) = root.join(name).canonicalize() else {
                return false;
            };
            target.starts_with(root)
                && target.is_file()
                && target.metadata().is_ok_and(|m| m.len() == *size)
        })
}

pub fn manifest(root: &Path, paths: Vec<PathBuf>) -> Result<BTreeMap<String, u64>> {
    if paths.is_empty() {
        bail!("下载器未返回任何图片文件");
    }
    let mut files = BTreeMap::new();
    for path in paths {
        let path = path.canonicalize().context("下载结果存在缺失文件")?;
        let relative = path
            .strip_prefix(root)
            .context("下载器返回了保存目录以外的文件")?;
        let meta = path.metadata()?;
        if !meta.is_file() || meta.len() == 0 {
            bail!("下载结果存在缺失或空文件，未标记为完成");
        }
        files.insert(relative.to_string_lossy().into_owned(), meta.len());
    }
    Ok(files)
}
