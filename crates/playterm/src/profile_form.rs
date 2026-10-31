//! Connection form shared by the quick-connect dialog and the session editor,
//! plus parsing of `ssh://` / `telnet://` URLs from the command line.

use eframe::egui;
use playterm_config::SessionProfile;
use playterm_net::Protocol;
use playterm_term::Charset;
use zeroize::Zeroizing;

/// Parses `ssh://[user@]host[:port]` or `telnet://host[:port]` (`user@host` alone means SSH).
/// IPv6 hosts are written in brackets: `ssh://user@[::1]:2222`.
pub fn parse_url(url: &str) -> Result<SessionProfile, String> {
    let (protocol, rest) = match url.split_once("://") {
        Some((scheme, rest)) => match scheme.to_ascii_lowercase().as_str() {
            "ssh" => (Protocol::Ssh, rest),
            "telnet" => (Protocol::Telnet, rest),
            other => return Err(format!("지원하지 않는 프로토콜: {other}")),
        },
        None => (Protocol::Ssh, url),
    };
    let rest = rest.trim_end_matches('/');
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

/// Which optional parts of the form to show.
pub struct FormOptions<'a> {
    /// Name and folder fields (session editor).
    pub meta: bool,
    /// "Save password" checkbox (session editor).
    pub save_password: bool,
    /// Existing folders offered in the folder picker.
    pub folders: &'a [String],
}

pub struct ProfileForm {
    pub profile: SessionProfile,
    port: String,
    password: Zeroizing<String>,
    /// The vault already holds a password for this profile.
    has_saved_password: bool,
    error: Option<String>,
    focus_requested: bool,
}

impl ProfileForm {
    pub fn new(profile: SessionProfile, has_saved_password: bool) -> Self {
        Self {
            port: profile.port.to_string(),
            profile,
            password: Zeroizing::default(),
            has_saved_password,
            error: None,
            focus_requested: false,
        }
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
                    if ui.radio_value(&mut p.protocol, proto, proto.to_string()).changed()
                        && self.port == old.default_port().to_string()
                    {
                        self.port = proto.default_port().to_string();
                    }
                }
            });
            ui.end_row();

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

            if p.protocol == Protocol::Ssh {
                ui.label("사용자");
                let user = ui.text_edit_singleline(&mut p.username);
                submit |= enter(ui, &user);
                ui.end_row();

                ui.label("비밀번호");
                let hint = if self.has_saved_password && p.save_password {
                    "저장된 비밀번호 유지 (바꾸려면 입력)"
                } else {
                    "비워두면 접속 시 물어봅니다"
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

        if let Some(err) = &self.error {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, err);
        }
        submit
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
        let password = match p.protocol {
            Protocol::Ssh => {
                if p.username.is_empty() {
                    return Err("SSH 사용자 이름을 입력하세요".into());
                }
                (!self.password.is_empty()).then(|| self.password.clone())
            }
            Protocol::Telnet => {
                // Telnet logs in through the remote prompt; nothing to store.
                p.username.clear();
                p.save_password = false;
                None
            }
        };
        Ok((p, password))
    }
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
        let (p, pw) = form.validate().unwrap();
        assert!(pw.is_none() && !p.save_password && p.username.is_empty());

        form.port = "70000".into();
        assert!(form.validate().is_none());
    }
}
