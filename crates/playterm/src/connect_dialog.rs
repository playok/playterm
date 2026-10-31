//! "New connection" dialog. Replaced by the saved-session manager in a later step.

use eframe::egui;
use playterm_net::Protocol;
use playterm_term::Charset;

pub struct ConnectRequest {
    pub protocol: Protocol,
    pub host: String,
    pub port: u16,
    pub username: String,
    pub password: Option<String>,
    pub charset: Charset,
    pub term_type: String,
}

impl ConnectRequest {
    /// Parses `ssh://[user@]host[:port]` or `telnet://host[:port]` (`user@host` alone means SSH).
    /// IPv6 hosts are written in brackets: `ssh://user@[::1]:2222`.
    pub fn from_url(url: &str) -> Result<Self, String> {
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

        Ok(Self {
            protocol,
            host: host.to_owned(),
            port,
            username,
            password: None,
            charset: Charset::Utf8,
            term_type: "xterm-256color".into(),
        })
    }
}

pub enum DialogResult {
    Open,
    Cancel,
    Connect(ConnectRequest),
}

pub struct ConnectDialog {
    protocol: Protocol,
    host: String,
    port: String,
    username: String,
    password: String,
    charset: Charset,
    term_type: String,
    error: Option<String>,
    focus_requested: bool,
}

impl Default for ConnectDialog {
    fn default() -> Self {
        Self {
            protocol: Protocol::Ssh,
            host: String::new(),
            port: Protocol::Ssh.default_port().to_string(),
            username: String::new(),
            password: String::new(),
            charset: Charset::Utf8,
            term_type: "xterm-256color".into(),
            error: None,
            focus_requested: false,
        }
    }
}

impl ConnectDialog {
    /// Keeps the previous answers but clears the password and error.
    pub fn reopen(&mut self) {
        self.password.clear();
        self.error = None;
        self.focus_requested = false;
    }

    pub fn show(&mut self, ctx: &egui::Context, can_cancel: bool) -> DialogResult {
        let mut result = DialogResult::Open;
        let modal = egui::Modal::new(egui::Id::unique("connect-dialog")).show(ctx, |ui| {
            ui.set_width(360.0);
            ui.heading("새 연결");
            ui.add_space(8.0);

            let mut submit = false;
            egui::Grid::new("connect-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                ui.label("프로토콜");
                ui.horizontal(|ui| {
                    for p in Protocol::ALL {
                        let old = self.protocol;
                        if ui.radio_value(&mut self.protocol, p, p.to_string()).changed()
                            && self.port == old.default_port().to_string()
                        {
                            self.port = p.default_port().to_string();
                        }
                    }
                });
                ui.end_row();

                ui.label("호스트");
                let host = ui.add(egui::TextEdit::singleline(&mut self.host).hint_text("example.com 또는 IP"));
                if !self.focus_requested {
                    host.request_focus();
                    self.focus_requested = true;
                }
                submit |= enter_pressed(ui, &host);
                ui.end_row();

                ui.label("포트");
                let port = ui.add(egui::TextEdit::singleline(&mut self.port).desired_width(80.0));
                submit |= enter_pressed(ui, &port);
                ui.end_row();

                if self.protocol == Protocol::Ssh {
                    ui.label("사용자");
                    let user = ui.text_edit_singleline(&mut self.username);
                    submit |= enter_pressed(ui, &user);
                    ui.end_row();

                    ui.label("비밀번호");
                    let pw = ui.add(
                        egui::TextEdit::singleline(&mut self.password)
                            .password(true)
                            .hint_text("비워두면 접속 시 물어봅니다"),
                    );
                    submit |= enter_pressed(ui, &pw);
                    ui.end_row();
                }

                ui.label("인코딩");
                egui::ComboBox::from_id_salt("charset")
                    .selected_text(self.charset.to_string())
                    .show_ui(ui, |ui| {
                        for c in Charset::ALL {
                            ui.selectable_value(&mut self.charset, c, c.to_string());
                        }
                    });
                ui.end_row();

                ui.label("터미널 타입");
                ui.text_edit_singleline(&mut self.term_type);
                ui.end_row();
            });

            if let Some(err) = &self.error {
                ui.add_space(6.0);
                ui.colored_label(ui.visuals().error_fg_color, err);
            }

            ui.add_space(10.0);
            ui.horizontal(|ui| {
                if ui.button("연결").clicked() {
                    submit = true;
                }
                if can_cancel && ui.button("취소").clicked() {
                    result = DialogResult::Cancel;
                }
            });

            if submit {
                match self.validate() {
                    Ok(req) => result = DialogResult::Connect(req),
                    Err(e) => self.error = Some(e),
                }
            }
        });
        if can_cancel && modal.should_close() && matches!(result, DialogResult::Open) {
            result = DialogResult::Cancel;
        }
        result
    }

    fn validate(&self) -> Result<ConnectRequest, String> {
        let host = self.host.trim();
        if host.is_empty() {
            return Err("호스트를 입력하세요".into());
        }
        let port: u16 = self
            .port
            .trim()
            .parse()
            .ok()
            .filter(|&p| p > 0)
            .ok_or("포트는 1~65535 사이의 숫자여야 합니다")?;
        let username = self.username.trim().to_owned();
        if self.protocol == Protocol::Ssh && username.is_empty() {
            return Err("SSH 사용자 이름을 입력하세요".into());
        }
        let term_type = match self.term_type.trim() {
            "" => "xterm-256color".to_owned(),
            t => t.to_owned(),
        };
        Ok(ConnectRequest {
            protocol: self.protocol,
            host: host.to_owned(),
            port,
            username: if self.protocol == Protocol::Ssh { username } else { String::new() },
            password: (self.protocol == Protocol::Ssh && !self.password.is_empty())
                .then(|| self.password.clone()),
            charset: self.charset,
            term_type,
        })
    }
}

fn enter_pressed(ui: &egui::Ui, response: &egui::Response) -> bool {
    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let r = ConnectRequest::from_url("ssh://root@10.0.0.1:2222").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port, r.username.as_str()), (Protocol::Ssh, "10.0.0.1", 2222, "root"));

        let r = ConnectRequest::from_url("telnet://router").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Telnet, "router", 23));

        let r = ConnectRequest::from_url("admin@[::1]").unwrap();
        assert_eq!((r.protocol, r.host.as_str(), r.port), (Protocol::Ssh, "::1", 22));
    }

    #[test]
    fn rejects_bad_urls() {
        assert!(ConnectRequest::from_url("ssh://host").is_err());
        assert!(ConnectRequest::from_url("ftp://host").is_err());
        assert!(ConnectRequest::from_url("telnet://host:0").is_err());
        assert!(ConnectRequest::from_url("telnet://:23").is_err());
    }
}
