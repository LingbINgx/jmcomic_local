use anyhow::{Context, Result};
use clap::Parser;
use jmcomic_local::{
    Interrupted, batch,
    client::Client,
    config::{Config, absolute},
    parse_ids, state,
};
use std::{
    io::{self, IsTerminal, Read, Write},
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
};

#[derive(Parser)]
#[command(version, about = "JMComic 本地批量下载器（原生 Rust，按顺序下载）")]
struct Args {
    /// 数字 ID 或 JM 加数字，可传多个
    ids: Vec<String>,
    /// 从 UTF-8 文本文件读取 ID
    #[arg(long)]
    file: Option<PathBuf>,
    /// 配置文件；默认从当前目录或可执行文件旁查找
    #[arg(long)]
    config: Option<PathBuf>,
    /// 下载记录路径；默认是配置文件目录下的 data/history.json
    #[arg(long)]
    state: Option<PathBuf>,
    /// 查看本地任务记录
    #[arg(long, conflicts_with_all = ["doctor", "dry_run"])]
    status: bool,
    /// 检查依赖和配置，不访问漫画网站
    #[arg(long, conflicts_with = "dry_run")]
    doctor: bool,
    /// 预览任务，不联网下载
    #[arg(long)]
    dry_run: bool,
}

fn default_config() -> Result<PathBuf> {
    let local = std::env::current_dir()?.join("config.toml");
    if local.is_file() {
        return Ok(local);
    }
    let executable = std::env::current_exe()?;
    for parent in executable.ancestors().skip(1).take(3) {
        let path = parent.join("config.toml");
        if path.is_file() {
            return Ok(path);
        }
    }
    anyhow::bail!("未找到 config.toml，请使用 --config 指定配置文件")
}

fn run(args: Args) -> Result<i32> {
    let config_path = absolute(&match args.config {
        Some(path) => path,
        None => default_config()?,
    })?
    .canonicalize()?;
    let base = config_path.parent().unwrap_or(Path::new("."));
    let cfg = Config::load(&config_path)?;
    let state_path = absolute(&args.state.unwrap_or_else(|| base.join("data/history.json")))?;
    if args.doctor {
        let _client = Client::new(&cfg)?;
        let records = state::read(&state_path)?;
        println!(
            "jmcomic_local {} / 原生 Rust（无需 Python）",
            env!("CARGO_PKG_VERSION")
        );
        println!("配置正常：{}", config_path.display());
        println!("下载位置：{}", cfg.root.display());
        println!(
            "下载记录：{}（{} 项）",
            state_path.display(),
            records.albums.len()
        );
        println!("本地检查通过；尚未验证漫画平台连接。");
        return Ok(0);
    }
    if args.status {
        let state = state::read(&state_path)?;
        if state.albums.is_empty() {
            println!("暂无下载记录。");
        }
        for (id, record) in state.albums {
            println!("JM{id}\t{}\t{}", record.status, record.updated_at);
        }
        return Ok(0);
    }
    let mut text = args.ids.join(" ");
    if let Some(path) = &args.file {
        text.push('\n');
        text.push_str(
            std::fs::read_to_string(path)
                .with_context(|| format!("无法读取清单 {}", path.display()))?
                .trim_start_matches('\u{feff}'),
        );
    }
    if text.trim().is_empty() && args.file.is_none() {
        if io::stdin().is_terminal() {
            println!("JMComic 本地批量下载\n多个 ID 用空格或逗号分隔；回车读取 queue.txt。");
            print!("请输入 ID：");
            io::stdout().flush()?;
            io::stdin().read_line(&mut text)?;
            if text.trim().is_empty() {
                text = std::fs::read_to_string(base.join("queue.txt"))
                    .context("无法读取 queue.txt")?;
            }
        } else {
            io::stdin().read_to_string(&mut text)?;
        }
    }
    let ids = parse_ids(&text)?;
    if ids.is_empty() {
        println!("没有待下载 ID。可重新启动输入，或编辑 queue.txt 后运行。");
        return Ok(0);
    }
    if args.dry_run {
        println!(
            "待下载 ID：{}",
            ids.iter()
                .map(|id| format!("JM{id}"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        println!("保存位置：{}", cfg.root.display());
        println!("仅预览，未联网下载。");
        return Ok(0);
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    ctrlc::set_handler(move || {
        signal.store(true, Ordering::SeqCst);
    })
    .context("无法安装中断处理器")?;
    let mut client = Client::new(&cfg)?;
    let summary = batch::run_batch(&ids, &cfg, &state_path, &cancel, |id, root, flag| {
        client.download_album(id, root, flag)
    })?;
    Ok(summary.exit_code())
}

fn main() {
    let code = match run(Args::parse()) {
        Ok(code) => code,
        Err(err) if err.is::<Interrupted>() => {
            eprintln!("\n任务已中断。重新运行同一批 ID 可继续补下缺失文件。");
            130
        }
        Err(err) => {
            eprintln!("无法运行：{err:#}");
            2
        }
    };
    std::process::exit(code);
}
