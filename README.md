# JMComic 本地批量下载器 · Rust

从原 Python 项目迁移的原生 Rust 命令行程序。运行时无需 Python、虚拟环境或 `jmcomic` 包。漫画依次下载，每次处理一个章节，章节内并发下载图片。

## 使用

在本目录双击 `启动.command`，输入自己的漫画 ID，多个 ID 用空格或逗号分隔。直接回车读取 `queue.txt`。图片默认保存在 `downloads/漫画ID/章节序号/原始文件名`。

首次构建或修改代码后，双击 `构建.command`，或执行：

```sh
cargo build --release --locked
./target/release/jmcomic_local --doctor
./target/release/jmcomic_local --file queue.txt
```

构建需要 Rust 工具链和依赖下载网络；本机使用 Rust 1.98.0 验证。编译后可以直接运行二进制文件。复制到其他目录时，将 `config.toml`、`queue.txt` 放在二进制文件旁，或用 `--config` 指定配置位置。

如果官方 crates.io 连接超时，可仅为这次构建使用镜像，不修改全局配置：

```sh
cargo --config 'source.crates-io.replace-with="rsproxy"' \
  --config 'source.rsproxy.registry="sparse+https://rsproxy.cn/index/"' \
  build --release --locked
```

## 命令行

```sh
# 帮助
./target/release/jmcomic_local --help

# 读取清单下载
./target/release/jmcomic_local --file queue.txt

# 只预览，不联网（下列 ID 仅为格式示例）
./target/release/jmcomic_local --dry-run JM0001 1,2
./target/release/jmcomic_local --file queue.txt --dry-run

# 检查配置和本地记录，不访问平台
./target/release/jmcomic_local --doctor

# 查看任务记录
./target/release/jmcomic_local --status

# 指定配置与记录路径
./target/release/jmcomic_local --config /path/to/config.toml --state /path/to/history.json --status
```

支持纯数字、`JM` 前缀、中文标点、UTF-8 BOM、`#` 行内注释和自动去重。ID 支持 1–20 位数字，必须大于零。也支持通过标准输入管道传入 ID。退出码：成功/无任务 `0`，批次存在失败 `1`，输入/配置/运行环境错误 `2`，Ctrl+C 中断 `130`。

## 原有功能与数据兼容

- 保留原配置的 `download_dir`、`attempts`、`retry_delay`、`request_retries`、`timeout`、`image_threads` 及取值范围。
- 每本漫画单独重试，失败后继续下一本；网络请求在各备用域名上分别重试。
- 直接实现移动接口签名、AES-256-ECB 响应解密、会话 Cookie 初始化、域名更新和图片分块还原。
- 缓存图片在复用前完整解码；损坏缓存会删除并重下。图片先在内存中还原并校验，再以临时文件原子替换，减少中断造成的残缺文件。GIF 保留动画原始字节。
- 图片或章节部分失败时，整本记录为 `failed`，不会误记为完成。缺少图片还原参数或返回了错误 ID 时也会失败，避免把错误结果当成成功。
- 沿用 `history.json` 的版本 1 结构：`complete`、`failed`、`interrupted`、`running`。历史和图片文件分别原子写入，历史文件在 Unix 上默认仅当前用户可读写。
- 完成任务按保存目录、文件存在性和大小判断是否跳过，与原版一致；这一步不计算哈希或重新解码。`running` 遗留记录在再次提交该 ID 时重试。
- 对历史记录及下载目录同时加进程锁。程序关闭后不驻留后台，不同步账号收藏，也不自动检查更新。

当前 Rust 工作区与原目录独立，没有复制或改动原目录的 `data/`、`downloads/`。需要继续使用旧数据时，将配置指向旧下载目录并用 `--state` 指向旧历史记录；也可以直接用 `--config /path/to/旧项目/config.toml`，此时默认记录路径为旧配置目录中的 `data/history.json`。

相对下载路径以**配置文件所在目录**为起点，`--file` 和显式 `--state` 的相对路径以当前工作目录为起点。默认配置优先查找当前目录，然后查找可执行文件旁及其上两级目录；默认历史路径为配置目录下的 `data/history.json`。这使发布的二进制文件可以与配置和数据一起移动。

## 接口配置

默认域名与协议参数来自原项目已安装的 `jmcomic 2.7.9`，启动下载时尝试更新 API 域名并初始化会话。可在 `config.toml` 中覆盖：

```toml
auto_update_domains = false
api_domains = ["https://your-api-host"]
image_domains = ["https://your-image-host"]
app_version = "2.1.7"
```

也可通过 `domain_servers` 指定域名更新服务列表。接口和平台域名可能变化；本地检查和模拟测试不能证明真实平台可达。Ctrl+C 会等待当前网络请求结束或超时，再保存中断状态；重新运行相同 ID 可复用完整图片。

## 验证与代码结构

```sh
cargo fmt --check
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
```

测试使用合成 ID、合成图片和本地 HTTP 服务，不请求实际漫画。覆盖 ID/配置解析、重试、跳过、补下、损坏缓存、中断状态、锁、旧格式历史、签名、Cookie、域名发现、响应解密、章节排序和图片还原。真实平台下载尚未验证。

| 文件 | 职责 |
| --- | --- |
| `src/main.rs` | 命令行、交互输入、离线检查、中断信号 |
| `src/config.rs` | TOML 配置与路径处理 |
| `src/batch.rs` | 批次顺序、重试、状态转换 |
| `src/state.rs` | 历史记录、文件清单、进程锁、原子写入 |
| `src/client.rs` | HTTP、域名、会话、签名与接口解密 |
| `src/images.rs` | 图片解码检查与分块还原 |

协议适配参考 [JMComic-Crawler-Python](https://github.com/hect0x7/JMComic-Crawler-Python) 及原目录安装的 2.7.9 源码，相关 MIT 许可见 `THIRD_PARTY_LICENSES.txt`。原项目文档：[jmcomic](https://jmcomic.readthedocs.io/zh-cn/latest/)。
