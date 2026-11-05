//! Native implementation of the mobile protocol used by jmcomic 2.7.9.
//! Protocol constants and strip algorithm: hect0x7/JMComic-Crawler-Python (MIT).
use crate::{
    check_cancel,
    config::{Config, base_url},
    images, parse_ids, state,
};
use aes::{
    Aes256,
    cipher::{BlockDecrypt, KeyInit, generic_array::GenericArray},
};
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine, engine::general_purpose::STANDARD};
use rayon::prelude::*;
use reqwest::blocking::{Client as HttpClient, Response};
use serde_json::Value;
use std::{
    collections::HashSet,
    io::Read,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const APP_SECRET: &str = "185Hcomic3PAPP7R";
pub const CONTENT_SECRET: &str = "18comicAPPContent";
pub const DOMAIN_SECRET: &str = "diosfjckwpqpdfjkvnqQjsik";
const USER_AGENT: &str = "Mozilla/5.0 (Linux; Android 9; V1938CT Build/PQ3A.190705.11211812; wv) AppleWebKit/537.36 (KHTML, like Gecko) Version/4.0 Chrome/91.0.4472.114 Safari/537.36";

pub fn decrypt(data: &str, timestamp: &str, secret: &str) -> Result<Vec<u8>> {
    let key = format!("{:x}", md5::compute(format!("{timestamp}{secret}")));
    let cipher = Aes256::new_from_slice(key.as_bytes()).map_err(|_| anyhow!("AES 密钥无效"))?;
    let mut bytes = STANDARD.decode(data.trim()).context("响应 Base64 无效")?;
    if bytes.is_empty() || !bytes.len().is_multiple_of(16) {
        bail!("加密响应长度无效");
    }
    for block in bytes.as_chunks_mut::<16>().0 {
        cipher.decrypt_block(GenericArray::from_mut_slice(block));
    }
    let padding = usize::from(*bytes.last().unwrap());
    if !(1..=16).contains(&padding)
        || !bytes[bytes.len() - padding..]
            .iter()
            .all(|b| usize::from(*b) == padding)
    {
        bail!("响应解密失败（填充无效）");
    }
    bytes.truncate(bytes.len() - padding);
    Ok(bytes)
}

fn timestamp() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn limited(response: Response, max: u64) -> Result<Vec<u8>> {
    let response = response.error_for_status()?;
    let mut bytes = Vec::new();
    response.take(max + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > max {
        bail!("服务器响应过大，超过 {max} 字节");
    }
    Ok(bytes)
}

pub struct Client {
    http: HttpClient,
    cfg: Config,
    initialized: bool,
    pool: rayon::ThreadPool,
}

impl Client {
    /// Constructs the HTTP client without making any network requests.
    pub fn new(cfg: &Config) -> Result<Self> {
        Ok(Self {
            http: HttpClient::builder()
                .user_agent(USER_AGENT)
                .cookie_store(true)
                .timeout(Duration::from_secs(cfg.timeout))
                .connect_timeout(Duration::from_secs(cfg.timeout))
                .gzip(true)
                .deflate(true)
                .build()?,
            cfg: cfg.clone(),
            initialized: false,
            pool: rayon::ThreadPoolBuilder::new()
                .num_threads(cfg.image_threads)
                .build()?,
        })
    }

    fn request<T>(
        &self,
        domains: &[String],
        path: &str,
        query: &[(&str, &str)],
        secret: Option<&str>,
        cancel: &AtomicBool,
        parse: impl Fn(Vec<u8>, &str) -> Result<T>,
    ) -> Result<T> {
        let mut last = anyhow!("没有可用域名");
        for domain in domains {
            for _ in 0..=self.cfg.request_retries {
                check_cancel(cancel)?;
                let result = (|| {
                    let mut url = base_url(domain)?;
                    if !path.is_empty() {
                        url = url.join(path)?;
                    }
                    url.query_pairs_mut().extend_pairs(query.iter().copied());
                    let ts = timestamp();
                    let mut req = self.http.get(url.clone());
                    if let Some(secret) = secret {
                        let token = format!("{:x}", md5::compute(format!("{ts}{secret}")));
                        req = req
                            .header("token", token)
                            .header("tokenparam", format!("{ts},{}", self.cfg.app_version));
                    } else if !path.is_empty() {
                        req = req
                            .header(
                                "Accept",
                                "image/avif,image/webp,image/apng,image/*,*/*;q=0.8",
                            )
                            .header("X-Requested-With", "com.JMComic3.app")
                            .header("Referer", base_url(&self.cfg.api_domains[0])?.as_str());
                    }
                    let max = if secret.is_some() || path.is_empty() {
                        16 * 1024 * 1024
                    } else {
                        128 * 1024 * 1024
                    };
                    let body =
                        limited(req.send().with_context(|| format!("请求 {url} 失败"))?, max)?;
                    check_cancel(cancel)?;
                    parse(body, &ts)
                })();
                match result {
                    Ok(value) => return Ok(value),
                    Err(err) => last = err,
                }
            }
        }
        check_cancel(cancel)?;
        Err(last)
    }

    fn api(&self, path: &str, query: &[(&str, &str)], cancel: &AtomicBool) -> Result<Value> {
        self.request(
            &self.cfg.api_domains,
            path,
            query,
            Some(APP_SECRET),
            cancel,
            |body, ts| {
                let envelope: Value = serde_json::from_slice(&body).context("接口未返回 JSON")?;
                if envelope["code"] != 200 {
                    bail!(
                        "接口返回错误：{}",
                        envelope["errorMsg"].as_str().unwrap_or("未知错误")
                    );
                }
                let encoded = envelope["data"].as_str().context("接口未返回加密数据")?;
                serde_json::from_slice(&decrypt(encoded, ts, APP_SECRET)?)
                    .context("解密后的 JSON 无效")
            },
        )
    }

    fn initialize(&mut self, cancel: &AtomicBool) -> Result<()> {
        if self.initialized {
            return Ok(());
        }
        if self.cfg.auto_update_domains {
            let domains = self.request(
                &self.cfg.domain_servers,
                "",
                &[],
                None,
                cancel,
                |body, _| {
                    let text =
                        std::str::from_utf8(&body)?.trim_start_matches(|c: char| !c.is_ascii());
                    let value: Value = serde_json::from_slice(&decrypt(text, "", DOMAIN_SECRET)?)?;
                    let domains = value["Server"]
                        .as_array()
                        .context("域名服务未返回 Server 列表")?;
                    let domains = domains
                        .iter()
                        .map(|v| {
                            let domain = v.as_str().context("域名不是字符串")?;
                            base_url(domain)?;
                            Ok(domain.to_owned())
                        })
                        .collect::<Result<Vec<_>>>()?;
                    if domains.is_empty() {
                        bail!("域名服务返回空列表");
                    }
                    Ok(domains)
                },
            );
            match domains {
                Ok(domains) => self.cfg.api_domains = domains,
                Err(err) => {
                    check_cancel(cancel)?;
                    eprintln!("域名自动更新失败，使用配置域名：{err:#}");
                }
            }
        }
        // Establish real session cookies before requesting any album.
        let settings = self
            .api("/setting", &[], cancel)
            .context("初始化接口会话失败")?;
        if let Some(version) = settings["jm3_version"].as_str() {
            let numeric = |v: &str| {
                v.split('.')
                    .map(str::parse::<u64>)
                    .collect::<std::result::Result<Vec<_>, _>>()
            };
            if let (Ok(new), Ok(old)) = (numeric(version), numeric(&self.cfg.app_version))
                && new > old
            {
                self.cfg.app_version = version.to_owned();
            }
        }
        self.initialized = true;
        Ok(())
    }

    fn scramble(&self, id: &str, cancel: &AtomicBool) -> Result<u128> {
        let ts = timestamp();
        self.request(
            &self.cfg.api_domains,
            "/chapter_view_template",
            &[
                ("id", id),
                ("mode", "vertical"),
                ("page", "0"),
                ("app_img_shunt", "1"),
                ("express", "off"),
                ("v", &ts),
            ],
            Some(CONTENT_SECRET),
            cancel,
            |body, _| {
                let text = std::str::from_utf8(&body)?;
                // Refuse to mark still-scrambled images complete on unexpected HTML.
                for tail in text.split("scramble_id").skip(1) {
                    let tail = tail.trim_start();
                    if let Some(tail) = tail.strip_prefix('=') {
                        let digits: String = tail
                            .trim_start()
                            .chars()
                            .take_while(char::is_ascii_digit)
                            .collect();
                        if !digits.is_empty() {
                            return Ok(digits.parse()?);
                        }
                    }
                }
                bail!("页面缺少 scramble_id，无法确认图片还原参数")
            },
        )
    }

    pub fn download_album(
        &mut self,
        aid: &str,
        root: &Path,
        cancel: &AtomicBool,
    ) -> Result<Vec<PathBuf>> {
        if parse_ids(aid)? != [aid] || !aid.bytes().all(|b| b.is_ascii_digit()) {
            bail!("下载 ID 未规范化");
        }
        self.initialize(cancel)?;
        let album = self.api("/album", &[("id", aid)], cancel)?;
        verify_id(&album, aid)?;
        let mut chapters = chapters(&album, aid)?;
        chapters.sort_by_key(|(_, sort)| *sort);
        let mut paths = Vec::new();
        let mut scramble = None;
        for (pid, index) in chapters {
            check_cancel(cancel)?;
            let chapter = self.api("/chapter", &[("id", &pid)], cancel)?;
            verify_id(&chapter, &pid)?;
            let names = chapter["images"].as_array().context("章节缺少图片列表")?;
            if names.is_empty() {
                bail!("章节 {pid} 未返回任何图片");
            }
            let scramble_id = match scramble {
                Some(value) => value,
                None => {
                    let value = self.scramble(&pid, cancel)?;
                    scramble = Some(value);
                    value
                }
            };
            let dir = image_dir(root, aid, index)?;
            let names: Vec<&str> = names
                .iter()
                .map(|name| {
                    let name = name.as_str().context("图片文件名不是字符串")?;
                    validate_filename(name)?;
                    Ok(name)
                })
                .collect::<Result<_>>()?;
            let mut seen = HashSet::new();
            if names.iter().any(|name| !seen.insert(*name)) {
                bail!("章节返回重复图片文件名");
            }
            println!("  章节 {index}：{} 张图片", names.len());
            let downloaded: Result<Vec<PathBuf>> = self.pool.install(|| {
                names
                    .par_iter()
                    .map(|name| {
                        check_cancel(cancel)?;
                        let path = dir.join(name);
                        // Never follow a cache symlink, even when it points inside the root.
                        match path.symlink_metadata() {
                            Ok(meta) if meta.file_type().is_symlink() => {
                                bail!("图片缓存不能是符号链接：{}", path.display())
                            }
                            Ok(meta) if meta.is_file() => {
                                if images::check(&path).is_ok() {
                                    return Ok(path);
                                }
                                std::fs::remove_file(&path)?;
                            }
                            Ok(_) => bail!("图片路径不是普通文件：{}", path.display()),
                            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
                            Err(err) => return Err(err.into()),
                        }
                        let stem = Path::new(name)
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .context("图片名无效")?;
                        let parts = images::segments(scramble_id, pid.parse()?, stem);
                        let ts = timestamp();
                        let bytes = self.request(
                            &self.cfg.image_domains,
                            &format!("/media/photos/{pid}/{name}"),
                            &[("v", &ts)],
                            None,
                            cancel,
                            |bytes, _| images::processed(&bytes, name, parts),
                        )?;
                        check_cancel(cancel)?;
                        state::atomic_write(&path, &bytes)?;
                        Ok(path)
                    })
                    .collect()
            });
            paths.extend(downloaded?);
        }
        Ok(paths)
    }
}

fn id_string(value: &Value) -> Result<String> {
    let raw = match value {
        Value::String(s) => s.clone(),
        Value::Number(n) => n.to_string(),
        _ => bail!("接口 ID 类型无效"),
    };
    let ids = parse_ids(&raw)?;
    if ids.len() != 1 || !raw.bytes().all(|b| b.is_ascii_digit()) {
        bail!("接口 ID 格式无效");
    }
    Ok(ids[0].clone())
}

fn verify_id(value: &Value, expected: &str) -> Result<()> {
    if id_string(&value["id"])? != expected || !value["name"].is_string() {
        bail!("接口返回的 ID/内容与请求不符：{expected}");
    }
    Ok(())
}

fn chapters(album: &Value, aid: &str) -> Result<Vec<(String, u128)>> {
    let series = album["series"].as_array().context("漫画缺少章节列表")?;
    if series.is_empty() {
        return Ok(vec![(aid.to_owned(), 1)]);
    }
    let mut result = Vec::new();
    let mut seen = HashSet::new();
    for chapter in series {
        let id = id_string(&chapter["id"])?;
        let sort: u128 = id_string(&chapter["sort"])?.parse()?;
        // The upstream implementation keeps the first chapter for each sort.
        if seen.insert(sort) {
            result.push((id, sort));
        }
    }
    Ok(result)
}

fn validate_filename(name: &str) -> Result<()> {
    if name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
        || !state::safe_relative(Path::new(name))
        || Path::new(name).components().count() != 1
    {
        bail!("不安全的图片文件名：{name:?}");
    }
    let format = image::ImageFormat::from_path(name).context("不支持的图片格式")?;
    if !matches!(
        format,
        image::ImageFormat::Jpeg
            | image::ImageFormat::Png
            | image::ImageFormat::WebP
            | image::ImageFormat::Gif
    ) {
        bail!("不支持的图片格式");
    }
    Ok(())
}

fn image_dir(root: &Path, aid: &str, index: u128) -> Result<PathBuf> {
    let mut dir = root.to_path_buf();
    for component in [aid.to_owned(), index.to_string()] {
        dir.push(component);
        match dir.symlink_metadata() {
            Ok(meta) if meta.file_type().is_symlink() || !meta.is_dir() => {
                bail!("下载子目录不能是符号链接或普通文件：{}", dir.display());
            }
            Ok(_) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(&dir)?,
            Err(err) => return Err(err.into()),
        }
    }
    Ok(dir)
}
