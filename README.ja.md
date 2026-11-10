# playterm

[한국어](README.md) | [English](README.en.md) | **日本語** | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Deutsch](README.de.md) | [Français](README.fr.md) | [繁體中文](README.zh-TW.md) | [Русский](README.ru.md)

**SSH、Telnet、シリアルコンソール、Windows リモートデスクトップを、ひとつのウィンドウのタブで管理。**

playterm は Rust（egui、alacritty_terminal、russh、IronRDP）で開発されたネイティブ端末アプリです。Windows x64・ARM64 ではインストール不要の単一実行ファイルとして動作し、macOS では Apple silicon・Intel 向けのアプリを提供します。

この文書は開発中の `master` を対象とした日本語の利用ガイドです。配布済みの機能は [v0.7.0 の変更点](docs/releases/v0.7.0.md)と[リリースノート](https://github.com/playok/playterm/releases)を参照してください。[詳細リファレンス（英語）](README.en.md)には、各設定と開発・配布手順を掲載しています。

## 主な機能

- SSH、Telnet、シリアル、RDP をタブや分割画面で同時に利用。
- サイトを Ctrl/Shift+クリックで複数選択し、一括接続。接続先と配置をテンプレートに保存。
- SSH 鍵・証明書・外部エージェント認証、ジャンプホスト、`-L` / `-R` / `-D` 転送、X11 転送。
- SFTP パネル、ZMODEM の送受信（`sz` / `rz`）、自動ログイン、マクロ、同時入力、セッションログ。
- マスターパスワードで暗号化するパスワード保管庫（Argon2id + XChaCha20-Poly1305）。
- PuTTY・Xshell・SecureCRT のセッション取り込み、フォルダー、タブの色、接続テンプレート。
- 10 言語の UI、ダーク・ライトテーマ、フォント選択、スクロールバック検索、多言語文字コード。

## 画面

画像は韓国語 UI です。設定で日本語に切り替えられます。

![SSH 接続中の画面（ダークテーマ）](docs/screenshots/main-dark.png)

| テンプレートから並べた接続 | 未接続タブの開始画面 |
| --- | --- |
| ![2 つの SSH セッション（ライトテーマ）](docs/screenshots/split-light.png) | ![最近のサイトとクイック接続](docs/screenshots/start-light.png) |

## ダウンロードと起動

[GitHub Releases](https://github.com/playok/playterm/releases)から OS と CPU に合うパッケージを取得してください。

- **Windows:** ZIP を展開し、`playterm.exe` を実行します。VC++ ランタイムの追加インストールは不要です。標準パッケージは非圧縮実行ファイルで、任意の UPX パッケージも提供する予定です。配布形式は各リリースの説明を確認してください。
- **macOS 11 以降:** DMG を開き、`playterm.app` を Applications にドラッグします。ZIP 版も利用できます。署名・公証の状態と初回起動の方法はリリースノートと[macOS の詳細](README.en.md#macos-app)を確認してください。
- アプリ内での更新インストールは Windows のみです。macOS では新しいバージョンの配布ページを開きます。

「ツール → 設定」の言語欄で **日本語** を選びます。変更はすぐにプレビューされ、キャンセルすると元に戻ります。保存した選択は再起動後も維持されます。UI 言語と接続先の文字コードは別の設定です。

## 接続する

サイトマネージャーで接続先を保存するか、クイック接続を使います。未接続タブの `playterm>` プロンプトでは `ssh user@host`、`telnet host`、`serial COM3 115200`、`rdp user@host`、`help` を入力できます。

コマンドラインから直接接続する例:

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

ローカルプロンプトの `sites` は保存済みサイト一覧、`open 名前` または `open 番号` は保存済みサイトへの接続です。`ls` / `dir`、`cd`、`pwd` はローカルファイルを扱います。接続が終了するとプロンプトに戻ります。[コマンド一覧（英語）](README.en.md#local-command-prompt)。

## 日常の操作

- **サイトと分割:** タブを別のタブグループの端へドラッグして分割します。サイトのフォルダーを削除しても中のサイトは削除されず、親フォルダーへ移動します。PuTTY・Xshell・SecureCRT のパスワードは取り込まれません。
- **コピー・貼り付け:** Ctrl+Shift+C / Ctrl+Shift+V。Ctrl+C は選択範囲があるとコピーし、なければ `^C` を送信します。選択時コピー・右クリック貼り付けは設定で有効にできます。複数行の貼り付けは、リモート側の bracketed paste が有効でない場合、実行前に確認されます。
- **検索と表示:** Ctrl+Shift+F でスクロールバックを含めて検索。Ctrl+ホイール、Ctrl+= / Ctrl+- / Ctrl+0 で文字サイズを変更します。OS に応じたショートカットはメニューで確認できます。
- **同時入力:** Ctrl+Shift+I で有効にすると、入力と貼り付けが他の対象タブにも送られます。オレンジ色の対象表示を確認してください。タブの右クリックで除外できます。
- **コマンドバーとマクロ:** Ctrl+Shift+M で入力欄を表示。Enter で送信、Shift+Enter で改行。マクロでは `@wait`、`@sleep`、`@if` などを使用でき、パスワードそのものではなく `{password}` を書きます。
- **自動ログインと接続維持:** サイトごとに「待機するプロンプト → 送信する文字列」を設定します。`{user}` と `{password}` を利用できます。SSH/Telnet のプロトコルによる接続維持と、機器の入力タイムアウトを防ぐ文字列送信を選べます。
- **文字コード:** 自動判定は UTF-8 と CP949 に限られます。Shift_JIS、EUC-JP、GBK、Big5、Windows コードページなどは接続設定で明示的に選択します。[文字コードと翻訳の構造](docs/i18n.md)。

## ファイル転送と RDP

SFTP は SSH 接続の横に表示されるパネルから操作します。ファイルのダウンロード・アップロード、フォルダー作成、名前変更、削除に対応します。SFTP がない機器ではサーバーの `sz file` で受信、`rz` で送信します（サーバー側に lrzsz が必要）。ZMODEM は SSH・Telnet・シリアルで使用でき、Ctrl+C で中止できます。

RDP はタブ内にリモートデスクトップを描画し、サイズ変更、音声、Unicode 入力に対応します。クリップボード・フォルダー・プリンター共有は **Windows のみ**です。RD Gateway を指定でき、マイクと Kerberos は未対応です。初回のサーバー証明書は指紋を確認し、保存後に変わった場合は接続を拒否します。[RDP の詳細（英語）](README.en.md#remote-desktop-rdp)。

## 認証と設定ファイル

初回 SSH 接続ではホスト鍵の指紋を確認してください。保存済みの鍵が変わると接続は拒否されます。SSH エージェント転送（`-A`）と信頼済み X11 転送（`-Y`）は、信頼できるサーバーでのみ有効にします。ローカル RSA 秘密鍵による署名には制限があります。[認証の詳細（英語）](README.en.md#ssh-authentication-order)と[RSA 対策記録](docs/security-rsa-223.md)を参照してください。

Windows の設定先は `%APPDATA%\playterm\config\` です。`PLAYTERM_CONFIG_DIR` で変更でき、ポータブル運用や検証用の設定分離に利用できます。

| ファイル | 内容 |
| --- | --- |
| `sessions.toml` | サイトとテンプレート（パスワードなし） |
| `settings.toml` | 言語、表示、キー操作、マクロ、更新設定など |
| `vault.toml` | マスターパスワードで暗号化した保存パスワード |
| `known_hosts` | 信頼した SSH ホスト鍵 |
| `known_rdp_hosts` | 信頼した RDP 証明書の指紋 |

マスターパスワードを忘れると保存パスワードは復元できません。マクロは平文で設定に保存されるため、秘密情報を直接書かないでください。

## ビルドと開発

Rust **1.95 以降**が必要です。Windows は Visual Studio 2022 の MSVC ビルドツール、ARM64 は追加で LLVM/clang が必要です。macOS は Xcode command line tools が必要です。Linux の依存パッケージは[貢献ガイド](CONTRIBUTING.md)を参照してください。

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Windows の補助スクリプトは `build.cmd` と `release.cmd`、macOS のパッケージ作成は `scripts/package-macos.sh` です。[ビルド・署名の詳細（英語）](README.en.md#building)。

- [互換性](docs/compatibility.md) · [検証結果](docs/validation.md) · [負荷テスト](docs/stress-testing.md)
- [リリース手順](docs/release-process.md) · [バージョン別変更履歴](docs/releases/)
- [翻訳の追加方法](docs/i18n.md) · [貢献ガイド](CONTRIBUTING.md)
- [脆弱性を非公開で報告](SECURITY.md)

上記の詳細文書は、主に英語または韓国語で提供しています。

## ライセンス

[MIT](LICENSE)。ボタン用アイコンフォント Material Symbols Rounded のサブセットには [Apache License 2.0](assets/icons/LICENSE.txt) が適用されます。依存ライブラリのライセンスはアプリの「ヘルプ → オープンソースライセンス」で確認できます。
