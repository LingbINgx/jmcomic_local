#!/bin/zsh
set -e
cd -- "${0:A:h}"
if ! command -v cargo >/dev/null 2>&1; then
  print "请先安装 Rust 工具链：https://rustup.rs/"
  read -r "?按回车关闭……"
  exit 1
fi
cargo build --release --locked
target/release/jmcomic_local --doctor
read -r "?构建完成，按回车关闭……"
