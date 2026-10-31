//! Connection form shared by the quick-connect dialog and the session editor,
//! plus parsing of `ssh://` / `telnet://` URLs from the command line.

use std::path::PathBuf;

use eframe::egui;
use playterm_config::{AutoLogin, LoginStep, SessionProfile};
use playterm_net::{
    ForwardKind, ForwardRule, Protocol, SerialFlow, SerialParity, SerialPortEntry, SerialSettings, list_ports,
};
use playterm_term::Charset;
use zeroize::Zeroizing;

/// Parses `ssh://[user@]host[:port]` or `telnet://host[:port]` (`user@host` alone means SSH).
/// IPv6 hosts are written in brackets: `ssh://user@[::1]:2222`.
pub fn parse_url(url: &str) -> Result<SessionProfile, String> {
    let (protocol, rest) = match url.split_once("://") {
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "ssh" => (Protocol::Ssh, rest),
            "telnet" => (Protocol::Telnet, rest),
            "serial" | "com" => (Protocol::Serial, rest),
            other => return Err(format!("지원하지 않는 프로토콜: {other}")),
        },
        None => (Protocol::Ssh, url),
    };
    let rest = rest.trim_end_matches('/');
    if protocol == Protocol::Serial {
        let (port, baud) = match rest.split_once(':') {
            Some((port, baud)) => (port, baud.parse().ok().filter(|&b: &u32| b > 0).ok_or(format!("잘못된 속도: {baud}"))?),
            None => (rest, SerialSettings::default().baud),
        };
        if port.is_empty() {
            return Err(format!("시리얼 포트가 없습니다: {url}"));
        }
        let serial = SerialSettings { port: port.to_owned(), baud, ..Default::default() };
        return Ok(SessionProfile { protocol, host: port.to_owned(), port: 0, serial, ..Default::default() });
    }
    let (username, hostport) = match rest.rsplit_once('@') {
        Some((user, hp)) => (user.to_owned(), hp),
        None => (String::new(), rest),
    };

    let (host, port) = if let Some(after) = hostport.strip_prefix('[') {
        let (host, tail) = after.split_once(']').ok_or_else(|| format!("잘못된 주소: {url}"))?;
        (host, tail.strip_prefix(':'))
    } else {
        match hostport.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (hostport, None),
        }
    };
    if host.is_empty() {
        return Err(format!("호스트가 없습니다: {url}"));
    }
    let port = match port {
        Some(p) => p.parse().ok().filter(|&p: &u16| p > 0).ok_or_else(|| format!("잘못된 포트: {p}"))?,
        None => protocol.default_port(),
    };
    if protocol == Protocol::Ssh && username.is_empty() {
        return Err(format!("SSH 주소에는 사용자 이름이 필요합니다: {url}"));
    }

    Ok(SessionProfile { protocol, host: host.to_owned(), port, username, ..Default::default() })
}

/// Expands a leading `~` to the user's home directory.
pub fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix('~') {
        Some(rest) if rest.is_empty() || rest.starts_with(['/', '\\']) => {
            match directories::BaseDirs::new() {
                Some(dirs) => dirs.home_dir().join(rest.trim_start_matches(['/', '\\'])),
                None => PathBuf::from(path),
            }
        }
        _ => PathBuf::from(path),
    }
}

/// Native file dialog for choosing a private key, starting in `~/.ssh`.
fn pick_key_file(current: &str) -> Option<PathBuf> {
    let start = match current.trim() {
        "" => expand_home("~/.ssh"),
        c => expand_home(c).parent().map(PathBuf::from).unwrap_or_default(),
    };
    rfd::FileDialog::new()
        .set_title("SSH 개인 키 선택")
        .set_directory(start)
        .add_filter("모든 파일", &["*"])
        .add_filter("PuTTY 키 (*.ppk)", &["ppk"])
        .pick_file()
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

    ui.label("시리얼 포트");
    ui.horizontal(|ui| {
        let port = ui.add(egui::TextEdit::singleline(&mut s.port).hint_text("COM3").desired_width(90.0));
        if !*focus_requested {
            port.request_focus();
            *focus_requested = true;
        }
        submit |= enter(ui, &port);
        egui::ComboBox::from_id_salt("serial-ports").selected_text("목록").show_ui(ui, |ui| {
            if ports.is_empty() {
                ui.weak("COM 포트가 없습니다");
            }
            for entry in ports.iter() {
                let label = if entry.description.is_empty() {
                    entry.name.clone()
                } else {
                    format!("{}  {}", entry.name, entry.description)
                };
                if ui.selectable_label(s.port == entry.name, label).clicked() {
                    s.port = entry.name.clone();
                }
            }
        });
        if ui.button("새로고침").clicked() {
            *ports = list_ports();
        }
    });
    ui.end_row();

    ui.label("속도 (baud)");
    ui.horizontal(|ui| {
        let r = ui.add(egui::TextEdit::singleline(baud).desired_width(90.0));
        submit |= enter(ui, &r);
        ui.menu_button("선택", |ui| {
            for rate in SerialSettings::COMMON_BAUD_RATES {
                if ui.button(rate.to_string()).clicked() {
                    *baud = rate.to_string();
                    ui.close();
                }
            }
        });
    });
    ui.end_row();

    ui.label("데이터 비트");
    egui::ComboBox::from_id_salt("data-bits").selected_text(s.data_bits.to_string()).show_ui(ui, |ui| {
        for bits in [8u8, 7, 6, 5] {
            ui.selectable_value(&mut s.data_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label("패리티");
    egui::ComboBox::from_id_salt("parity").selected_text(s.parity.to_string()).show_ui(ui, |ui| {
        for parity in SerialParity::ALL {
            ui.selectable_value(&mut s.parity, parity, parity.to_string());
        }
    });
    ui.end_row();

    ui.label("정지 비트");
    egui::ComboBox::from_id_salt("stop-bits").selected_text(s.stop_bits.to_string()).show_ui(ui, |ui| {
        for bits in [1u8, 2] {
            ui.selectable_value(&mut s.stop_bits, bits, bits.to_string());
        }
    });
    ui.end_row();

    ui.label("흐름 제어");
    egui::ComboBox::from_id_salt("flow").selected_text(s.flow.to_string()).show_ui(ui, |ui| {
        for flow in SerialFlow::ALL {
            ui.selectable_value(&mut s.flow, flow, flow.to_string());
        }
    });
    ui.end_row();
    submit
}

fn log_ui(ui: &mut egui::Ui, log: &mut playterm_config::LogSettings) {
    egui::CollapsingHeader::new("세션 로그").id_salt("log-settings").default_open(log.auto).show(ui, |ui| {
        ui.checkbox(&mut log.auto, "접속하면 자동으로 로그 기록");
        ui.horizontal(|ui| {
            ui.radio_value(&mut log.raw, false, "텍스트 (제어 문자 제거)");
            ui.radio_value(&mut log.raw, true, "원본 (받은 바이트 그대로)");
        });
        ui.add_enabled(!log.raw, egui::Checkbox::new(&mut log.timestamps, "줄마다 시각 표시"));
        ui.horizontal(|ui| {
            ui.label("폴더");
            let default_dir = crate::logging::default_log_dir().display().to_string();
            ui.add(egui::TextEdit::singleline(&mut log.dir).hint_text(default_dir).desired_width(240.0));
            if ui.button("찾아보기").clicked()
                && let Some(dir) = rfd::FileDialog::new().set_title("로그 폴더").pick_folder()
            {
                log.dir = dir.display().to_string();
            }
        });
    });
}

/// Which optional parts of the form to show.
pub struct FormOptions<'a> {
    /// Name and folder fields (session editor).
    pub meta: bool,
    /// "Save password" checkbox (session editor).
    pub save_password: bool,
    /// Existing folders offered in the folder picker.
    pub folders: &'a [String],
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
            s.trim().parse::<u16>().ok().filter(|&p| p > 0).ok_or_else(|| format!("포워딩 {n}: {what} 포트가 잘못되었습니다"))
        };
        let mut rule = ForwardRule {
            kind: self.kind,
            bind_host: self.bind_host.trim().to_owned(),
            bind_port: port(&self.bind_port, "수신")?,
            ..Default::default()
        };
        if self.kind != ForwardKind::Dynamic {
            rule.dest_host = self.dest_host.trim().to_owned();
            if rule.dest_host.is_empty() {
                return Err(format!("포워딩 {n}: 대상 호스트를 입력하세요"));
            }
            rule.dest_port = port(&self.dest_port, "대상")?;
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
        !self.password.is_empty()
            || (&self.profile, &self.port, &self.baud, &self.forwards) != (profile, port, baud, forwards)
    }

    /// Draws the form. Returns `true` if Enter was pressed in one of its text fields.
    pub fn ui(&mut self, ui: &mut egui::Ui, opts: &FormOptions<'_>) -> bool {
        let mut submit = false;
        let enter = |ui: &egui::Ui, r: &egui::Response| r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let p = &mut self.profile;

        egui::Grid::new("profile-form").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
            if opts.meta {
                ui.label("이름");
                let name = ui.add(egui::TextEdit::singleline(&mut p.name).hint_text("비우면 user@host"));
                if !self.focus_requested {
                    name.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &name);
                ui.end_row();

                ui.label("폴더");
                ui.horizontal(|ui| {
                    let folder = ui.add(
                        egui::TextEdit::singleline(&mut p.folder).hint_text("없음").desired_width(150.0),
                    );
                    submit |= enter(ui, &folder);
                    let existing: Vec<&String> = opts.folders.iter().filter(|f| !f.is_empty()).collect();
                    if !existing.is_empty() {
                        ui.menu_button("선택", |ui| {
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
            }

            ui.label("프로토콜");
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
                ui.label("호스트");
                let host = ui.add(egui::TextEdit::singleline(&mut p.host).hint_text("example.com 또는 IP"));
                if !self.focus_requested {
                    host.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter(ui, &host);
                ui.end_row();

                ui.label("포트");
                let port = ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                submit |= enter(ui, &port);
                ui.end_row();
            }

            // Telnet and serial consoles log in at a prompt: the site manager keeps a user name
            // and password for the automatic login.
            let ssh = p.protocol == Protocol::Ssh;
            if ssh || opts.meta {
                ui.label("사용자");
                let hint = if ssh { "" } else { "자동 로그인의 {user}" };
                let user = ui.add(egui::TextEdit::singleline(&mut p.username).hint_text(hint));
                submit |= enter(ui, &user);
                ui.end_row();

                ui.label("비밀번호");
                let hint = if self.has_saved_password && p.save_password {
                    "저장된 비밀번호 유지 (바꾸려면 입력)"
                } else if ssh {
                    "비워두면 접속 시 물어봅니다"
                } else {
                    "자동 로그인의 {password}"
                };
                let pw = ui.add(egui::TextEdit::singleline(&mut *self.password).password(true).hint_text(hint));
                submit |= enter(ui, &pw);
                ui.end_row();

                if opts.save_password {
                    ui.label("");
                    ui.checkbox(&mut p.save_password, "비밀번호 저장 (마스터 비밀번호로 암호화)");
                    ui.end_row();
                }
            }

            if ssh {
                ui.label("키 파일");
                ui.horizontal(|ui| {
                    let key = ui.add(
                        egui::TextEdit::singleline(&mut p.identity_file)
                            .hint_text("없음 (에이전트·기본 키 사용)")
                            .desired_width(220.0),
                    );
                    submit |= enter(ui, &key);
                    if ui.button("찾아보기").clicked()
                        && let Some(path) = pick_key_file(&p.identity_file)
                    {
                        p.identity_file = path.display().to_string();
                    }
                });
                ui.end_row();

                ui.label("");
                ui.checkbox(&mut p.use_agent, "SSH 에이전트 사용 (Pageant / Windows OpenSSH)");
                ui.end_row();
            }

            ui.label("인코딩");
            egui::ComboBox::from_id_salt("charset").selected_text(p.charset.to_string()).show_ui(ui, |ui| {
                for c in Charset::ALL {
                    ui.selectable_value(&mut p.charset, c, c.to_string());
                }
            });
            ui.end_row();

            ui.label("터미널 타입");
            ui.text_edit_singleline(&mut p.term_type);
            ui.end_row();
        });

        if self.profile.protocol == Protocol::Ssh {
            ui.add_space(4.0);
            self.forwards_ui(ui);
        }
        if opts.meta {
            self.auto_login_ui(ui);
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
        let title = if a.enabled { format!("자동 로그인 ({}단계)", a.steps.len()) } else { "자동 로그인".to_owned() };
        egui::CollapsingHeader::new(title).id_salt("auto-login").default_open(a.enabled).show(ui, |ui| {
            if ui.checkbox(&mut a.enabled, "접속하면 프롬프트에 맞춰 자동으로 입력").changed() && a.enabled && a.steps.is_empty() {
                a.steps = AutoLogin::standard_steps();
            }
            ui.add_enabled_ui(a.enabled, |ui| {
                ui.weak("화면의 줄이 [기다릴 문자열] 로 끝나면 [보낼 내용] 과 Enter 를 입력합니다.\n{user}, {password} 는 위의 사용자 이름과 저장된 비밀번호로 바뀝니다.");
                let mut remove = None;
                egui::Grid::new("login-steps").num_columns(4).spacing([6.0, 4.0]).show(ui, |ui| {
                    ui.weak("");
                    ui.weak("기다릴 문자열");
                    ui.weak("보낼 내용");
                    ui.end_row();
                    for (i, step) in a.steps.iter_mut().enumerate() {
                        ui.label(format!("{}.", i + 1));
                        ui.add(egui::TextEdit::singleline(&mut step.expect).hint_text("login:").desired_width(150.0));
                        ui.add(egui::TextEdit::singleline(&mut step.send).hint_text("{user}").desired_width(170.0));
                        if ui.small_button("✕").on_hover_text("삭제").clicked() {
                            remove = Some(i);
                        }
                        ui.end_row();
                    }
                });
                if let Some(i) = remove {
                    a.steps.remove(i);
                }
                ui.horizontal(|ui| {
                    if ui.button("+ 단계 추가").clicked() {
                        a.steps.push(LoginStep::default());
                    }
                    if ui.button("기본값 (login: / Password:)").clicked() {
                        a.steps = AutoLogin::standard_steps();
                    }
                });
                if a.uses_password() && !save_password {
                    ui.colored_label(ui.visuals().warn_fg_color, "{password} 를 쓰려면 위에서 '비밀번호 저장' 을 켜세요");
                }
            });
        });
    }

    fn forwards_ui(&mut self, ui: &mut egui::Ui) {
        let title = format!("포트 포워딩 ({})", self.forwards.len());
        egui::CollapsingHeader::new(title).id_salt("forwards").default_open(!self.forwards.is_empty()).show(ui, |ui| {
            let mut remove = None;
            egui::Grid::new("forward-rows").num_columns(5).spacing([6.0, 4.0]).show(ui, |ui| {
                for (i, row) in self.forwards.iter_mut().enumerate() {
                    egui::ComboBox::from_id_salt(("fwd-kind", i))
                        .width(110.0)
                        .selected_text(row.kind.to_string())
                        .show_ui(ui, |ui| {
                            for k in ForwardKind::ALL {
                                ui.selectable_value(&mut row.kind, k, k.to_string());
                            }
                        });
                    ui.horizontal(|ui| {
                        let hint = if row.kind == ForwardKind::Remote { "(서버)" } else { "127.0.0.1" };
                        ui.add(egui::TextEdit::singleline(&mut row.bind_host).hint_text(hint).desired_width(80.0));
                        ui.label(":");
                        ui.add(egui::TextEdit::singleline(&mut row.bind_port).hint_text("포트").desired_width(45.0));
                    });
                    if row.kind == ForwardKind::Dynamic {
                        ui.label("SOCKS5");
                        ui.label("");
                    } else {
                        ui.label("→");
                        ui.horizontal(|ui| {
                            ui.add(egui::TextEdit::singleline(&mut row.dest_host).hint_text("호스트").desired_width(90.0));
                            ui.label(":");
                            ui.add(egui::TextEdit::singleline(&mut row.dest_port).hint_text("포트").desired_width(45.0));
                        });
                    }
                    if ui.small_button("✕").on_hover_text("삭제").clicked() {
                        remove = Some(i);
                    }
                    ui.end_row();
                }
            });
            if let Some(i) = remove {
                self.forwards.remove(i);
            }
            if ui.button("+ 포워딩 추가").clicked() {
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
        let password = (!self.password.is_empty()).then(|| self.password.clone());
        if p.protocol == Protocol::Serial {
            p.serial.port = p.serial.port.trim().to_uppercase();
            if p.serial.port.is_empty() {
                return Err("시리얼 포트를 입력하세요 (예: COM3)".into());
            }
            p.serial.baud =
                self.baud.trim().parse().ok().filter(|&b: &u32| b > 0).ok_or("속도(baud)는 양의 정수여야 합니다")?;
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
            return Err("호스트를 입력하세요".into());
        }
        p.port = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|&port: &u16| port > 0)
            .ok_or("포트는 1~65535 사이의 숫자여야 합니다")?;
        if p.term_type.trim().is_empty() {
            p.term_type = "xterm-256color".into();
        }
        match p.protocol {
            Protocol::Ssh => {
                if p.username.is_empty() {
                    return Err("SSH 사용자 이름을 입력하세요".into());
                }
                p.forwards =
                    self.forwards.iter().enumerate().map(|(i, r)| r.to_rule(i + 1)).collect::<Result<_, _>>()?;
            }
            Protocol::Serial => unreachable!("handled above"),
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
        return Err(format!("자동 로그인 {}단계: 기다릴 문자열을 입력하세요", n + 1));
    }
    if a.enabled && steps.is_empty() {
        return Err("자동 로그인 단계를 하나 이상 넣거나 자동 로그인을 끄세요".into());
    }
    Ok(AutoLogin { enabled: a.enabled, steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let r = parse_url("ssh://root@10.0.0.1:2222").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Ssh, "10.0.0.1", 2222, "root"));

        let r = parse_url("telnet://router").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Telnet, "router", 23));

        let r = parse_url("serial://COM3:115200").unwrap();
        assert_eq!((r.protocol, r.serial.port.as_str(), r.serial.baud), (Protocol::Serial, "COM3", 115200));
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
