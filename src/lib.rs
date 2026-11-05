pub mod batch;
pub mod client;
pub mod config;
pub mod images;
pub mod state;

use anyhow::{Result, bail};
use std::{
    collections::HashSet,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

pub fn parse_ids(text: &str) -> Result<Vec<String>> {
    let mut seen = HashSet::new();
    let mut ids = Vec::new();
    for line in text.trim_start_matches('\u{feff}').lines() {
        for token in line
            .split('#')
            .next()
            .unwrap_or("")
            .split(|c: char| c.is_whitespace() || matches!(c, ',' | '，' | ';' | '；' | '、'))
            .filter(|s| !s.is_empty())
        {
            let digits = if token.get(..2).is_some_and(|s| s.eq_ignore_ascii_case("jm")) {
                &token[2..]
            } else {
                token
            };
            if digits.is_empty() || digits.len() > 20 || !digits.bytes().all(|b| b.is_ascii_digit())
            {
                bail!("无效 ID：{token:?}。请输入正整数或 JM 加数字。");
            }
            let id = digits.trim_start_matches('0');
            if id.is_empty() {
                bail!("无效 ID：{token:?}。ID 必须大于零。");
            }
            if seen.insert(id.to_owned()) {
                ids.push(id.to_owned());
            }
        }
    }
    Ok(ids)
}

#[derive(Debug)]
pub struct Interrupted;
impl std::fmt::Display for Interrupted {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "任务已中断")
    }
}
impl std::error::Error for Interrupted {}

pub fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::SeqCst) {
        return Err(Interrupted.into());
    }
    Ok(())
}

pub fn wait_cancel(seconds: u64, cancel: &AtomicBool) -> Result<()> {
    for _ in 0..seconds * 10 {
        check_cancel(cancel)?;
        std::thread::sleep(Duration::from_millis(100));
    }
    check_cancel(cancel)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn parses_ids_without_integer_overflow() {
        assert_eq!(
            parse_ids("\u{feff}JM0001，1\n2 # note\nJM3；2、99999999999999999999").unwrap(),
            ["1", "2", "3", "99999999999999999999"]
        );
        for input in [
            "0",
            "-1",
            "1.2",
            "123oops",
            "JM",
            "１２３",
            "123456789012345678901",
        ] {
            assert!(parse_ids(input).is_err(), "{input}");
        }
    }
}
