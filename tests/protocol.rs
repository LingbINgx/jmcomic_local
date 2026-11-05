//! All IDs and images here are synthetic; traffic stays on the loopback interface.
use aes::{
    Aes256,
    cipher::{BlockEncrypt, KeyInit, generic_array::GenericArray},
};
use anyhow::Result;
use base64::{Engine, engine::general_purpose::STANDARD};
use jmcomic_local::{
    batch,
    client::{APP_SECRET, CONTENT_SECRET, Client, DOMAIN_SECRET, decrypt},
    config::Config,
    state,
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    io::{Cursor, Read, Write},
    net::{TcpListener, TcpStream},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

fn encrypt(value: &Value, ts: &str, secret: &str) -> String {
    let mut bytes = serde_json::to_vec(value).unwrap();
    let padding = 16 - bytes.len() % 16;
    bytes.extend(std::iter::repeat_n(padding as u8, padding));
    let key = format!("{:x}", md5::compute(format!("{ts}{secret}")));
    let cipher = Aes256::new_from_slice(key.as_bytes()).unwrap();
    for block in bytes.as_chunks_mut::<16>().0 {
        cipher.encrypt_block(GenericArray::from_mut_slice(block));
    }
    STANDARD.encode(bytes)
}

#[test]
fn decrypts_fixture_from_original_python_implementation() {
    assert_eq!(
        decrypt(
            "1rFYokqcUimA1uq3/1Oafhsy+kmva9/0D9YkMs7hQes=",
            "1700000000",
            APP_SECRET
        )
        .unwrap(),
        br#"{"id":1,"name":"fixture"}"#
    );
    for input in ["", "garbage", "AAAAAAAAAAAAAAAAAAAAAA=="] {
        assert!(decrypt(input, "1700000000", APP_SECRET).is_err());
    }
    for (id, expected) in [
        (220979, 0),
        (220980, 10),
        (268850, 6),
        (421925, 18),
        (421926, 14),
        (999999, 12),
    ] {
        assert_eq!(
            jmcomic_local::images::segments(220980, id, "00001"),
            expected
        );
    }
}

struct Server {
    url: String,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    image_calls: Arc<AtomicUsize>,
    bad_scramble: Arc<AtomicBool>,
    bad_name: Arc<AtomicBool>,
    bad_id: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<String>>>,
}

impl Server {
    fn new() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        listener.set_nonblocking(true).unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let image_calls = Arc::new(AtomicUsize::new(0));
        let bad_scramble = Arc::new(AtomicBool::new(false));
        let bad_name = Arc::new(AtomicBool::new(false));
        let bad_id = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let mut server = Self {
            url: url.clone(),
            stop: stop.clone(),
            thread: None,
            image_calls: image_calls.clone(),
            bad_scramble: bad_scramble.clone(),
            bad_name: bad_name.clone(),
            bad_id: bad_id.clone(),
            requests: requests.clone(),
        };
        let handle = thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                let (mut stream, _) = match listener.accept() {
                    Ok(pair) => pair,
                    Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(5));
                        continue;
                    }
                    Err(err) => panic!("{err}"),
                };
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(Duration::from_secs(3)))
                    .unwrap();
                let (path, headers) = read_request(&mut stream);
                requests.lock().unwrap().push(path.clone());
                let parsed = reqwest::Url::parse(&format!("{url}{path}")).unwrap();
                let query: HashMap<_, _> = parsed.query_pairs().into_owned().collect();
                let route = parsed.path();
                if route == "/newsvr" {
                    let body = encrypt(&json!({"Server": [url]}), "", DOMAIN_SECRET);
                    respond(&mut stream, 200, body.as_bytes(), false);
                } else if route.starts_with("/media/") {
                    assert_eq!(headers["x-requested-with"], "com.JMComic3.app");
                    if image_calls.fetch_add(1, Ordering::SeqCst) == 0 {
                        respond(&mut stream, 503, b"transient failure", false);
                    } else {
                        respond(&mut stream, 200, &scrambled_png(), false);
                    }
                } else {
                    let ts = headers["tokenparam"].split(',').next().unwrap();
                    let secret = if route == "/chapter_view_template" {
                        CONTENT_SECRET
                    } else {
                        APP_SECRET
                    };
                    assert_eq!(
                        headers["token"],
                        format!("{:x}", md5::compute(format!("{ts}{secret}")))
                    );
                    if route == "/chapter_view_template" {
                        assert_eq!(query["app_img_shunt"], "1");
                        let text = if bad_scramble.load(Ordering::SeqCst) {
                            "unexpected HTML"
                        } else {
                            "<script>var scramble_id = 1;</script>"
                        };
                        respond(&mut stream, 200, text.as_bytes(), false);
                        continue;
                    }
                    if route != "/setting" {
                        assert!(
                            headers
                                .get("cookie")
                                .is_some_and(|s| s.contains("session=fixture"))
                        );
                    }
                    let value = match route {
                        "/setting" => json!({"jm3_version": "2.1.7"}),
                        "/album" => {
                            json!({"id": if bad_id.load(Ordering::SeqCst) { 999 } else { 1 }, "name": "fixture", "series": [{"id":"3", "sort":"2"}, {"id":"2", "sort":"1"}]})
                        }
                        "/chapter" => {
                            json!({"id": query["id"].parse::<u64>().unwrap(), "name": "fixture chapter", "images": if bad_name.load(Ordering::SeqCst) { vec!["../escape.png"] } else { vec!["00001.png", "00002.png"] }})
                        }
                        _ => panic!("unexpected route {route}"),
                    };
                    let body = serde_json::to_vec(
                        &json!({"code":200, "data": encrypt(&value, ts, APP_SECRET)}),
                    )
                    .unwrap();
                    respond(&mut stream, 200, &body, route == "/setting");
                }
            }
        });
        server.thread = Some(handle);
        server
    }

    fn config(&self, root: &Path) -> Config {
        Config {
            root: root.to_path_buf(),
            api_domains: vec!["http://127.0.0.1:1".into()],
            image_domains: vec![self.url.clone()],
            domain_servers: vec![format!("{}/newsvr", self.url)],
            auto_update_domains: true,
            attempts: 1,
            retry_delay: 0,
            request_retries: 1,
            timeout: 5,
            ..Config::default()
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.thread.take() {
            handle.join().unwrap();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> (String, HashMap<String, String>) {
    let mut bytes = Vec::new();
    while !bytes.ends_with(b"\r\n\r\n") {
        let mut byte = [0];
        stream.read_exact(&mut byte).unwrap();
        bytes.push(byte[0]);
        assert!(bytes.len() < 16384);
    }
    let request = String::from_utf8(bytes).unwrap();
    let mut lines = request.lines();
    let path = lines
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .to_owned();
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect();
    (path, headers)
}
fn respond(stream: &mut TcpStream, code: u16, body: &[u8], cookie: bool) {
    write!(
        stream,
        "HTTP/1.1 {code} OK\r\nContent-Length: {}\r\nConnection: close\r\n{}\r\n",
        body.len(),
        if cookie {
            "Set-Cookie: session=fixture; Path=/\r\n"
        } else {
            ""
        }
    )
    .unwrap();
    stream.write_all(body).unwrap();
}
fn scrambled_png() -> Vec<u8> {
    // Inverse strip transform, independently generated for a 23-row image / 10 parts.
    let mut source = image::RgbImage::new(2, 23);
    let order = [
        21, 22, 19, 20, 17, 18, 15, 16, 13, 14, 11, 12, 9, 10, 7, 8, 5, 6, 0, 1, 2, 3, 4,
    ];
    for (y, value) in order.into_iter().enumerate() {
        for x in 0..2 {
            source.put_pixel(x, y as u32, image::Rgb([value, 0, 0]));
        }
    }
    let mut output = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(source)
        .write_to(&mut output, image::ImageFormat::Png)
        .unwrap();
    output.into_inner()
}

#[test]
fn native_protocol_download_retry_restore_cache_and_repair() -> Result<()> {
    let server = Server::new();
    let temp = tempfile::tempdir()?;
    let cfg = server.config(&temp.path().join("images"));
    let history = temp.path().join("history.json");
    let cancel = AtomicBool::new(false);
    let mut client = Client::new(&cfg)?;
    let tasks = vec!["1".to_owned()];
    let result = batch::run_batch(&tasks, &cfg, &history, &cancel, |id, root, flag| {
        client.download_album(id, root, flag)
    })?;
    assert_eq!(result.complete, 1);
    assert_eq!(server.image_calls.load(Ordering::SeqCst), 5);
    let saved = state::read(&history)?;
    assert_eq!(saved.albums["1"].files.len(), 4);
    for name in saved.albums["1"].files.keys() {
        let image = image::open(cfg.root.join(name))?.to_rgb8();
        assert_eq!(
            (0..23)
                .map(|y| image.get_pixel(0, y)[0])
                .collect::<Vec<_>>(),
            (0..23).collect::<Vec<u8>>()
        );
    }
    let result = batch::run_batch(&tasks, &cfg, &history, &cancel, |_, _, _| {
        panic!("complete task fetched")
    })?;
    assert_eq!(result.skipped, 1);
    std::fs::remove_file(cfg.root.join("1/1/00001.png"))?;
    std::fs::write(cfg.root.join("1/1/00002.png"), b"truncated image")?;
    assert_eq!(
        batch::run_batch(&tasks, &cfg, &history, &cancel, |id, root, flag| client
            .download_album(id, root, flag))?
        .complete,
        1
    );
    assert_eq!(server.image_calls.load(Ordering::SeqCst), 7);
    let requests = server.requests.lock().unwrap();
    let chapters: Vec<_> = requests
        .iter()
        .filter(|s| s.starts_with("/chapter?"))
        .collect();
    assert!(chapters[0].contains("id=2"));
    assert!(chapters[1].contains("id=3"));
    Ok(())
}

#[test]
fn missing_scramble_unsafe_filename_and_wrong_id_never_complete() -> Result<()> {
    for mode in 0..3 {
        let server = Server::new();
        match mode {
            0 => server.bad_scramble.store(true, Ordering::SeqCst),
            1 => server.bad_name.store(true, Ordering::SeqCst),
            _ => server.bad_id.store(true, Ordering::SeqCst),
        }
        let temp = tempfile::tempdir()?;
        let cfg = server.config(&temp.path().join("images"));
        let history = temp.path().join("history.json");
        let mut client = Client::new(&cfg)?;
        let result = batch::run_batch(
            &["1".into()],
            &cfg,
            &history,
            &AtomicBool::new(false),
            |id, root, flag| client.download_album(id, root, flag),
        )?;
        assert_eq!(result.failed, 1);
        assert_eq!(state::read(&history)?.albums["1"].status, "failed");
        assert_eq!(server.image_calls.load(Ordering::SeqCst), 0);
        assert!(!temp.path().join("escape.png").exists());
    }
    Ok(())
}
