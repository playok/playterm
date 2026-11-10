# playterm

[한국어](README.md) | [English](README.en.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Deutsch](README.de.md) | [Français](README.fr.md) | **繁體中文** | [Русский](README.ru.md)

**在同一個視窗管理所有設備：透過分頁使用 SSH、Telnet、序列埠主控台與 Windows 遠端桌面。**

playterm 是以 Rust（egui、alacritty_terminal、russh、IronRDP）開發的原生終端機應用程式。Windows x64 與 ARM64 版本提供單一可攜式執行檔，不需另外安裝執行階段；macOS 提供 Apple silicon 與 Intel 應用程式。

本文是針對開發分支 `master` 的繁體中文使用指南，採用臺灣用語。已發行功能請參閱 [v0.7.0 變更](docs/releases/v0.7.0.md)與[發行說明](https://github.com/playok/playterm/releases)。[英文詳細參考](README.en.md)包含完整設定、開發與封裝流程。

## 主要功能

- 在分頁與分割畫面中同時使用 SSH、Telnet、序列埠和 RDP。
- Ctrl/Shift+點選多個站台並一併連線，將站台與配置儲存為範本。
- SSH 金鑰、憑證及外部代理程式驗證，跳板主機，`-L` / `-R` / `-D` 連接埠轉送與 X11 轉送。
- SFTP 面板、ZMODEM 收發（`sz` / `rz`）、自動登入、巨集、同時輸入和工作階段記錄。
- 以主密碼加密的密碼保管庫（Argon2id + XChaCha20-Poly1305）。
- 匯入 PuTTY、Xshell 與 SecureCRT 工作階段；支援資料夾、分頁色彩與連線範本。
- 十種介面語言、深色與淺色佈景主題、字型選擇、捲動歷史搜尋與多語言字元編碼。

## 螢幕擷取畫面

圖片使用韓文介面，可在設定中切換成繁體中文。

![SSH 連線畫面（深色佈景主題）](docs/screenshots/main-dark.png)

| 透過範本並排開啟連線 | 未連線分頁的起始畫面 |
| --- | --- |
| ![兩個 SSH 工作階段（淺色佈景主題）](docs/screenshots/split-light.png) | ![最近站台與快速連線](docs/screenshots/start-light.png) |

## 下載與啟動

從 [GitHub Releases](https://github.com/playok/playterm/releases)下載適合作業系統和 CPU 的套件。

- **Windows：**解壓縮 ZIP 後執行 `playterm.exe`，不需另外安裝 VC++ 執行階段。後續版本的標準套件採用未壓縮執行檔，並提供可選的 UPX 套件；實際格式請查看各版本發行說明。
- **macOS 11 以上：**開啟 DMG，將 `playterm.app` 拖到 Applications，也可使用 ZIP。簽署、公證與首次啟動方式請參閱發行說明及 [macOS 詳細指南（英文）](README.en.md#macos-app)。
- 應用程式內安裝更新僅支援 Windows。macOS 會開啟新版下載頁面。

在「工具 → 設定」的語言欄位選擇 **繁體中文**。變更會立即預覽，取消則還原；儲存後重新啟動仍會保留。介面語言與遠端連線的字元編碼是獨立設定。

## 建立連線

透過站台管理員儲存連線，或使用快速連線。未連線分頁的 `playterm>` 提示字元接受 `ssh user@host`、`telnet host`、`serial COM3 115200`、`rdp user@host` 和 `help`。

也可從命令列直接連線：

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

本機提示字元中的 `sites` 列出已儲存站台，`open 名稱` 或 `open 編號` 連線至站台；`ls` / `dir`、`cd`、`pwd` 操作本機檔案。連線結束後會回到提示字元。[完整命令清單（英文）](README.en.md#local-command-prompt)。

## 日常操作

- **站台與分割：**將分頁拖到另一個分頁群組的邊緣即可分割畫面。刪除站台資料夾不會刪除其中的站台，內容會移到上一層。PuTTY、Xshell 與 SecureCRT 的密碼不會匯入。
- **複製與貼上：**使用 Ctrl+Shift+C / Ctrl+Shift+V。Ctrl+C 有選取內容時複製，否則傳送 `^C`。可在設定啟用選取即複製和右鍵貼上。遠端程式未啟用 bracketed paste 時，多行貼上會先要求確認。
- **搜尋與顯示：**Ctrl+Shift+F 搜尋畫面與捲動歷史；Ctrl+滾輪、Ctrl+= / Ctrl+- / Ctrl+0 調整字型大小。各作業系統的快速鍵以選單顯示為準。
- **同時輸入：**Ctrl+Shift+I 啟用後，按鍵與貼上也會傳送至其他目標分頁。請確認橘色目標標示；可透過分頁右鍵選單排除分頁。
- **命令列與巨集：**Ctrl+Shift+M 顯示輸入列，Enter 傳送、Shift+Enter 換行。巨集支援 `@wait`、`@sleep`、`@if` 等指令；請使用 `{password}`，不要直接寫入密碼。
- **自動登入與保持連線：**依站台設定「等候提示 → 傳送內容」，支援 `{user}` 與 `{password}`。可選擇 SSH/Telnet 通訊協定保持連線，或傳送文字以避免設備輸入逾時。
- **字元編碼：**自動偵測僅區分 UTF-8 與 CP949。Shift_JIS、EUC-JP、GBK、Big5、Windows 字碼頁等須在連線設定中明確選擇。[編碼與翻譯結構](docs/i18n.md)。

## 檔案傳輸與 RDP

SFTP 面板位於 SSH 終端機旁，支援下載、上傳、建立資料夾、重新命名與刪除。沒有 SFTP 的設備可在伺服器執行 `sz file` 下載至本機，執行 `rz` 從本機上傳（伺服器需要 lrzsz）。ZMODEM 支援 SSH、Telnet 與序列埠，可用 Ctrl+C 取消。

RDP 在分頁中顯示遠端桌面，支援解析度調整、音效與 Unicode 輸入。剪貼簿、資料夾及印表機共用**僅支援 Windows**。可使用 RD Gateway；尚不支援麥克風與 Kerberos。首次連線需驗證憑證指紋，儲存後若憑證變更則拒絕連線。[RDP 詳細說明（英文）](README.en.md#remote-desktop-rdp)。

## 驗證與設定檔

首次 SSH 連線時請核對主機金鑰指紋，已儲存金鑰變更時會拒絕連線。SSH 代理程式轉送（`-A`）與信任的 X11 轉送（`-Y`）僅應對可信任伺服器啟用。本機 RSA 私密金鑰簽署受到限制，請參閱[驗證說明（英文）](README.en.md#ssh-authentication-order)與 [RSA 緩解措施](docs/security-rsa-223.md)。

Windows 預設設定目錄是 `%APPDATA%\playterm\config\`。可透過 `PLAYTERM_CONFIG_DIR` 變更，以便可攜式使用或隔離測試設定。

| 檔案 | 內容 |
| --- | --- |
| `sessions.toml` | 站台與範本，不含密碼 |
| `settings.toml` | 語言、顯示、按鍵、巨集及更新設定等 |
| `vault.toml` | 以主密碼加密的已儲存密碼 |
| `known_hosts` | 已信任的 SSH 主機金鑰 |
| `known_rdp_hosts` | 已信任的 RDP 憑證指紋 |

忘記主密碼後無法復原已儲存密碼。巨集以明文儲存在設定中，請勿直接寫入機密資訊。

## 建置與開發

需要 **Rust 1.95 以上**。Windows 需要 Visual Studio 2022 MSVC 建置工具，ARM64 另需 LLVM/clang；macOS 需要 Xcode command line tools。Linux 相依套件請參閱[貢獻指南](CONTRIBUTING.md)。

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Windows 提供 `build.cmd` 與 `release.cmd`；macOS 使用 `scripts/package-macos.sh` 封裝。[建置與簽署詳情（英文）](README.en.md#building)。

- [相容性](docs/compatibility.md) · [驗證結果](docs/validation.md) · [壓力測試](docs/stress-testing.md)
- [發行流程](docs/release-process.md) · [版本變更](docs/releases/)
- [新增翻譯](docs/i18n.md) · [貢獻指南](CONTRIBUTING.md)
- [私下回報安全漏洞](SECURITY.md)

以上詳細文件主要使用英文或韓文。

## 授權

[MIT](LICENSE)。按鈕圖示字型 Material Symbols Rounded 的子集採用 [Apache License 2.0](assets/icons/LICENSE.txt)。可在應用程式的「說明 → 開放原始碼授權」中查看相依元件授權。
