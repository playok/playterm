//! Connection form shared by the quick-connect dialog and the session editor,
//! plus parsing of `ssh://` / `telnet://` URLs from the command line.

use std::path::PathBuf;

use eframe::egui;
use playterm_config::{AutoLogin, LoginStep, SessionProfile, TabColor};
use playterm_i18n::{t, tr};
use playterm_net::{
    ForwardKind, ForwardRule, KeepAlive, KeepAliveMode, Protocol, SerialFlow, SerialParity, SerialPortEntry, SerialSettings,
    decode_escapes, list_ports,
};
use playterm_term::Charset;
use zeroize::Zeroizing;

use crate::icon;

/// Parses `ssh://[user@]host[:port]` or `telnet://host[:port]` (`user@host` alone means SSH).
/// IPv6 hosts are written in brackets: `ssh://user@[::1]:2222`.
pub fn parse_url(url: &str) -> Result<SessionProfile, String> {
    let (protocol, rest) = match url.split_once("://") {
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "ssh" => (Protocol::Ssh, rest),
            "telnet" => (Protocol::Telnet, rest),
            "serial" | "com" => (Protocol::Serial, rest),
            "rdp" => (Protocol::Rdp, rest),
            other => return Err(tr!("지원하지 않는 프로토콜: {other}", "Unsupported protocol: {other}")),
        },
        None => (Protocol::Ssh, url),
    };
    let rest = rest.trim_end_matches('/');
    if protocol == Protocol::Serial {
        let (port, baud) = match rest.split_once(':') {
            Some((port, baud)) => {
                (port, baud.parse().ok().filter(|&b: &u32| b > 0).ok_or(tr!("잘못된 속도: {baud}", "Invalid speed: {baud}"))?)
            }
            None => (rest, SerialSettings::default().baud),
        };
        if port.is_empty() {
            return Err(tr!("시리얼 포트가 없습니다: {url}", "Missing serial port: {url}"));
        }
        let serial = SerialSettings { port: port.to_owned(), baud, ..Default::default() };
        return Ok(SessionProfile { protocol, host: port.to_owned(), port: 0, serial, ..Default::default() });
    }
    let (username, hostport) = match rest.rsplit_once('@') {
        Some((user, hp)) => (user.to_owned(), hp),
        None => (String::new(), rest),
    };

    let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
        let (host, tail) = after.split_once(']').ok_or_else(|| tr!("잘못된 주소: {url}", "Invalid address: {url}"))?;
        (host, tail.strip_prefix(':'))
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(tr!("호스트가 없습니다: {url}", "Missing host: {url}"));
    }
    let port = match port {
        Some(p) => p.parse().ok().filter(|&p: &u16| p > 0).ok_or_else(|| tr!("잘못된 포트: {p}", "Invalid port: {p}"))?,
        None => protocol.default_port(),
    };
    if protocol == Protocol::Ssh && username.is_empty() {
        return Err(tr!("SSH 주소에는 사용자 이름이 필요합니다: {url}", "An SSH address needs a user name: {url}"));
    }

    Ok(SessionProfile { protocol, host: host.to_owned(), port, username, ..Default::default() })
}

/// Expands a leading `~` to the user's home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => match directories::BaseDirs::new() {
            Some(dirs) => dirs.home_dir().join(rest.trim_start_matches(['/', '\\'])),
            None => PathBuf::from(path),
        },
        _ => PathBuf::from(path),
    }
}

/// Native file dialog for choosing a private key, starting in `~/.ssh`.
fn pick_key_file(current: &str) -> Option<PathBuf> {
    let start = match current.trim() {
        "" => expand_home("~/.ssh"),
        c => expand_home(c).parent().map(PathBuf::from).unwrap_or_default(),
    };
    crate::picker::Picker::new(crate::picker::Kind::File, t("SSH 개인 키 선택", "Choose an SSH private key"))
        .directory(start)
        .filter(t("모든 파일", "All files"), &["*"])
        .filter(t("PuTTY 키 (*.ppk)", "PuTTY keys (*.ppk)"), &["ppk"])
        .pick_one()
}

/// Serial line fields. Returns `true` if Enter was pressed in a text field.
fn serial_rows(
    ui: &mut egui::Ui,
    s: &mut SerialSettings,
    baud: &mut String,
    ports: &mut Vec<SerialPortEntry>,
    focus_requested: &mut bool,
) -> bool {
    let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let mut submit = false;

    ui.label(t("시리얼 포트", "Serial port"));
    ui.horizontal(|ui| {
        let port = ui.add(egui::TextEdit::singleline(&mut s.port).hint_text("COM3").desired_width(90.0));
        if !*focus_requested {
            port.request_focus();
            *focus_requested = true;
        }
        submit |= enter(ui, &port);
        egui::ComboBox::from_id_salt("serial-ports").selected_text(t("목록", "List")).show_ui(ui, |ui| {
            if ports.is_empty() {
                ui.weak(t("COM 포트가 없습니다", "No COM ports"));
            }
            for entry in ports.iter() {
                let label =
                    if entry.description.is_empty() { entry.name.clone() } else { format!("{}  {}", entry.name, entry.description) };
                if ui.selectable_label(s.port == entry.name, label).clicked() {
                    s.port = entry.name.clone();
                }
            }
        });
        if ui.add(icon::button(icon::REFRESH, t("새로고침", "Refresh"))).clicked() {
            *ports = list_ports();
        }
    });
    ui.end_row();

    ui.label(t("속도 (baud)", "Speed (baud)"));
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(baud).desired_width(90.0));
        submit |= enter(ui, &r);
        ui.menu_button(t("선택", "Choose"), |ui| {
            for rate in SerialSettings::COMMON_BAUD_RATES {
                if ui.button(rate.to_string()).clicked() {
                    *baud = rate.to_string();
                    ui.close();
                }
            }
        });
    });
    ui.end_row();

    ui.label(t("데이터 비트", "Data bits"));
    egui::ComboBox::from_id_salt("data-bits").selected_text(s.data_bits.to_string()).show_ui(ui, |ui| {
        for bits in [8u8, 7, 6, 5] {
            ui.selectable_value(&mut s.data_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label(t("패리티", "Parity"));
    egui::ComboBox::from_id_salt("parity").selected_text(s.parity.to_string()).show_ui(ui, |ui| {
        for parity in SerialParity::ALL {
            ui.selectable_value(&mut s.parity, parity, parity.to_string());
        }
    });
    ui.end_row();

    ui.label(t("정지 비트", "Stop bits"));
    egui::ComboBox::from_id_salt("stop-bits").selected_text(s.stop_bits.to_string()).show_ui(ui, |ui| {
        for bits in [1u8, 2] {
            ui.selectable_value(&mut s.stop_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label(t("흐름 제어", "Flow control"));
    egui::ComboBox::from_id_salt("flow").selected_text(s.flow.to_string()).show_ui(ui, |ui| {
        for flow in SerialFlow::ALL {
            ui.selectable_value(&mut s.flow, flow, flow.to_string());
        }
    });
    ui.end_row();
    submit
}

fn log_ui(ui: &mut egui::Ui, log: &mut playterm_config::LogSettings) {
    egui::CollapsingHeader::new(t("세션 로그", "Session log")).id_salt("log-settings").default_open(log.auto).show(ui, |ui| {
        ui.checkbox(&mut log.auto, t("접속하면 자동으로 로그 기록", "Start logging automatically on connect"));
        ui.horizontal(|ui| {
            ui.radio_value(&mut log.raw, false, t("텍스트 (제어 문자 제거)", "Text (control characters removed)"));
            ui.radio_value(&mut log.raw, true, t("원본 (받은 바이트 그대로)", "Raw (bytes as received)"));
        });
        ui.add_enabled(!log.raw, egui::Checkbox::new(&mut log.timestamps, t("줄마다 시각 표시", "Timestamp each line")));
        ui.horizontal(|ui| {
            ui.label(t("폴더", "Folder"));
            let default_dir = crate::logging::default_log_dir().display().to_string();
            ui.add(egui::TextEdit::singleline(&mut log.dir).hint_text(default_dir).desired_width(240.0));
            if ui.add(icon::button(icon::BROWSE, t("찾아보기", "Browse…"))).clicked()
                && let Some(dir) = crate::picker::Picker::new(crate::picker::Kind::Folder, t("로그 폴더", "Log folder")).pick_one()
            {
                log.dir = dir.display().to_string();
            }
        });
    });
}

fn size_label(size: (u16, u16)) -> String {
    if size.0 == 0 || size.1 == 0 {
        t("탭 크기에 맞춤 (창 크기를 바꾸면 따라감)", "Fit the tab (follows the window size)").into()
    } else {
        format!("{} × {}", size.0, size.1)
    }
}

/// Desktop size and clipboard of an RDP site.
fn rdp_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t("화면 크기", "Screen size"));
    let current = (rdp.width, rdp.height);
    egui::ComboBox::from_id_salt("rdp-size").width(260.0).selected_text(size_label(current)).show_ui(ui, |ui| {
        for size in playterm_net::RdpSettings::SIZES {
            if ui.selectable_label(current == size, size_label(size)).clicked() {
                (rdp.width, rdp.height) = size;
            }
        }
    });
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.clipboard, t("클립보드 공유 (복사·붙여넣기)", "Share the clipboard (copy and paste)"));
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.sound, t("원격 컴퓨터의 소리를 이 PC 에서 재생", "Play the remote computer's sound on this PC"));
    ui.end_row();

    // The device redirection backend is Windows-only.
    if cfg!(windows) {
        rdp_device_rows(ui, rdp);
    }

    rdp_gateway_rows(ui, rdp);
}

/// The RD Gateway to connect through, and its own user when it does not take the site's.
fn rdp_gateway_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t("RD 게이트웨이", "RD Gateway"));
    ui.add(
        egui::TextEdit::singleline(&mut rdp.gateway).hint_text(t("사용 안 함 (직접 연결)", "None (connect directly)")).desired_width(220.0),
    )
    .on_hover_text(t(
        "gateway.example.com 또는 gateway.example.com:8443 (HTTPS, 기본 443)",
        "gateway.example.com or gateway.example.com:8443 (HTTPS, 443 by default)",
    ));
    ui.end_row();

    ui.label(t("게이트웨이 사용자", "Gateway user"));
    ui.add_enabled(
        rdp.gateway().is_some(),
        egui::TextEdit::singleline(&mut rdp.gateway_user)
            .hint_text(t("사이트와 같은 사용자·비밀번호", "Same user and password as the site"))
            .desired_width(220.0),
    )
    .on_hover_text(t(
        "다른 계정으로 게이트웨이에 로그인할 때만 적습니다. 비밀번호는 연결할 때 묻습니다",
        "Only for logging on to the gateway with another account; its password is asked when connecting",
    ));
    ui.end_row();
}

/// The folder shown as a drive in the remote desktop, and the printer.
fn rdp_device_rows(ui: &mut egui::Ui, rdp: &mut playterm_net::RdpSettings) {
    ui.label(t("공유 폴더", "Shared folder"));
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut rdp.drive_folder).hint_text(t("공유 안 함", "Not shared")).desired_width(220.0))
            .on_hover_text(t("원격 데스크톱의 \"내 PC\" 에 드라이브로 보입니다", "Shown as a drive in \"This PC\" of the remote desktop"));
        if ui.add(icon::button(icon::BROWSE, t("찾아보기", "Browse…"))).clicked()
            && let Some(dir) = crate::picker::Picker::new(crate::picker::Kind::Folder, t("공유 폴더", "Shared folder")).pick_one()
        {
            rdp.drive_folder = dir.display().to_string();
        }
    });
    ui.end_row();

    ui.label("");
    ui.add_enabled(
        rdp.shared_folder().is_some(),
        egui::Checkbox::new(&mut rdp.drive_read_only, t("읽기 전용 (원격에서 바꿀 수 없음)", "Read-only (cannot be changed remotely)")),
    );
    ui.end_row();

    ui.label("");
    ui.checkbox(&mut rdp.printer, t("이 PC 의 기본 프린터로 인쇄", "Print on this PC's default printer")).on_hover_text(t(
        "원격 컴퓨터에 같은 이름의 프린터 드라이버가 있어야 프린터가 보입니다",
        "The printer appears only if the remote computer has a driver of the same name",
    ));
    ui.end_row();
}

/// Swatches to pick the site's tab color, "none" first.
fn tab_color_row(ui: &mut egui::Ui, color: &mut Option<TabColor>) {
    ui.horizontal(|ui| {
        ui.spacing_mut().item_spacing.x = 4.0;
        let none = ui.add(egui::Button::selectable(color.is_none(), t("없음", "None")));
        if none.clicked() {
            *color = None;
        }
        for c in TabColor::ALL {
            let fill = crate::menu::color32(c);
            let selected = *color == Some(c);
            let (rect, response) = ui.allocate_exact_size(egui::vec2(18.0, 18.0), egui::Sense::click());
            let painter = ui.painter();
            painter.rect_filled(rect.shrink(2.0), 3.0, fill);
            if selected || response.hovered() {
                let stroke = egui::Stroke::new(2.0, ui.visuals().strong_text_color());
                painter.rect_stroke(rect, 4.0, stroke, egui::StrokeKind::Inside);
            }
            if response.on_hover_text(c.name()).clicked() {
                *color = Some(c);
            }
        }
    });
}

fn keepalive_ui(ui: &mut egui::Ui, k: &mut KeepAlive, protocol: Protocol) {
    let mode_name = |m: KeepAliveMode| match m {
        KeepAliveMode::Off => t("끔", "Off"),
        KeepAliveMode::Protocol => t("프로토콜 신호", "Protocol keep-alive"),
        KeepAliveMode::Text => t("문자열 입력", "Type text"),
    };
    let title = match k.mode {
        KeepAliveMode::Off => t("접속 유지: 끔", "Keep-alive: off").to_owned(),
        m => tr!("접속 유지: {} · {}초", "Keep-alive: {} · {} s", mode_name(m), k.interval),
    };
    egui::CollapsingHeader::new(title).id_salt("keepalive").show(ui, |ui| {
        ui.horizontal(|ui| {
            for m in KeepAliveMode::ALL {
                ui.radio_value(&mut k.mode, m, mode_name(m));
            }
        });
        ui.weak(match k.mode {
            KeepAliveMode::Off => t("아무것도 보내지 않습니다.", "Nothing is sent."),
            KeepAliveMode::Protocol => t(
                "SSH keepalive / Telnet NOP 를 보냅니다. 방화벽·공유기·서버의 무통신 끊김을 막고, 끊긴 SSH 연결을 알아챕니다.\n장비의 입력 대기 시간 제한(exec-timeout 등)에는 '문자열 입력' 을 쓰세요.",
                "Sends SSH keepalives / Telnet NOP. Stops firewalls, routers and servers from dropping a quiet connection, and notices dead SSH connections.\nFor device input timeouts (exec-timeout etc.) use 'Type text'.",
            ),
            KeepAliveMode::Text => t(
                "아래 문자열을 입력한 것처럼 보내 장비·셸의 자동 로그아웃(exec-timeout, TMOUT)을 막습니다.\n\\r Enter, \\b 백스페이스, \\e Esc, \\xHH 바이트. 기본값 \\x20\\b (스페이스+백스페이스)는 치던 명령을 건드리지 않습니다.",
                "Types the text below to stop devices and shells from logging you out (exec-timeout, TMOUT).\n\\r Enter, \\b backspace, \\e Esc, \\xHH a byte. The default \\x20\\b (space + backspace) leaves a half-typed command alone.",
            ),
        });
        ui.add_enabled_ui(k.mode != KeepAliveMode::Off, |ui| {
            ui.horizontal(|ui| {
                ui.label(t("보내는 간격", "Interval"));
                ui.add(
                    egui::DragValue::new(&mut k.interval)
                        .range(KeepAlive::MIN_INTERVAL..=KeepAlive::MAX_INTERVAL)
                        .speed(1)
                        .suffix(t(" 초", " s")),
                );
                ui.weak(t("(이 시간 동안 보낸 것이 없을 때)", "(after this long with nothing sent)"));
            });
            if k.mode == KeepAliveMode::Text {
                ui.horizontal(|ui| {
                    ui.label(t("문자열", "Text"));
                    ui.add(egui::TextEdit::singleline(&mut k.text).hint_text(KeepAlive::DEFAULT_TEXT).desired_width(160.0));
                    if ui.add(icon::button(icon::RESET, t("기본값", "Default")).small()).clicked() {
                        k.text = KeepAlive::DEFAULT_TEXT.into();
                    }
                });
                if let Err(e) = decode_escapes(&k.text) {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
            }
            if protocol == Protocol::Serial && k.mode == KeepAliveMode::Protocol {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    t("시리얼에는 프로토콜 신호가 없습니다. '문자열 입력' 을 쓰세요", "Serial lines have no protocol keep-alive; use 'Type text'"),
                );
            }
        });
    });
}

fn validate_keepalive(k: &KeepAlive) -> Result<KeepAlive, String> {
    let mut k = k.clone();
    k.interval = k.interval.clamp(KeepAlive::MIN_INTERVAL, KeepAlive::MAX_INTERVAL);
    if k.mode == KeepAliveMode::Text {
        let bytes = decode_escapes(&k.text).map_err(|e| tr!("접속 유지 문자열: {e}", "Keep-alive text: {e}"))?;
        if bytes.is_empty() {
            return Err(t("접속 유지로 보낼 문자열을 입력하세요", "Enter the keep-alive text to send").into());
        }
    }
    Ok(k)
}

/// The macro a site runs once connected (and logged in automatically, if it does).
fn after_connect_row(ui: &mut egui::Ui, p: &mut SessionProfile, macros: &[String]) {
    ui.label(t("연결 후 매크로", "Macro after connecting"));
    let none = t("없음", "None");
    let current = p.after_connect_macro.clone();
    let shown = if current.is_empty() {
        none.to_owned()
    } else if macros.contains(&current) {
        current.clone()
    } else {
        tr!("{current} (없는 매크로)", "{current} (no such macro)")
    };
    egui::ComboBox::from_id_salt("after-connect-macro")
        .selected_text(shown)
        .show_ui(ui, |ui| {
            ui.selectable_value(&mut p.after_connect_macro, String::new(), none);
            for name in macros {
                ui.selectable_value(&mut p.after_connect_macro, name.clone(), name);
            }
        })
        .response
        .on_hover_text(t(
            "연결되고 자동 로그인이 끝나면 이 매크로를 실행합니다 (도구 → 매크로 → 매크로 관리 에서 만듭니다). 매크로의 {user}, {password} 는 이 사이트의 사용자와 비밀번호입니다.",
            "Runs this macro once connected and the automatic login is done (create macros in Tools → Macros → Manage macros). {user} and {password} in it are this site's user and password.",
        ));
    ui.end_row();
}

/// Which optional parts of the form to show.
/// The rows most connections leave as they are: key file and agent (SSH), the remote desktop
/// settings, encoding and terminal type. Returns whether Enter was pressed in one of them.
fn advanced_rows(ui: &mut egui::Ui, p: &mut SessionProfile) -> bool {
    let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    let ssh = p.protocol == Protocol::Ssh;
    let rdp = p.protocol == Protocol::Rdp;
    let mut submit = false;
    if ssh {
        ui.label(t("키 파일", "Key file"));
        ui.horizontal(|ui| {
            let key = ui.add(
                egui::TextEdit::singleline(&mut p.identity_file)
                    .hint_text(t("없음 (에이전트·기본 키 사용)", "None (agent and default keys)"))
                    .desired_width(220.0),
            );
            submit |= enter(ui, &key);
            if ui.add(icon::button(icon::BROWSE, t("찾아보기", "Browse…"))).clicked()
                && let Some(path) = pick_key_file(&p.identity_file)
            {
                p.identity_file = path.display().to_string();
            }
        });
        ui.end_row();

        ui.label(t("점프 호스트", "Jump host"));
        let jump = ui.add(
            egui::TextEdit::singleline(&mut p.jump_host)
                .hint_text(t("없음 · 사이트 이름 또는 user@host:port", "None · a site name or user@host:port"))
                .desired_width(260.0),
        )
        .on_hover_text(t(
            "이 서버를 거쳐 접속합니다 (OpenSSH 의 ProxyJump / -J). 저장된 사이트 이름을 쓰면 그 사이트의 키·저장된 비밀번호·점프 호스트를 씁니다.",
            "Connects through this server (OpenSSH ProxyJump / -J). With a saved site's name, that site's key, saved password and jump host are used.",
        ));
        submit |= enter(ui, &jump);
        ui.end_row();

        ui.label(t("X11 포워딩", "X11 forwarding"));
        ui.horizontal(|ui| {
            ui.checkbox(&mut p.x11_forwarding, "")
                .on_hover_text(t(
                    "서버의 X 프로그램(xterm, 설치 도구 등)을 이 PC 의 X 서버에 띄웁니다 (ssh -X). Windows: VcXsrv·Xming, macOS: XQuartz 를 먼저 실행하세요.",
                    "Shows X programs of the server (xterm, installers…) on the X server of this PC (ssh -X). Start VcXsrv or Xming (Windows) or XQuartz (macOS) first.",
                ));
            let hint = tr!("DISPLAY (기본 {})", "DISPLAY (default {})", playterm_net::x11::default_display());
            let display = ui.add_enabled(
                p.x11_forwarding,
                egui::TextEdit::singleline(&mut p.x11_display).hint_text(hint).desired_width(180.0),
            );
            submit |= enter(ui, &display);
            ui.add_enabled(p.x11_forwarding, egui::Checkbox::new(&mut p.x11_trusted, t("신뢰 (-Y)", "Trusted (-Y)")))
                .on_hover_text(t(
                    "켜면 서버의 X 프로그램이 이 PC 화면 전체와 키 입력까지 접근할 수 있습니다. 믿는 서버에만 켜세요. 끄면(-X) 제한된(untrusted) 권한으로, 만들 수 없으면 X 연결을 막습니다 (xauth 와 SECURITY 확장을 지원하는 X 서버 필요).",
                    "On, the server's X programs can reach the whole display of this PC, keyboard included: only for servers you trust. Off (-X), they get limited (untrusted) access, or none when it cannot be set up (needs xauth and an X server with the SECURITY extension).",
                ));
        });
        ui.end_row();

        ui.label("");
        ui.checkbox(&mut p.use_agent, tr!("SSH 에이전트 사용 ({})", "Use the SSH agent ({})", crate::platform::ssh_agents()));
        ui.end_row();
    }

    if rdp {
        rdp_rows(ui, &mut p.rdp);
    } else {
        ui.label(t("인코딩", "Encoding"));
        egui::ComboBox::from_id_salt("charset").selected_text(p.charset.to_string()).show_ui(ui, |ui| {
            for c in Charset::ALL {
                ui.selectable_value(&mut p.charset, c, c.to_string());
            }
        });
        ui.end_row();

        ui.label(t("터미널 타입", "Terminal type"));
        ui.text_edit_singleline(&mut p.term_type);
        ui.end_row();
    }
    submit
}

/// What `user@host:port` typed or pasted into the host field holds. A bare IPv6 address
/// (`fe80::1`) is a host, not host and port; `[fe80::1]:2222` has a port.
#[derive(Debug, PartialEq, Eq)]
pub struct Address {
    pub user: Option<String>,
    pub host: String,
    pub port: Option<u16>,
}

pub fn split_address(text: &str) -> Address {
    let text = text.trim();
    let (user, rest) = match text.rsplit_once('@') {
        Some((u, h)) if !u.is_empty() => (Some(u.to_owned()), h),
        _ => (None, text),
    };
    let (host, port) = if let Some(inner) = rest.strip_prefix('[') {
        // [v6] or [v6]:port
        match inner.split_once(']') {
            Some((h, after)) => (h.to_owned(), after.strip_prefix(':').and_then(|p| p.parse().ok())),
            None => (rest.to_owned(), None),
        }
    } else if rest.matches(':').count() == 1 {
        let (h, p) = rest.split_once(':').unwrap_or((rest, ""));
        match p.parse::<u16>() {
            Ok(port) if port > 0 => (h.to_owned(), Some(port)),
            _ => (rest.to_owned(), None),
        }
    } else {
        (rest.to_owned(), None)
    };
    Address { user, host, port }
}

pub struct FormOptions<'a> {
    /// Name and folder fields (session editor).
    pub meta: bool,
    /// "Save password" checkbox (session editor).
    pub save_password: bool,
    /// Existing folders offered in the folder picker.
    pub folders: &'a [String],
    /// Names of the macros a site can run once connected.
    pub macros: &'a [String],
}

/// Editable text form of a [`ForwardRule`] (ports stay strings while typing).
#[derive(Clone, PartialEq)]
struct ForwardRow {
    kind: ForwardKind,
    bind_host: String,
    bind_port: String,
    dest_host: String,
    dest_port: String,
}

impl ForwardRow {
    fn from_rule(r: &ForwardRule) -> Self {
        Self {
            kind: r.kind,
            bind_host: r.bind_host.clone(),
            bind_port: r.bind_port.to_string(),
            dest_host: r.dest_host.clone(),
            dest_port: if r.kind == ForwardKind::Dynamic { String::new() } else { r.dest_port.to_string() },
        }
    }

    fn to_rule(&self, n: usize) -> Result<ForwardRule, String> {
        let port = |s: &str, what: &str| {
            s.trim()
                .parse::<u16>()
                .ok()
                .filter(|&p| p > 0)
                .ok_or_else(|| tr!("포워딩 {n}: {what} 포트가 잘못되었습니다", "Port forwarding {n}: invalid {what} port"))
        };
        let mut rule = ForwardRule {
            kind: self.kind,
            bind_host: self.bind_host.trim().to_owned(),
            bind_port: port(&self.bind_port, t("수신", "listening"))?,
            ..Default::default()
        };
        if self.kind != ForwardKind::Dynamic {
            rule.dest_host = self.dest_host.trim().to_owned();
            if rule.dest_host.is_empty() {
                return Err(tr!("포워딩 {n}: 대상 호스트를 입력하세요", "Port forwarding {n}: enter the destination host"));
            }
            rule.dest_port = port(&self.dest_port, t("대상", "destination"))?;
        }
        Ok(rule)
    }
}

pub struct ProfileForm {
    pub profile: SessionProfile,
    port: String,
    forwards: Vec<ForwardRow>,
    password: Zeroizing<String>,
    /// The vault already holds a password for this profile.
    has_saved_password: bool,
    baud: String,
    /// COM ports found on this machine; scanned when the serial fields are first shown.
    serial_ports: Option<Vec<SerialPortEntry>>,
    error: Option<String>,
    focus_requested: bool,
    /// Editable state when the form was opened, to detect unsaved changes.
    initial: (SessionProfile, String, String, Vec<ForwardRow>),
}

impl ProfileForm {
    pub fn new(profile: SessionProfile, has_saved_password: bool) -> Self {
        let port = profile.port.to_string();
        let baud = profile.serial.baud.to_string();
        let forwards: Vec<ForwardRow> = profile.forwards.iter().map(ForwardRow::from_rule).collect();
        Self {
            initial: (profile.clone(), port.clone(), baud.clone(), forwards.clone()),
            port,
            forwards,
            profile,
            password: Zeroizing::default(),
            has_saved_password,
            baud,
            serial_ports: None,
            error: None,
            focus_requested: false,
        }
    }

    /// The profile as it was when the form was opened.
    pub fn initial_profile(&self) -> &SessionProfile {
        &self.initial.0
    }

    /// Whether anything was changed since the form was opened.
    pub fn is_dirty(&self) -> bool {
        let (profile, port, baud, forwards) = &self.initial;
        !self.password.is_empty() || (&self.profile, &self.port, &self.baud, &self.forwards) != (profile, port, baud, forwards)
    }

    /// Draws the form. Returns `true` if Enter was pressed in one of its text fields.
    pub fn ui(&mut self, ui: &mut egui::Ui, opts: &FormOptions<'_>) -> bool {
        let mut submit = false;
        let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let p = &mut self.profile;

        egui::Grid::new("profile-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            if opts.meta {
                ui.label(t("이름", "Name"));
                let name = ui.add(egui::TextEdit::singleline(&mut p.name).hint_text(t("비우면 user@host", "user@host if empty")));
                if !self.focus_requested {
                    name.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &name);
                ui.end_row();

                ui.label(t("폴더", "Folder"));
                ui.horizontal(|ui| {
                    let folder = ui.add(egui::TextEdit::singleline(&mut p.folder).hint_text(t("없음", "None")).desired_width(150.0));
                    submit |= enter(ui, &folder);
                    let existing: Vec<&String> = opts.folders.iter().filter(|f| !f.is_empty()).collect();
                    if !existing.is_empty() {
                        ui.menu_button(t("선택", "Choose"), |ui| {
                            for f in existing {
                                if ui.button(f).clicked() {
                                    p.folder = f.clone();
                                    ui.close();
                                }
                            }
                        });
                    }
                });
                ui.end_row();

                ui.label(t("탭 색상", "Tab color"));
                ui.horizontal(|ui| {
                    tab_color_row(ui, &mut p.tab_color);
                    ui.add(
                        egui::TextEdit::singleline(&mut p.tab_label)
                            .hint_text(t("라벨 (예: 운영)", "Label (e.g. PROD)"))
                            .char_limit(12)
                            .desired_width(110.0),
                    )
                    .on_hover_text(t(
                        "탭, 터미널 위 연결 줄, 창 제목에 함께 보입니다",
                        "Shown with the tab, the connection line and the window title",
                    ));
                });
                ui.end_row();
            }

            ui.label(t("프로토콜", "Protocol"));
            ui.horizontal(|ui| {
                for proto in Protocol::ALL {
                    let old = p.protocol;
                    if ui.radio_value(&mut p.protocol, proto, proto.to_string()).changed() {
                        // Focus the first field of the newly shown fields (host or COM port).
                        self.focus_requested = opts.meta;
                        if self.port == old.default_port().to_string() {
                            self.port = proto.default_port().to_string();
                        }
                    }
                }
            });
            ui.end_row();

            if p.protocol == Protocol::Serial {
                let ports = self.serial_ports.get_or_insert_with(list_ports);
                submit |= serial_rows(ui, &mut p.serial, &mut self.baud, ports, &mut self.focus_requested);
            } else {
                ui.label(t("호스트", "Host"));
                let host = ui.add(
                    egui::TextEdit::singleline(&mut p.host)
                        .hint_text(t("example.com, IP 또는 user@host:port", "example.com, IP or user@host:port")),
                );
                if host.changed() {
                    // user@host:port pasted or typed: fill in the user and port fields.
                    let address = split_address(&p.host);
                    if let Some(user) = address.user {
                        p.username = user;
                    }
                    if let Some(port) = address.port {
                        self.port = port.to_string();
                    }
                    if address.host != p.host {
                        p.host = address.host;
                    }
                }
                if !self.focus_requested {
                    host.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &host);
                ui.end_row();

                ui.label(t("포트", "Port"));
                let port = ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                submit |= enter(ui, &port);
                ui.end_row();
            }

            // Telnet and serial consoles log in at a prompt: the site manager keeps a user name
            // and password for the automatic login.
            let ssh = p.protocol == Protocol::Ssh;
            let rdp = p.protocol == Protocol::Rdp;
            if ssh || rdp || opts.meta {
                ui.label(t("사용자", "User"));
                let hint = if ssh {
                    ""
                } else if rdp {
                    t(r"사용자 또는 DOMAIN\사용자", r"user or DOMAIN\user")
                } else {
                    t("자동 로그인의 {user}", "{user} in the automatic login")
                };
                let user = ui.add(egui::TextEdit::singleline(&mut p.username).hint_text(hint));
                submit |= enter(ui, &user);
                ui.end_row();

                ui.label(t("비밀번호", "Password"));
                let hint = if self.has_saved_password && p.save_password {
                    t("저장된 비밀번호 유지 (바꾸려면 입력)", "Saved password kept (type to change)")
                } else if ssh || rdp {
                    t("비워두면 접속 시 물어봅니다", "Asked on connect if empty")
                } else {
                    t("자동 로그인의 {password}", "{password} in the automatic login")
                };
                let pw = ui.add(egui::TextEdit::singleline(&mut *self.password).password(true).hint_text(hint));
                crate::dialogs::secret_field(ui, &pw);
                submit |= enter(ui, &pw);
                ui.end_row();

                if opts.save_password {
                    ui.label("");
                    ui.checkbox(
                        &mut p.save_password,
                        t("비밀번호 저장 (마스터 비밀번호로 암호화)", "Save password (encrypted with the master password)"),
                    );
                    ui.end_row();
                }
            }

            if opts.meta {
                submit |= advanced_rows(ui, p);
                if p.protocol != Protocol::Rdp {
                    after_connect_row(ui, p, opts.macros);
                }
            }
        });

        if opts.meta {
            if self.profile.protocol == Protocol::Ssh {
                ui.add_space(4.0);
                self.forwards_ui(ui);
            }
        } else {
            // Quick connect: what most connections leave as it is, folded away.
            ui.add_space(4.0);
            egui::CollapsingHeader::new(t("고급 설정", "Advanced")).id_salt("quick-advanced").show(ui, |ui| {
                egui::Grid::new("profile-form-advanced").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    submit |= advanced_rows(ui, &mut self.profile);
                });
                if self.profile.protocol == Protocol::Ssh {
                    ui.add_space(4.0);
                    self.forwards_ui(ui);
                }
            });
        }
        // A remote desktop has no text to log or prompts to answer.
        if opts.meta && self.profile.protocol != Protocol::Rdp {
            self.auto_login_ui(ui);
            keepalive_ui(ui, &mut self.profile.keepalive, self.profile.protocol);
            log_ui(ui, &mut self.profile.log);
        }

        if let Some(err) = &self.error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        submit
    }

    fn auto_login_ui(&mut self, ui: &mut egui::Ui) {
        let save_password = self.profile.save_password;
        let a = &mut self.profile.auto_login;
        let title = if a.enabled {
            tr!("자동 로그인 ({}단계)", "Automatic login ({} steps)", a.steps.len())
        } else {
            t("자동 로그인", "Automatic login").to_owned()
        };
        egui::CollapsingHeader::new(title).id_salt("auto-login").default_open(a.enabled).show(ui, |ui| {
            let label = t("접속하면 프롬프트에 맞춰 자동으로 입력", "Type at the prompts automatically on connect");
            if ui.checkbox(&mut a.enabled, label).changed() && a.enabled && a.steps.is_empty() {
                a.steps = AutoLogin::standard_steps();
            }
            ui.add_enabled_ui(a.enabled, |ui| {
                ui.weak(t(
                    "화면의 줄이 [기다릴 문자열] 로 끝나면 [보낼 내용] 과 Enter 를 입력합니다.\n{user}, {password} 는 위의 사용자 이름과 저장된 비밀번호로 바뀝니다.",
                    "When a screen line ends with [Wait for], types [Send] and Enter.\n{user} and {password} become the user name and saved password above.",
                ));
                let mut remove = None;
                egui::Grid::new("login-steps").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                    ui.weak("");
                    ui.weak(t("기다릴 문자열", "Wait for"));
                    ui.weak(t("보낼 내용", "Send"));
                    ui.end_row();
                    for (i, step) in a.steps.iter_mut().enumerate() {
                        ui.label(format!("{}.", i + 1));
                        ui.add(egui::TextEdit::singleline(&mut step.expect).hint_text("login:").desired_width(150.0));
                        ui.add(egui::TextEdit::singleline(&mut step.send).hint_text("{user}").desired_width(170.0));
                        if ui.add(icon::icon_button(icon::DELETE).small()).on_hover_text(t("삭제", "Delete")).clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    a.steps.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui.add(icon::button(icon::ADD, t("단계 추가", "Add step"))).clicked() {
                        a.steps.push(LoginStep::default());
                    }
                    if ui.add(icon::button(icon::RESET, t("기본값 (login: / Password:)", "Defaults (login: / Password:)"))).clicked() {
                        a.steps = AutoLogin::standard_steps();
                    }
                });
                if a.uses_password() && !save_password {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        t(
                            "{password} 를 쓰려면 위에서 '비밀번호 저장' 을 켜세요",
                            "Turn on 'Save password' above to use {password}",
                        ),
                    );
                }
            });
        });
    }

    fn forwards_ui(&mut self, ui: &mut egui::Ui) {
        let title = tr!("포트 포워딩 ({})", "Port forwarding ({})", self.forwards.len());
        egui::CollapsingHeader::new(title).id_salt("forwards").default_open(!self.forwards.is_empty()).show(ui, |ui| {
            let mut remove = None;
            egui::Grid::new("forward-rows").num_columns(5).spacing([6.0, 4.0]).show(ui, |ui| {
                for (i, row) in self.forwards.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("fwd-kind", i)).width(110.0).selected_text(row.kind.to_string()).show_ui(ui, |ui| {
                        for k in ForwardKind::ALL {
                            ui.selectable_value(&mut row.kind, k, k.to_string());
                        }
                    });
                    ui.horizontal(|ui| {
                        let hint = if row.kind == ForwardKind::Remote { t("(서버)", "(server)") } else { "127.0.0.1" };
                        ui.add(egui::TextEdit::singleline(&mut row.bind_host).hint_text(hint).desired_width(80.0));
                        ui.label(":");
                        ui.add(egui::TextEdit::singleline(&mut row.bind_port).hint_text(t("포트", "port")).desired_width(45.0));
                    });
                    if row.kind == ForwardKind::Dynamic {
                        ui.label("SOCKS5");
                        ui.label("");
                    } else {
                        ui.label("→");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut row.dest_host).hint_text(t("호스트", "host")).desired_width(90.0));
                            ui.label(":");
                            ui.add(egui::TextEdit::singleline(&mut row.dest_port).hint_text(t("포트", "port")).desired_width(45.0));
                        });
                    }
                    if ui.add(icon::icon_button(icon::DELETE).small()).on_hover_text(t("삭제", "Delete")).clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                self.forwards.remove(i);
            }
            if ui.add(icon::button(icon::ADD, t("포워딩 추가", "Add forwarding"))).clicked() {
                self.forwards.push(ForwardRow {
                    kind: ForwardKind::Local,
                    bind_host: String::new(),
                    bind_port: String::new(),
                    dest_host: "localhost".into(),
                    dest_port: String::new(),
                });
            }
        });
    }

    /// Checks the input. On success returns the profile and the typed password, if any.
    pub fn validate(&mut self) -> Option<(SessionProfile, Option<Zeroizing<String>>)> {
        match self.try_validate() {
            Ok(v) => Some(v),
            Err(e) => {
                self.error = Some(e);
                None
            }
        }
    }

    fn try_validate(&self) -> Result<(SessionProfile, Option<Zeroizing<String>>), String> {
        let mut p = self.profile.clone();
        p.name = p.name.trim().to_owned();
        p.folder = p.folder.trim().to_owned();
        p.host = p.host.trim().to_owned();
        p.username = p.username.trim().to_owned();
        p.identity_file = p.identity_file.trim().trim_matches('"').to_owned();
        p.log.dir = p.log.dir.trim().trim_matches('"').to_owned();
        p.auto_login = validate_auto_login(&p.auto_login)?;
        p.keepalive = validate_keepalive(&p.keepalive)?;
        let password = (!self.password.is_empty()).then(|| self.password.clone());
        if p.protocol == Protocol::Serial {
            p.serial.port = p.serial.port.trim().to_uppercase();
            if p.serial.port.is_empty() {
                return Err(t("시리얼 포트를 입력하세요 (예: COM3)", "Enter a serial port (e.g. COM3)").into());
            }
            p.serial.baud = self
                .baud
                .trim()
                .parse()
                .ok()
                .filter(|&b: &u32| b > 0)
                .ok_or(t("속도(baud)는 양의 정수여야 합니다", "The speed (baud) must be a positive integer"))?;
            p.host = p.serial.port.clone();
            p.port = 0;
            p.identity_file.clear();
            p.forwards.clear();
            if p.term_type.trim().is_empty() {
                p.term_type = "xterm-256color".into();
            }
            // The user name and password are for the automatic login at the console prompt.
            return Ok((p, password));
        }
        if p.host.is_empty() {
            return Err(t("호스트를 입력하세요", "Enter a host").into());
        }
        p.port = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|&port: &u16| port > 0)
            .ok_or(t("포트는 1~65535 사이의 숫자여야 합니다", "The port must be a number from 1 to 65535"))?;
        if p.term_type.trim().is_empty() {
            p.term_type = "xterm-256color".into();
        }
        match p.protocol {
            Protocol::Ssh => {
                if p.username.is_empty() {
                    return Err(t("SSH 사용자 이름을 입력하세요", "Enter the SSH user name").into());
                }
                p.forwards = self.forwards.iter().enumerate().map(|(i, r)| r.to_rule(i + 1)).collect::<Result<_, _>>()?;
            }
            Protocol::Serial => unreachable!("handled above"),
            Protocol::Rdp => {
                p.identity_file.clear();
                p.forwards.clear();
            }
            Protocol::Telnet => {
                // The user name and password are for the automatic login at the remote prompt.
                p.identity_file.clear();
                p.forwards.clear();
            }
        }
        Ok((p, password))
    }
}

/// Drops empty rows; a step that types something must say what to wait for.
fn validate_auto_login(a: &AutoLogin) -> Result<AutoLogin, String> {
    let steps: Vec<LoginStep> = a
        .steps
        .iter()
        .map(|s| LoginStep { expect: s.expect.trim().to_owned(), send: s.send.clone() })
        .filter(|s| !s.expect.is_empty() || !s.send.trim().is_empty())
        .collect();
    if let Some(n) = steps.iter().position(|s| s.expect.is_empty()) {
        return Err(tr!("자동 로그인 {}단계: 기다릴 문자열을 입력하세요", "Automatic login step {}: enter the text to wait for", n + 1));
    }
    if a.enabled && steps.is_empty() {
        return Err(t(
            "자동 로그인 단계를 하나 이상 넣거나 자동 로그인을 끄세요",
            "Add at least one automatic login step or turn automatic login off",
        )
        .into());
    }
    Ok(AutoLogin { enabled: a.enabled, steps })
}

#[cfg(test)]
mod tests {

    #[test]
    fn splits_pasted_addresses() {
        let a = |user: Option<&str>, host: &str, port: Option<u16>| Address { user: user.map(str::to_owned), host: host.to_owned(), port };
        assert_eq!(split_address("web01"), a(None, "web01", None));
        assert_eq!(split_address(" deploy@web01 "), a(Some("deploy"), "web01", None));
        assert_eq!(split_address("web01:2222"), a(None, "web01", Some(2222)));
        assert_eq!(split_address("deploy@10.0.0.5:2222"), a(Some("deploy"), "10.0.0.5", Some(2222)));
        assert_eq!(split_address("fe80::1"), a(None, "fe80::1", None), "bare IPv6: no port");
        assert_eq!(split_address("root@[fe80::1]:2222"), a(Some("root"), "fe80::1", Some(2222)));
        assert_eq!(split_address("[::1]"), a(None, "::1", None));
        assert_eq!(split_address("host:abc"), a(None, "host:abc", None), "not a port: left as typed");
        assert_eq!(split_address("@host"), a(None, "@host", None));
    }
    use super::*;

    #[test]
    fn parses_urls() {
        let r = parse_url("ssh://root@10.0.0.1:2222").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Ssh, "10.0.0.1", 2222, "root"));

        let r = parse_url("telnet://router").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Telnet, "router", 23));

        let r = parse_url("serial://COM3:115200").unwrap();
        assert_eq!((r.protocol, r.serial.port.as_str(), r.serial.baud), (Protocol::Serial, "COM3", 115200));
        let r = parse_url("rdp://admin@win01").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Rdp, "win01", 3389, "admin"));
        assert_eq!(parse_url("rdp://10.0.0.9:3390").unwrap().port, 3390);
        assert_eq!(parse_url("serial://COM1").unwrap().serial.baud, 9600);
        assert!(parse_url("serial://COM1:fast").is_err());

        let r = parse_url("admin@[::1]").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Ssh, "::1", 22));
    }

    #[test]
    fn rejects_bad_urls() {
        assert!(parse_url("ssh://host").is_err());
        assert!(parse_url("ftp://host").is_err());
        assert!(parse_url("telnet://host:0").is_err());
        assert!(parse_url("telnet://:23").is_err());
    }

    #[test]
    fn home_expansion() {
        let home = directories::BaseDirs::new().unwrap().home_dir().to_owned();
        assert_eq!(expand_home("~/.ssh/id_rsa"), home.join(".ssh").join("id_rsa"));
        assert_eq!(expand_home("~"), home);
        assert_eq!(expand_home(r"C:\keys.ppk"), PathBuf::from(r"C:\keys.ppk"));
        assert_eq!(expand_home("~other/x"), PathBuf::from("~other/x"));
    }

    #[test]
    fn validation() {
        let mut form = ProfileForm::new(SessionProfile { host: " web ".into(), ..Default::default() }, false);
        assert!(form.validate().is_none(), "SSH needs a user name");

        form.profile.username = "me".into();
        *form.password = "pw".into();
        let (p, pw) = form.validate().unwrap();
        assert_eq!((p.host.as_str(), p.port), ("web", 22));
        assert_eq!(pw.as_deref().map(|s| s.as_str()), Some("pw"));

        form.profile.protocol = Protocol::Telnet;
        form.profile.save_password = true;
        form.profile.identity_file = "key".into();
        let (p, pw) = form.validate().unwrap();
        // Telnet keeps the user name and password for the automatic login; keys are SSH only.
        assert_eq!(pw.as_deref().map(|s| s.as_str()), Some("pw"));
        assert!(p.save_password && p.username == "me" && p.identity_file.is_empty());

        form.port = "70000".into();
        assert!(form.validate().is_none());
    }

    #[test]
    fn keepalive_validation() {
        let k = |mode, interval, text: &str| KeepAlive { mode, interval, text: text.into() };
        assert_eq!(validate_keepalive(&k(KeepAliveMode::Protocol, 1, "")).unwrap().interval, KeepAlive::MIN_INTERVAL);
        assert_eq!(validate_keepalive(&k(KeepAliveMode::Protocol, 99999, "")).unwrap().interval, KeepAlive::MAX_INTERVAL);
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "\\r")).is_ok());
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "")).is_err());
        assert!(validate_keepalive(&k(KeepAliveMode::Text, 60, "\\x1")).is_err());
        assert!(validate_keepalive(&k(KeepAliveMode::Off, 60, "\\x1")).is_ok(), "the text only matters when used");
    }

    #[test]
    fn dirty_tracking() {
        let profile = SessionProfile { host: "h".into(), username: "u".into(), ..Default::default() };
        let mut form = ProfileForm::new(profile, false);
        assert!(!form.is_dirty());
        form.port = "2222".into();
        assert!(form.is_dirty());
        form.port = "22".into();
        assert!(!form.is_dirty(), "changing a field back is not a change");
        *form.password = "pw".into();
        assert!(form.is_dirty(), "a typed password is a change");
        form.password.clear();
        form.profile.protocol = Protocol::Telnet;
        assert!(form.is_dirty());
    }

    #[test]
    fn serial_validation() {
        let profile = SessionProfile {
            protocol: Protocol::Serial,
            username: "root".into(),
            serial: SerialSettings { port: " com4 ".into(), ..Default::default() },
            ..Default::default()
        };
        let mut form = ProfileForm::new(profile, false);
        form.baud = "115200".into();
        let (p, pw) = form.validate().unwrap();
        assert_eq!((p.serial.port.as_str(), p.serial.baud, p.host.as_str()), ("COM4", 115200, "COM4"));
        assert!(pw.is_none() && p.username == "root", "kept for the console login");

        form.baud = "0".into();
        assert!(form.validate().is_none());
        form.baud = "9600".into();
        form.profile.serial.port.clear();
        assert!(form.validate().is_none());
    }

    #[test]
    fn auto_login_validation() {
        let step = |e: &str, s: &str| LoginStep { expect: e.into(), send: s.into() };
        let mut a = AutoLogin { enabled: true, steps: vec![step(" login: ", "{user}"), step("", "  "), step("Password:", "{password}")] };
        let v = validate_auto_login(&a).unwrap();
        assert_eq!(v.steps, vec![step("login:", "{user}"), step("Password:", "{password}")], "trimmed, blank row dropped");

        a.steps.push(step("", "show run"));
        assert!(validate_auto_login(&a).unwrap_err().contains("3단계"));

        let empty = AutoLogin { enabled: true, steps: vec![] };
        assert!(validate_auto_login(&empty).is_err());
        assert!(validate_auto_login(&AutoLogin::default()).is_ok(), "off and empty is fine");
    }

    #[test]
    fn forward_rows_validate() {
        let rule = ForwardRule::parse(ForwardKind::Local, "8080:db:5432").unwrap();
        let profile = SessionProfile {
            host: "h".into(),
            username: "u".into(),
            forwards: vec![rule.clone(), ForwardRule::parse(ForwardKind::Dynamic, "1080").unwrap()],
            ..Default::default()
        };
        let mut form = ProfileForm::new(profile, false);
        assert_eq!(form.validate().unwrap().0.forwards[0], rule);

        form.forwards[0].dest_port = "x".into();
        assert!(form.validate().is_none());
        form.forwards[0].dest_port = "5432".into();
        form.forwards[0].dest_host = " ".into();
        assert!(form.validate().is_none());
    }
}
