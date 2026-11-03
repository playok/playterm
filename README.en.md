# playterm

**English** | [한국어](README.md)

**One window for every box you manage — SSH, Telnet, serial consoles and Windows Remote Desktop, in tabs.**

playterm is a native Windows terminal written in Rust (egui + alacritty_terminal + russh + IronRDP).
It ships as a single portable `.exe` for x64 and ARM64, with no installer and no runtime to install.

## Highlights

- **SSH, Telnet, serial and RDP** side by side in one window
- **Split view**: drag tabs to the edges to see several sessions at once
- **Connect many at once**: Ctrl/Shift+click sites to open them together, or save them as a **template** (tabs, columns, rows or grid) and open it with a double-click
- **SSH extras**: key authentication (OpenSSH / PuTTY `.ppk`, Pageant, Windows OpenSSH agent), `-L` / `-R` / `-D` port forwarding, and an SFTP explorer panel beside the terminal
- **Remote Desktop**: resolution that follows the tab size, shared clipboard, Unicode/IME input, and certificate pinning
- **Password vault**: saved passwords are encrypted with a master password (Argon2id + XChaCha20-Poly1305)
- **Network-device friendly**: automatic login, typing into several tabs at once, session logging, keep-alive, CP949 (EUC-KR) with encoding auto-detection, and ZMODEM downloads and uploads (`sz`/`rz`)
- **Site manager**: folders, per-site tab colors, PuTTY and Xshell session import, and a `playterm>` command prompt in unconnected tabs
- Korean / English UI, dark / light themes

## Layout

| Crate | Role |
|---|---|
| `crates/playterm-term` | Terminal emulation (alacritty_terminal), UTF-8/CP949 conversion, color palettes |
| `crates/playterm-net` | SSH (russh) / Telnet / serial / RDP (IronRDP) transports, session event and command channels, ZMODEM receiving and sending |
| `crates/playterm-config` | Site list (`sessions.toml`), password vault (`vault.toml`), settings (`settings.toml`) |
| `crates/playterm-i18n` | Korean / English UI strings |
| `crates/playterm` | GUI app (eframe/egui): rendering, keyboard/IME input, dialogs, updates |
| `crates/playterm-sign` | Signs release checksums (for release builds) |

## Building

```powershell
cargo build --release
```

- Needs Rust 1.90+ and the MSVC build tools (Visual Studio 2022).
- **On Windows ARM64 you also need clang** (to build the crypto library `ring`):
  `winget install LLVM.LLVM`, then add `C:\Program Files\LLVM\bin` to PATH.

### Scripts (cmd)

| Command | What it does |
|---|---|
| `build.cmd` | Release build → `target\release\playterm.exe` (`build.cmd debug` for a debug build) |
| `release.cmd` | clippy → tests → ARM64 and x64 packages → zips and `SHA256SUMS.txt` in `dist\` (`release.cmd x64` builds just one). The exe is compressed with UPX (`noupx` turns it off) |

Both scripts put LLVM (clang) on PATH by themselves and build with `--locked`, so only the dependency versions in `Cargo.lock` are used.
`release.cmd` stops when there are uncommitted changes (`release.cmd dirty` makes a test package anyway).

### GitHub releases (automatic build)

Pushing a tag that starts with `v` runs GitHub Actions (`.github/workflows/release.yml`): on Windows clippy → tests → x64 and ARM64 packages
(UPX-compressed), on macOS tests → arm64 and x64 `playterm.app` zips (signed ad hoc). When both succeed, the four zips are uploaded with
`SHA256SUMS.txt`, its signature `SHA256SUMS.txt.sig` and `SHA256SUMS-macos.txt` (for the macOS zips) to that tag's release. The tag must match the version in `Cargo.toml`.

The in-app updater installs only releases with a valid signature, so a tag build needs the signing private key (hex) in the repository secret
`PLAYTERM_SIGNING_KEY`. Make a key with `cargo run -p playterm-sign -- keygen > keyfile` (the public key it prints goes into `PUBLIC_KEY` in
`crates/playterm/src/update.rs`) and store it with `gh secret set PLAYTERM_SIGNING_KEY < keyfile`. Keep the private key safe, outside the repository.
If it is lost, make a new key and release a version with the new `PUBLIC_KEY`; users of earlier versions then have to download that one by hand.

```powershell
git tag v0.1.0
git push origin v0.1.0
```

Running it by hand from the Actions tab (**Release → Run workflow**) makes no release; the same builds are kept as workflow artifacts (`playterm-windows`, `playterm-macos`; to try the build).

### Release packages

```powershell
rustup target add x86_64-pc-windows-msvc     # once
release.cmd                                  # or: pwsh scripts\package.ps1 -Upx (package only, no checks or tests; without -Upx nothing is compressed)
```

- The exe in the zip is compressed with **UPX** (about 21 MB → 7 MB). Set `UPX_EXE` (a path or command name) with `UPX_SHA256`, its SHA-256, or the
  script downloads a pinned version (5.3.0) once from the official GitHub release into `tools\upx`. The zip and the `upx.exe` in it are checked
  against SHA-256, and each run copies `upx.exe` to a new temporary folder and checks that copy right before running it (ARM64 exes need UPX 5.3.0 or newer). A compressed exe is unpacked in memory by each window (process),
  so several windows use a little more memory. Some antivirus products flag UPX-compressed exes; build with `release.cmd noupx` if that matters.
- The C runtime is linked statically (`.cargo/config.toml`), so no VC++ redistributable is needed.
- DLLs are loaded from System32 only (the `/DEPENDENTLOADFLAG` link option and `SetDefaultDllDirectories` at start): a DLL of the same name
  next to the exe (in Downloads, say) is not used.
- The screen is drawn with OpenGL (glow); where OpenGL is not available (a VM without a GPU, for example) playterm switches to wgpu (DX12) by itself.
  Set `PLAYTERM_RENDERER=wgpu` or `glow` to force one.
- The icon is generated by `assets/make_icon.py` (`playterm.png`, `playterm.ico`, and `playterm.icns` for macOS).
- Button icons are drawn with a subset of Material Symbols Rounded (Apache-2.0) holding only the icons used (`assets/icons/material-symbols-subset.ttf`, about 7 KB).
  After adding an icon to `crates/playterm/src/icon.rs`, run `python assets/make_icon_font.py` (needs fontTools) again; a test reports missing icons.

### macOS (.app)

playterm also builds and runs on macOS (needs Rust 1.95+ and the Xcode command line tools; runs on macOS 11 or later).

```sh
cargo build --release                        # target/release/playterm
scripts/package-macos.sh                     # arm64 and x64 → dist/playterm-<version>-macos-<arch>.zip (playterm.app)
scripts/package-macos.sh universal           # one app for Apple silicon and Intel (lipo)
RUSTUP_TOOLCHAIN=1.95 scripts/package-macos.sh arm64   # with a toolchain other than the default
```

- Unzip and move `playterm.app` to Applications. The SHA-256 sums of the zips go to `dist/SHA256SUMS-macos.txt`.
- The app is **signed ad hoc** by default. Gatekeeper stops such an app downloaded from the internet on its first run: right-click → Open (or
  System Settings → Privacy & Security → Open Anyway). With `MACOS_SIGN_IDENTITY` set to a "Developer ID Application" certificate, the app is
  signed with the hardened runtime and a timestamp, ready for `xcrun notarytool submit <zip> --wait` and `xcrun stapler staple`.
- The terminal font defaults to Menlo, with Apple SD Gothic Neo for Hangul. The default SSH user is `USER`, and the agent is `SSH_AUTH_SOCK`.
- Installing updates from within the app works on Windows only. On macOS a new version (when its release has a zip for this Mac) is announced
  and its release page opened.

## Running

```powershell
playterm                                  # connection dialog
playterm ssh://user@host:22               # connect right away
playterm --cp949 telnet://192.168.0.1     # older CP949 (EUC-KR) devices
playterm -i ~/.ssh/id_ed25519 ssh://user@host   # key file
playterm -L 8080:db:5432 -D 1080 ssh://user@host   # port forwarding (same syntax as OpenSSH, -R works too)
playterm serial://COM3:115200             # serial console (8N1)
playterm rdp://user@host                  # Windows Remote Desktop
```

## Local command prompt

A tab that is not connected (a new tab with Ctrl+Shift+N, a tab whose connection ended, or when no tab is open) shows a `playterm>` prompt.
Connection commands connect in that tab, and the prompt comes back when the connection ends.

| Command | Does |
|---|---|
| `ssh [-p port] [-l user] [-i key] [-J jumphost] [-L/-R/-D ...] [--cp949] [user@]host[:port]` | SSH (without a user, your user name on this PC). `-J` goes through a jump host: a site name or `user@host:port` |
| `telnet host [port]` | Telnet |
| `serial COM3 [speed]` | Serial console |
| `rdp [user@]host[:port]` | Remote Desktop |
| `ping [-t] [-n count] [-w ms] [-l size] [-4\|-6] host` | Check that a host answers (the options of Windows' `ping`: 4 times by default, `-t` until Ctrl+C, `-c count` works too). Sent through the Windows ICMP API without administrator rights; at the end it shows the loss and round-trip statistics |
| `open` | Site Manager |
| `open name\|number` | Connect a saved site (the number from `sites`, or part of the name) |
| `sites [search]` | List saved sites |
| `ls [-l] [-a] [path]`, `dir [/a] [path]` | List files (patterns like `*.log` work, `-a`: include hidden files) |
| `cd [path]`, `pwd` | Change folder (`cd` alone goes home, `cd ..`, `cd D:`) / current folder |
| `new` / `quick` | New site / Quick Connect |
| `r` | Reconnect the last connection |
| `clear`, `exit`, `help` | Clear the screen, close the tab, help |

←/→/Home/End edit the line, ↑/↓ recall earlier commands, Ctrl+C cancels the input, Ctrl+L clears the screen.

## Menus and the site manager

- Menu bar: **File · Edit · View · Tools · Tab · Window · Help**. Items with a shortcut show it in the menu.
- **Help → Open-source licenses**: the licenses of playterm (MIT) and of every open-source component built into the executable (searchable, with each one's full license text and source repository).
  The list is generated with `cargo metadata` at build time, so it follows dependency changes by itself.
- **File → Site Manager** (Ctrl+Shift+S): sites by folder (searchable) on the left, the settings of the selected site on the right.
  It is a window that does not cover the app: drag it aside and change settings while reading the terminal (a connection error, say). Esc or × closes it.
  - [New site] / [New folder] / [Duplicate] / [Delete]; [Connect] / [Save] / [Revert] at the bottom right
  - **Folders** can be nested (`prod/web`). Right-click a folder for New site here, New folder inside, Rename and Delete; right-click a site for
    Connect, Duplicate, Move to folder and Delete; right-click the empty space for New site and New folder.
    Renaming a folder takes its sites and subfolders along; deleting a folder deletes no site: what is inside moves up to the parent folder.
  - Folders have a yellow folder icon, sites a badge with the connection type (SSH green, TEL orange, COM (serial) purple, RDP blue), also in the site list.
  - **[Tree] / [Grid]** switches the view. The grid has one row per site (type, name, target, user, folder); the chosen view is remembered.
  - Moving to another site or closing the window with unsaved changes asks whether to save them.
  - Double-clicking a site connects it and closes the window.
- The site list on the left of the main window (Ctrl+Shift+B) is for quick access. [Manage] or right-click → Edit opens the Site Manager.
- **File → Import → PuTTY sessions**: pick sessions saved by PuTTY (in the registry) and import them as sites.
  Host, port, user, key file, encoding, terminal type, port forwarding and serial settings are carried over; sites that already exist are unchecked by default.
  PuTTY does not store passwords, so none are imported. rlogin/raw sessions are skipped.
- **File → Import → Xshell sessions**: reads the `.xsh` files in Xshell's sessions folder (`Documents\NetSarang Computer\<version>\Xshell\Sessions`,
  or `%APPDATA%\NetSarang\Xshell\Sessions` for Xshell 5) and imports them as sites. Pick **Other folder…** when it is not found or lives elsewhere.
  Xshell's subfolders are kept inside the import folder. Host, port, user, encoding and terminal type of SSH, Telnet and serial sessions are
  carried over; passwords Xshell saved (encrypted), user keys kept inside Xshell and port forwardings are not, and the list says so. Check serial
  settings after the import.
- **Tab color**: give each site a color (red, orange, yellow, green, teal, blue, purple, pink, gray) under "Tab color" in the Site Manager. Its tabs
  are tinted (a bar at the bottom and a light background) and the site list shows the color too — handy for marking production servers red, say.
  An open tab can be recolored by itself: right-click the tab → **Tab color** (or Tab menu → Tab color).
- **Open in new window**: right-click a tab (or the Tab menu) → Open in new window. A second playterm opens that tab's connection in a new window (the original tab stays).
  Passwords are not passed along: a saved site unlocks the vault in the new window and uses its saved password; other connections ask again.
  File → **New window** opens an empty window (local prompt).
- **Docking (split view)**: drag a tab onto the left, right, top or bottom edge of another tab group to split the view and see several sessions together.
  Drop it in a tab strip to make it a tab of that group. Drag the border between panes to resize them; the pane you click becomes the current tab
  (blue outline). Closing the last tab of a group removes that pane, and new tabs open in the group of the current tab.
- **Connect several sites at once**: Ctrl+click (add/remove one) or Shift+click (a range) sites in the site list, and a bar under the list shows
  `N selected [Connect] [Side by side ▾] [Save template]` (also in the right-click menu of the selected sites).
  [Connect] opens them as tabs; [Side by side] splits the view into columns, rows or a grid. If some of them use passwords from the vault, the master password is asked only once.
- **Templates**: save a group of sites you often open together under a name (select them, then [Save template]). Choose the name, the layout
  (tabs / columns / rows / grid) and the sites and their order; double-click the template under **Templates** at the top of the site list
  (or right-click → Connect all) to open them all. Right-click → Edit / Delete. Templates are stored in `sessions.toml`; deleting a site removes it from its templates.

## Display and search

- **Tools → Settings**: language (한국어 / English), UI theme (dark / light / follow Windows), font, size, color theme, scrollback lines. Changes preview
  in the open tabs right away, and [Cancel] puts them back.
  - Font: any font installed on this PC (system and per-user, monospaced fonts first, searchable) or a TTF/OTF/TTC file via [From file…].
    Hangul missing from the font is drawn with Malgun Gothic.
  - Color themes: Campbell, One Half Dark/Light, Solarized Dark/Light, Tango Dark, Dracula, Gruvbox Dark
  - Connection timeout (default 15 s, 3–300 s): gives up when an SSH, Telnet or RDP server does not answer for that long. It covers everything
    from the TCP connect to the finished login (the SSH handshake and authentication, RDP's TLS and logon), not counting time spent confirming a
    host key or typing a password. Applies to new connections.
  - Copy / paste: the keys the terminal uses.
    - Copy: **copy on select** (as in PuTTY, off by default) · Ctrl+C (only with a selection, ^C otherwise) · Ctrl+Shift+C · Ctrl+Insert
    - Paste: Ctrl+V · Ctrl+Shift+V · Shift+Insert · **right click** (as in PuTTY, off by default — then Ctrl+right click opens the menu) · **middle click** (off by default)
    - Keys turned off go to the remote side as they are (with Ctrl+V off, vim and others get ^V). Menus and the shortcut list show the keys that are on.
    - Dialogs and text fields always take Ctrl+C/V/X, Ctrl/Shift+Insert and Shift+Delete, whatever the setting.
    - Remote copy (OSC 52, on by default): remote programs such as tmux or vim can put text on this PC's clipboard, only from the tab on screen,
      up to 1 MB, with a notice each time. Reading the clipboard from the remote side is always refused.
    - Programs that use bracketed paste get pasted text without any ESC in it, so a paste cannot end early and run as typed commands.
    - Confirm paste (on by default): pasting several lines where there is no bracketed paste would run each line as a command, so the text is shown first
      for confirmation.
    - Copying in the terminal slides up a short notice at the bottom, like `Copied · 104 chars · 3 lines “pla…~>”`: the size and the first
      and last 3 characters (line breaks shown as ⏎, tabs as →), so copying the wrong range is noticed right away.
- **Status bar** (bottom of the window, View → Status bar): connection state, target, encoding, logging indicator,
  terminal size (columns×rows), data received/sent on this connection (↓/↑, bytes after decryption), Caps Lock and Num Lock.
- Font size: **Ctrl+wheel**, **Ctrl+= / Ctrl+- / Ctrl+0** (also in the View menu). The size is saved.
- **Find (Ctrl+Shift+F)**: searches the screen and the scrollback. Enter goes up (older output), Shift+Enter down.
  Visible matches are yellow and the current one is selected, ready to copy. All-lowercase searches ignore case.

## Automatic login

Under **Automatic login** in the Site Manager, set "prompt → input" steps, and playterm types the input when that prompt appears after connecting.
Use it for logins on Telnet devices and serial consoles, or for commands after an SSH login (`sudo -i` and so on).

| Wait for | Send |
|---|---|
| `login:` (or `Username:`) | `{user}` |
| `Password:` | `{password}` |

- When a line on the screen **ends with** the text to wait for, the input is typed followed by Enter (case is ignored). Lines like "Last login: ..." do not match.
- `{user}` becomes the site's user name and `{password}` the password saved in the vault (Telnet and serial sites can store a user name and password too).
- Steps run once each, in order; the next step is checked after a new line arrives. Typing yourself (other than just Enter) stops the automatic login.
- It stops when the next prompt does not come within a minute, so a `Password:` showing up much later (another program's output) never gets the password.
- If no password is saved, it stops at that step and tells you.

## Typing into all tabs

For sending the same commands to several devices. Toggle it with **Ctrl+Shift+I** (or Tab menu → Type into all tabs).

- While it is on, the keys and pastes in the current tab **go to the other connected tabs too**, sent with each tab's encoding and cursor-key mode.
- The tab bar shows an orange `Typing into all tabs · N other` with [Turn off], and the tabs that receive the input have orange titles.
- To leave a tab out, right-click it and uncheck **Include in simultaneous input**. Unconnected tabs (the local prompt) receive nothing.

## Command input bar

A box under the terminal to write commands before sending them. Toggle it with View → **Command input bar** or **Ctrl+Shift+M** (remembered for the next run).

- **Enter** sends, **Shift+Enter** starts a new line. Several lines are typed one by one, each with Enter. Enter on an empty box sends just Enter.
- **↑ / ↓** bring back what you sent before.
- Send to: **This tab** (and the other tabs when typing into all tabs is on) or **All tabs** (every connected tab, except tabs with 'Include in simultaneous input' unchecked).

## Keep-alive and sleep prevention

So long-running sessions are not dropped, set a **keep-alive** for each site (Site Manager → Keep-alive). It is only sent when nothing has been sent for that long.

| Mode | Sends | Prevents |
|---|---|---|
| Protocol keep-alive (default, 60 s) | SSH keepalive request, Telnet `IAC NOP` | Idle disconnects by firewalls, routers and servers; also notices SSH connections that stopped answering |
| Type text | The text, as if typed (default `\x20\b` = space + backspace) | Idle logouts on devices and shells (Cisco `exec-timeout`, `TMOUT`) |
| Off | Nothing | |

- The text can use `\r` (Enter), `\n`, `\t`, `\b` (backspace), `\e` (Esc), `\\` and `\xHH`. Space + backspace leaves a half-typed command alone, but in a pager such as `--More--` the space turns the page.
- Serial has no protocol keep-alive, so use typed text there.

**Prevent sleep** (Settings → Prevent sleep): while a session is connected, Windows does not go to sleep from idleness and drop the connection.
The default is "No sleep while connected"; you can also pick "Keep the screen on while connected" or "Off". While it is active the status bar shows `No sleep`.
Closing the lid or choosing Sleep yourself still puts the PC to sleep.

**Prevent screen saver** (Tools menu, remembered for the next run): keeps the screen saver from starting, connected or not.
The sleep-prevention signal cannot stop the screen saver, so when the PC has had no input for over 30 seconds playterm sends a zero-distance
mouse move to reset the idle time. That also postpones idle screen-off and automatic locking. While it is active the status bar shows `No screen saver`.

## Encoding auto-detection

With a site's encoding set to **Auto-detect** (the default for new sites), playterm decides from the output whether it is UTF-8 or CP949.
A couple of Hangul characters settle it; if it is CP949, output that was garbled before is redrawn (the prompt lines from before the connection keep only their text).
The status bar shows something like `CP949 (EUC-KR) (auto-detected)`. Choosing UTF-8 or CP949 yourself turns detection off.

## Serial and logging

- In the connection dialog, protocol **Serial** → COM port (from the list or typed), speed, data bits, parity, stop bits, flow control.
- Right-click a tab → **Send BREAK** (serial: a 300 ms line break, Telnet: `IAC BRK`) — for entering ROMMON or password recovery on devices.
- Session logs: turn on automatic logging under "Session log" in the Site Manager, or start/stop it by hand from the Log menu of a tab (right-click).
  - Text: the output without control characters and color codes (UTF-8, optionally with a timestamp on every line)
  - Raw: the bytes exactly as received
  - Default folder: `Documents\playterm-logs`, file name: `SessionName_YYYYMMDD_HHMMSS.log`

## SFTP and port forwarding

- **SFTP explorer (side panel)**: Tools → SFTP explorer, [SFTP] in the tab bar, or **Ctrl+Shift+X** (Ctrl+Shift+E works too). A panel slides out from the right of the window, beside the terminal, and shows
  the remote files of the current tab (it follows when you switch tabs).
  Upload / upload folder / download / new folder / delete buttons, double-click to open a folder or download a file, drag files in from Explorer to upload.
  Drag the left edge of the panel to change its width.
- Downloads go to a partial file next to the target (`.name.….part`) that replaces it once complete, so a failed or cancelled download leaves an
  existing file as it was. Names from the server that cannot be used on this PC or could point outside the folder (`..`, `\`, `:`, device names
  such as `CON`, ...) are refused with a message.
- The SFTP explorer opens at the **current directory of the terminal's shell**; **[Terminal's folder]** goes back there any time.
  playterm opens another, invisible channel on the same SSH connection and reads the shell process's directory (`/proc/<shell>/cwd`): no shell
  setup is needed and nothing is typed into the terminal. Nested shells (`bash` in `bash`) are followed. It works on Linux servers; when the
  directory cannot be found (another OS, a shell of another user after `sudo`, ...) SFTP opens at the login directory.
- When a download finishes, the transfer list shows **[Open folder]**, which opens Windows Explorer (macOS: Finder) with the file selected.
- Icons tell files apart: folders are yellow folders, files get a colored band by kind (images green, archives purple, code blue, scripts orange,
  configuration teal, documents red, media pink, executables dark red; text and others striped).
  Symbolic links get a small arrow, and hidden files (starting with .) are dimmed.
  Change folders, upload (files/folders, or drop them on the panel), download (folders as a whole), new folder, rename, delete, transfer progress/cancel.
- **Selecting several**: Ctrl+click (add/remove one), Shift+click (a range), Ctrl+A or the checkbox at the top of the list (all). [Download (N)],
  [Delete (N)] and 'Download/Delete N selected' in the right-click menu act on all of them; downloading several asks for one folder to put them in.
  After a click in the list, Delete deletes the selection (after asking) and Esc clears it.
- Port forwarding is set under "Port forwarding" in the Site Manager and starts on connecting.
  - Local (L): a port on this PC → connected from the server to the target
  - Remote (R): a port on the server → connected from this PC to the target
  - Dynamic (D): a SOCKS5 proxy on this PC

## Macros

**Tools → Macros** runs a named sequence of commands in the current tab, and with simultaneous input on in the tabs that receive it.
**Manage macros…** creates and edits them.

```text
# a comment
terminal length 0     ← each line is typed with Enter
@wait #               ← wait until '#' comes in the output (ignoring case)
show running-config
@timeout 60           ← seconds the next @wait steps wait (default 30); past it the macro stops and says so
@sleep 1.5            ← pause (seconds)
@send y               ← type without Enter,  @enter ← Enter alone,  @@… ← a line that starts with '@'
```

- A running macro is shown in the terminal; a key press or **Tools → Macros → Stop the running macro** stops it.
- Macros are kept as they are in the settings file (`settings.toml`): do not put passwords in them.

## ZMODEM downloads (sz)

- Running `sz file...` on the server is recognized by the terminal, and the files are received on this PC. Works over SSH, Telnet and serial (the server needs lrzsz).
- They are saved in **Tools → Settings → ZMODEM downloads** (Windows' Downloads folder by default). A file with the same name is not replaced: the new one is saved as `name (1).ext`.
- With **Ask for the folder each time** (on by default), a window asks where to save when a download starts (starting at the folder picked last).
  `sz` gives up after about 20 seconds without an answer, so the files are received into a temporary folder meanwhile and moved to the chosen one when both are done. Cancelling the window cancels the download and deletes what was received.
- While receiving, the terminal shows the progress of each file, and at the end the files received and where they are. **Ctrl+C** (or Esc, Ctrl+X) cancels.
  After a cancel, the transfer data the server had already sent is dropped instead of filling the screen; only the shell prompt after it is shown.
- Every data block is checked (16/32-bit CRC); a damaged part is received again from where it went wrong, and only complete files are kept (a file is written under a temporary name and renamed at the end).
  Cancelling or losing the connection deletes the file being received. File names from the server lose their path and characters Windows does not allow, so nothing is saved outside the folder.
- ZCOMMAND, a server asking to run a command on this PC, is refused.

## ZMODEM uploads (rz)

- Run `rz` on the server and a window asks for the files to send (several can be picked); closing it cancels. Use it to put files on devices without SFTP (network gear over Telnet or serial, embedded boards).
- Each file shows its progress; **Ctrl+C** (or Ctrl+X, Esc) cancels. Files the server skips (e.g. one of that name exists) are marked.
- When the server already has part of a file (`rz -r`), the rest is sent; after an error, sending goes on from where the server asks.

## Remote Desktop (RDP)

Connects to a Windows Remote Desktop server and draws its screen inside a tab (playterm connects itself; it does not start mstsc).
Choose protocol **RDP** in the Site Manager, type `rdp [user@]host[:port]` at the prompt, or run `playterm rdp://user@host`.

- Connection: TCP → TLS → **NLA (CredSSP/NTLM)**. The user is `user` or `DOMAIN\user`. A missing or wrong password is asked for (up to 3 times), and
  'Save password' in the password dialog stores it in the vault.
- **Server certificate**: RDP servers usually have self-signed certificates, so, like an SSH host key, the first connection shows the fingerprint (SHA-256)
  and remembers it in `known_rdp_hosts` once you trust it. If the certificate changes later, the connection is refused.
- **Screen size**: by default it fits the tab — when the window, the site list or the SFTP panel changes the tab's size, the remote resolution follows
  (on servers that support the display control channel). A fixed size like 1920×1080 can be chosen too; if it is bigger than the tab it is scaled down, keeping its proportions.
- **Keyboard**: characters (including Korean IME input) are sent as Unicode, so they arrive as typed whatever the remote keyboard layout; Enter, arrows,
  F1–F12 and Ctrl/Alt combinations are sent as keys (scan codes). playterm shortcuts (Ctrl+Shift+…, Ctrl+Tab, Alt+←/→, F11) are not sent to the remote side.
- **Mouse**: movement, left/right/middle buttons, wheel. The remote cursor is drawn on the screen.
- **Shared clipboard** (site setting, on by default): the Windows clipboard and the remote clipboard are kept in step, so copy and paste work both ways.
- Text sent from the command input bar or by typing into all tabs reaches RDP tabs too.
- Not supported yet: sound, drive and printer redirection, RD Gateway, Kerberos (domain accounts authenticate with NTLM).

Test RDP server: `tools\dev-rdpd` (`cargo run --release` → 127.0.0.1:3389, tester / secret). It draws crosshairs that follow the mouse and prints the keyboard and mouse input it receives.

## Jump hosts (ProxyJump)

Put a saved site's name or `user@host:port` in a site's **Jump host** (quick connect: under Advanced) and playterm first connects to that
server (a bastion) over SSH, then reaches the target through a channel of that connection (as OpenSSH's `ProxyJump` / `-J`). At the local
prompt: `ssh -J bastion db01`.
- With a site's name, that site's key, saved password (while the vault is open) and its own jump host are used (A → B → target, up to 4 hops).
- In `user@host:port`, a missing user is the target site's, a missing port is 22. Login uses the agent and default keys, else a prompt.
- The host keys of the jump host and of the target are each checked against known_hosts.

## SSH authentication order

1. The key file set for the session (OpenSSH format / PuTTY `.ppk`; asks for the passphrase if it has one)
2. SSH agent — `SSH_AUTH_SOCK` if it is a named pipe, otherwise the Windows OpenSSH agent (`\\.\pipe\openssh-ssh-agent`), then Pageant (on macOS, the `SSH_AUTH_SOCK` socket)
3. The default key files `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa` (if present)
4. The saved password
5. keyboard-interactive / password prompt

RSA keys sign with `rsa-sha2-512` / `rsa-sha2-256`, as advertised by the server.

Password fields (login, key passphrase, vault master password, a site's password) switch the input method (IME) off: even right after typing
Korean in the terminal, a password is typed in Latin letters instead of jamo hidden behind the dots, which would fail the login. The input mode comes back when the dialog closes.

If you entered the wrong user name, fix it in the **User** field of the password dialog. SSH servers do not allow changing the user during authentication,
so playterm reconnects and logs in as that user; for a saved site, check **Save it in the site too** to change the site's user name as well.

Check **Save password** in the password dialog (for a saved site) to store the password in the vault after a successful login and switch the site to
'Save password'. A wrong password is never stored; if the vault is locked you are asked to unlock it (or create it the first time). Cancelling that dialog changes nothing.
A further factor asked after the password (a one-time code, say) never gets the saved password and is never saved as the password.

**Locking** the vault also takes the passwords back from the open tabs; reconnecting such a tab asks to unlock the vault first. Resetting the vault
with 'Forgot the master password?' keeps the old file as `vault.toml.bak`.

## Updates

At start-up playterm looks up the latest release on GitHub and tells you about a new version (turn it off in **Tools → Settings → Updates**;
**Help → Check for updates** checks at any time). **Update** downloads the zip for this PC (x64 / ARM64), compares it with the release's
`SHA256SUMS.txt` and replaces `playterm.exe` only if that file's Ed25519 signature matches the public key built into the app. Only versions newer than
the running one are installed (the version is part of the signed file name). The running exe is moved aside to `playterm.exe.old`, deleted at the next
start, and the new version takes effect when playterm restarts (**Restart now**, or later). After **Later**, **⬆ New version** at the right of the
menu bar opens the dialog again; **Skip this version** stops offering that version. An exe in a folder without write access (such as
`Program Files`) cannot be updated: download the new version from the releases page.
On macOS playterm does not replace the signed `playterm.app` itself: **Open the download page** opens the release page.

## Configuration files

`%APPDATA%\playterm\config\` (change it with the `PLAYTERM_CONFIG_DIR` environment variable — for portable use)

| File | Contents |
|---|---|
| `sessions.toml` | Saved sites and templates (no passwords) |
| `settings.toml` | Language, UI theme, font, size, color theme, scrollback, status bar, command input bar, sleep prevention, screen saver prevention, connection timeout, copy / paste keys, site manager view (tree/grid), ZMODEM download folder and asking for it, macros, update check / skipped version |
| `vault.toml` | Saved passwords, encrypted with the master password → Argon2id → XChaCha20-Poly1305. Each password is stored with the target it was saved for (protocol, user, host, port), also encrypted: if a site in `sessions.toml` is pointed elsewhere, its password is not sent (changes made in playterm carry it along). Several windows do not delete each other's saved passwords |
| `known_hosts` | SSH host keys. If a known host's key changes — even to a key of another type — the connection is refused (protection against man-in-the-middle attacks). Key types on record are asked for first |
| `known_rdp_hosts` | RDP server certificate fingerprints. A changed certificate is refused |

## Keyboard shortcuts

| Keys | Action |
|---|---|
| Ctrl+Shift+N | New tab (local command prompt) |
| Ctrl+Shift+T | Quick Connect |
| Ctrl+Shift+S | Site Manager |
| Ctrl+Shift+W / middle click | Close tab |
| Ctrl+Shift+D | Duplicate tab |
| Ctrl+Tab / Ctrl+Shift+Tab | Next / previous tab |
| Alt+→ / Alt+← | Next / previous tab |
| Ctrl+Shift+B | Show/hide the site list |
| Ctrl+Shift+F | Find (including scrollback) |
| Ctrl+Shift+E | SFTP explorer (side panel) on/off (same as Ctrl+Shift+X) |
| Ctrl+Shift+X | SFTP explorer (side panel) on/off |
| Ctrl+Shift+M | Command input bar (bottom) on/off |
| Ctrl+Shift+I | Typing into all tabs on/off |
| Drag / double-click / triple-click | Select characters / words / lines (Shift+click extends) |
| Shift+drag | Local selection even inside programs that use the mouse (vim `mouse=a`, htop, mc, tmux) |
| Ctrl+C | Copy if there is a selection, otherwise send ^C |
| Ctrl+Shift+C, Ctrl+Insert | Copy |
| Ctrl+Shift+V, Ctrl+V, Shift+Insert | Paste |
| (setting) drag to select / right or middle click | Copy on select / paste — Copy / paste in Tools → Settings (off by default) |
| F11 | Full screen |
| Ctrl+wheel, Ctrl+= / Ctrl+- / Ctrl+0 | Font larger / smaller / default size |

## Development

```powershell
cargo test --workspace                              # unit + SSH loopback integration tests
cargo run -p playterm-net --example dev_sshd        # test SSH server on 127.0.0.1:2222 (tester / secret)
cargo run -p playterm-net --example dev_sshd -- --authorized-keys key.pub --sftp-root C:\temp   # public-key auth / SFTP root
cargo run -p playterm -- ssh://tester@127.0.0.1:2222
```

## License

[MIT](LICENSE)

Except the button icon font `assets/icons/material-symbols-subset.ttf` (a subset of Google's Material Symbols Rounded), which is under the
[Apache License 2.0](assets/icons/LICENSE.txt), not MIT. It is also listed under **Help → Open-source licenses** in the app.
