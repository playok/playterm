# playterm

[한국어](README.md) | [English](README.en.md) | [日本語](README.ja.md) | **简体中文** | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Deutsch](README.de.md) | [Français](README.fr.md) | [繁體中文](README.zh-TW.md) | [Русский](README.ru.md)

**在一个窗口中管理所有设备：通过标签页使用 SSH、Telnet、串口控制台和 Windows 远程桌面。**

playterm 是使用 Rust（egui、alacritty_terminal、russh、IronRDP）开发的原生终端应用。Windows x64 和 ARM64 版本提供单个便携式可执行文件，无需安装运行时；macOS 提供 Apple silicon 和 Intel 应用。

本文是面向开发分支 `master` 的简体中文使用指南。已发布功能请查看 [v0.7.0 变更](docs/releases/v0.7.0.md)和[发行说明](https://github.com/playok/playterm/releases)。[英文详细参考](README.en.md)包含完整设置、开发与打包流程。

## 主要功能

- 在标签页和分屏中同时使用 SSH、Telnet、串口和 RDP。
- Ctrl/Shift+单击选择多个站点并一起连接，将站点和布局保存为模板。
- SSH 密钥、证书和外部代理认证，跳板机，`-L` / `-R` / `-D` 端口转发及 X11 转发。
- SFTP 面板、ZMODEM 收发（`sz` / `rz`）、自动登录、宏、同时输入和会话日志。
- 使用主密码加密的密码库（Argon2id + XChaCha20-Poly1305）。
- 导入 PuTTY、Xshell 和 SecureCRT 会话；支持文件夹、标签颜色和连接模板。
- 十种界面语言、深色与浅色主题、字体选择、回滚搜索和多语言字符编码。

## 截图

截图使用韩语界面，可在设置中切换为简体中文。

![SSH 连接界面（深色主题）](docs/screenshots/main-dark.png)

| 通过模板并排打开连接 | 未连接标签页的起始界面 |
| --- | --- |
| ![两个 SSH 会话（浅色主题）](docs/screenshots/split-light.png) | ![最近站点和快速连接](docs/screenshots/start-light.png) |

## 下载与启动

从 [GitHub Releases](https://github.com/playok/playterm/releases)下载适合操作系统和 CPU 的软件包。

- **Windows：**解压 ZIP 后运行 `playterm.exe`，无需另行安装 VC++ 运行时。后续发行版的标准软件包使用未压缩可执行文件，并提供可选 UPX 软件包；具体格式请查看对应发行说明。
- **macOS 11 及以上：**打开 DMG，将 `playterm.app` 拖到 Applications，也可使用 ZIP。签名、公证和首次启动说明见发行说明及 [macOS 详细指南（英文）](README.en.md#macos-app)。
- 应用内自动安装更新仅支持 Windows。macOS 会打开新版本的下载页面。

在“工具 → 设置”的语言选项中选择 **简体中文**。修改会立即预览，取消则恢复原设置；保存后重启仍会保留。界面语言与远程连接的字符编码互不影响。

## 建立连接

通过站点管理器保存连接，或使用快速连接。在未连接标签页的 `playterm>` 提示符中，可输入 `ssh user@host`、`telnet host`、`serial COM3 115200`、`rdp user@host` 或 `help`。

也可从命令行直接连接：

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

本地提示符中的 `sites` 列出已保存站点，`open 名称` 或 `open 编号` 连接站点；`ls` / `dir`、`cd`、`pwd` 操作本地文件。连接结束后会返回提示符。[完整命令列表（英文）](README.en.md#local-command-prompt)。

## 日常操作

- **站点与分屏：**把标签拖到另一个标签组的边缘即可分屏。删除站点文件夹不会删除其中的站点，而是将内容移到上一级。PuTTY、Xshell 和 SecureCRT 的密码不会导入。
- **复制与粘贴：**使用 Ctrl+Shift+C / Ctrl+Shift+V。Ctrl+C 在有选区时复制，否则发送 `^C`。可在设置中开启选中即复制和右键粘贴。远程程序未启用 bracketed paste 时，多行粘贴会先请求确认。
- **搜索与显示：**Ctrl+Shift+F 搜索屏幕和回滚内容；Ctrl+滚轮、Ctrl+= / Ctrl+- / Ctrl+0 调整字体大小。当前系统的快捷键以菜单显示为准。
- **同时输入：**Ctrl+Shift+I 开启后，按键和粘贴也会发送到其他目标标签。请确认橙色目标标记；可通过标签右键菜单排除某个标签。
- **命令栏与宏：**Ctrl+Shift+M 显示命令栏，Enter 发送、Shift+Enter 换行。宏支持 `@wait`、`@sleep`、`@if` 等指令；应写入 `{password}`，而不是密码本身。
- **自动登录与保活：**按站点配置“等待提示 → 发送内容”，支持 `{user}` 和 `{password}`。可选择 SSH/Telnet 协议保活，或发送文本以防设备输入超时。
- **字符编码：**自动检测仅区分 UTF-8 和 CP949。Shift_JIS、EUC-JP、GBK、Big5、Windows 代码页等需在连接设置中明确选择。[编码与翻译结构](docs/i18n.md)。

## 文件传输与 RDP

SFTP 面板位于 SSH 终端旁，支持下载、上传、创建文件夹、重命名和删除。没有 SFTP 的设备可在服务器执行 `sz file` 下载到本机，执行 `rz` 从本机上传（服务器需要 lrzsz）。ZMODEM 支持 SSH、Telnet 和串口，可用 Ctrl+C 取消。

RDP 在标签内显示远程桌面，支持分辨率调整、声音和 Unicode 输入。剪贴板、文件夹和打印机共享**仅支持 Windows**。可以使用 RD Gateway；暂不支持麦克风和 Kerberos。首次连接需验证证书指纹，保存后若证书改变则拒绝连接。[RDP 详细说明（英文）](README.en.md#remote-desktop-rdp)。

## 认证与配置文件

首次 SSH 连接时请核对主机密钥指纹，已保存密钥发生变化时会拒绝连接。SSH 代理转发（`-A`）和受信任 X11 转发（`-Y`）只应对可信服务器启用。本地 RSA 私钥签名受到限制，参见[认证说明（英文）](README.en.md#ssh-authentication-order)和 [RSA 缓解措施](docs/security-rsa-223.md)。

Windows 默认配置目录为 `%APPDATA%\playterm\config\`。可通过 `PLAYTERM_CONFIG_DIR` 修改，用于便携运行或隔离测试配置。

| 文件 | 内容 |
| --- | --- |
| `sessions.toml` | 站点和模板，不含密码 |
| `settings.toml` | 语言、显示、按键、宏和更新设置等 |
| `vault.toml` | 使用主密码加密的已保存密码 |
| `known_hosts` | 已信任的 SSH 主机密钥 |
| `known_rdp_hosts` | 已信任的 RDP 证书指纹 |

忘记主密码后无法恢复已保存密码。宏以明文保存在设置中，请勿直接写入秘密信息。

## 构建与开发

需要 **Rust 1.95 或更新版本**。Windows 需要 Visual Studio 2022 MSVC 构建工具，ARM64 还需要 LLVM/clang；macOS 需要 Xcode command line tools。Linux 依赖请参见[贡献指南](CONTRIBUTING.md)。

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Windows 提供 `build.cmd` 和 `release.cmd`；macOS 使用 `scripts/package-macos.sh` 打包。[构建与签名详情（英文）](README.en.md#building)。

- [兼容性](docs/compatibility.md) · [验证结果](docs/validation.md) · [压力测试](docs/stress-testing.md)
- [发布流程](docs/release-process.md) · [版本变更](docs/releases/)
- [添加翻译](docs/i18n.md) · [贡献指南](CONTRIBUTING.md)
- [私下报告安全漏洞](SECURITY.md)

以上详细文档主要使用英语或韩语。

## 许可证

[MIT](LICENSE)。按钮图标字体 Material Symbols Rounded 的子集采用 [Apache License 2.0](assets/icons/LICENSE.txt)。可在应用的“帮助 → 开源许可证”中查看依赖项许可证。
