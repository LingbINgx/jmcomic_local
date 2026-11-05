use anyhow::{Context, Result, bail};
use serde::Deserialize;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub download_dir: PathBuf,
    pub attempts: u64,
    pub retry_delay: u64,
    pub request_retries: u64,
    pub timeout: u64,
    pub image_threads: usize,
    pub api_domains: Vec<String>,
    pub image_domains: Vec<String>,
    pub domain_servers: Vec<String>,
    pub auto_update_domains: bool,
    pub app_version: String,
    #[serde(skip)]
    pub root: PathBuf,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            download_dir: "downloads".into(),
            attempts: 2,
            retry_delay: 5,
            request_retries: 2,
            timeout: 30,
            image_threads: 4,
            api_domains: [
                "www.cdnhjk.net",
                "www.cdngwc.cc",
                "www.cdngwc.net",
                "www.cdngwc.club",
            ]
            .map(str::to_owned)
            .into(),
            image_domains: [
                "cdn-msp.jmapiproxy1.cc",
                "cdn-msp.jmapiproxy2.cc",
                "cdn-msp2.jmapiproxy2.cc",
                "cdn-msp3.jmapiproxy2.cc",
                "cdn-msp.jmapinodeudzn.net",
                "cdn-msp3.jmapinodeudzn.net",
            ]
            .map(str::to_owned)
            .into(),
            domain_servers: [
                "https://rup4a04-c01.tos-ap-southeast-1.bytepluses.com/newsvr-2025.txt",
                "https://rup4a04-c02.tos-cn-hongkong.bytepluses.com/newsvr-2025.txt",
                "https://rup4a04-c03.tos-cn-beijing.bytepluses.com.cn/newsvr-2025.txt",
            ]
            .map(str::to_owned)
            .into(),
            auto_update_domains: true,
            app_version: "2.1.7".into(),
            root: PathBuf::new(),
        }
    }
}

pub fn absolute(path: &Path) -> Result<PathBuf> {
    let path = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut result = PathBuf::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                result.pop();
            }
            Component::CurDir => {}
            other => result.push(other.as_os_str()),
        }
    }
    Ok(result)
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("无法读取配置 {}", path.display()))?;
        let mut cfg: Self = toml::from_str(&text).context("config.toml 格式错误")?;
        cfg.validate()?;
        let mut root = cfg.download_dir.clone();
        if root == Path::new("~") || root.starts_with("~/") {
            let home = std::env::var_os("HOME").context("无法展开 ~：HOME 未设置")?;
            root = PathBuf::from(home).join(root.strip_prefix("~")?);
        }
        cfg.root = absolute(&path.parent().unwrap_or(Path::new(".")).join(root))?;
        // Existing symlink roots must have a stable canonical identity in history.
        if cfg.root.exists() {
            cfg.root = cfg.root.canonicalize()?;
        }
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<()> {
        for (name, value, low, high) in [
            ("attempts", self.attempts, 1, 5),
            ("retry_delay", self.retry_delay, 0, 300),
            ("request_retries", self.request_retries, 0, 5),
            ("timeout", self.timeout, 5, 300),
            ("image_threads", self.image_threads as u64, 1, 8),
        ] {
            if !(low..=high).contains(&value) {
                bail!("config.toml 中 {name} 必须是 {low} 至 {high} 的整数。");
            }
        }
        if self.download_dir.as_os_str().is_empty() {
            bail!("download_dir 不能为空");
        }
        if self.api_domains.is_empty() || self.image_domains.is_empty() {
            bail!("API 和图片域名列表不能为空");
        }
        for domain in self
            .api_domains
            .iter()
            .chain(&self.image_domains)
            .chain(&self.domain_servers)
        {
            base_url(domain)?;
        }
        if self.app_version.is_empty()
            || !self
                .app_version
                .bytes()
                .all(|b| b.is_ascii_digit() || b == b'.')
        {
            bail!("app_version 必须是数字和点组成的版本号");
        }
        Ok(())
    }
}

pub fn base_url(domain: &str) -> Result<reqwest::Url> {
    let text = if domain.contains("://") {
        domain.to_owned()
    } else {
        format!("https://{domain}")
    };
    let url = reqwest::Url::parse(&text).context("域名/URL 无效")?;
    if !matches!(url.scheme(), "https" | "http")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
    {
        bail!("仅支持无身份信息的 HTTP(S) URL：{domain}");
    }
    Ok(url)
}
