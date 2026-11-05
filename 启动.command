#!/bin/zsh
cd -- "${0:A:h}" || exit 1
if [[ ! -x target/release/jmcomic_local ]]; then
  print "缺少可执行文件，请先运行 构建.command。"
  read -r "?按回车关闭……"
  exit 1
fi
target/release/jmcomic_local --config "$PWD/config.toml" "$@"
task_status=$?
read -r "?按回车关闭……"
exit "$task_status"
