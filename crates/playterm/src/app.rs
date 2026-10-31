use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Context as _;
use eframe::egui::{self, Color32, RichText};
use playterm_net::{ConnectParams, NetRuntime, Waker};

use crate::connect_dialog::{ConnectDialog, ConnectRequest, DialogResult};
use crate::session::{Session, SessionState};
use crate::{fonts, view};

pub struct PlaytermApp {
    net: NetRuntime,
    waker: Waker,
    known_hosts: PathBuf,
    sessions: Vec<Session>,
    active: usize,
    next_id: u64,
    dialog: ConnectDialog,
    dialog_open: bool,
    window_title: String,
}

impl PlaytermApp {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Vec<ConnectRequest>) -> anyhow::Result<Self> {
        fonts::install(&cc.egui_ctx);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());

        let dirs = directories::ProjectDirs::from("", "", "playterm")
            .context("사용자 설정 폴더를 찾을 수 없습니다")?;
        let known_hosts = dirs.config_dir().join("known_hosts");

        let ctx = cc.egui_ctx.clone();
        let mut app = Self {
            net: NetRuntime::new().context("네트워크 런타임 시작 실패")?,
            waker: Arc::new(move || ctx.request_repaint()),
            known_hosts,
            sessions: Vec::new(),
            active: 0,
            next_id: 1,
            dialog: ConnectDialog::default(),
            dialog_open: initial.is_empty(),
            window_title: String::new(),
        };
        for req in initial {
            app.open_session(req);
        }
        Ok(app)
    }

    fn open_session(&mut self, req: ConnectRequest) {
        // The real size is applied on the first frame; this is only the initial PTY request.
        let (cols, rows) = self
            .sessions
            .get(self.active)
            .map(|s| (s.terminal.cols() as u16, s.terminal.rows() as u16))
            .unwrap_or((80, 24));
        let params = ConnectParams {
            protocol: req.protocol,
            host: req.host,
            port: req.port,
            username: req.username,
            password: req.password,
            term_type: req.term_type,
            cols,
            rows,
            known_hosts: self.known_hosts.clone(),
        };
        let session = Session::connect(self.next_id, params, req.charset, &self.net, self.waker.clone());
        self.next_id += 1;
        self.sessions.push(session);
        self.active = self.sessions.len() - 1;
    }

    fn close_session(&mut self, index: usize) {
        let mut session = self.sessions.remove(index);
        session.close();
        if self.active >= self.sessions.len() {
            self.active = self.sessions.len().saturating_sub(1);
        }
        if self.sessions.is_empty() {
            self.dialog.reopen();
            self.dialog_open = true;
        }
    }

    fn tab_bar(&mut self, ui: &mut egui::Ui) {
        let mut close = None;
        let mut reconnect = false;
        ui.horizontal_wrapped(|ui| {
            if ui.button("+ 새 연결").clicked() {
                self.dialog.reopen();
                self.dialog_open = true;
            }
            ui.separator();
            for (i, s) in self.sessions.iter().enumerate() {
                let dot = match s.state {
                    SessionState::Connecting => Color32::from_rgb(220, 180, 40),
                    SessionState::Connected => Color32::from_rgb(60, 190, 90),
                    SessionState::Closed => Color32::from_rgb(200, 70, 70),
                };
                ui.label(RichText::new("●").color(dot));
                if ui.selectable_label(i == self.active, s.tab_title()).clicked() {
                    self.active = i;
                }
                if ui.small_button("✕").on_hover_text("탭 닫기").clicked() {
                    close = Some(i);
                }
                ui.add_space(6.0);
            }
            if self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Closed) {
                ui.separator();
                reconnect = ui.button("다시 연결").clicked();
            }
        });
        if reconnect {
            let waker = self.waker.clone();
            self.sessions[self.active].reconnect(&self.net, waker);
        }
        if let Some(i) = close {
            self.close_session(i);
        }
    }

    /// Shows at most one modal dialog. Returns `true` if one is open.
    fn dialogs(&mut self, ctx: &egui::Context) -> bool {
        if let Some(index) = self.sessions.iter().position(|s| s.host_key.is_some()) {
            self.active = index;
            host_key_dialog(ctx, &mut self.sessions[index]);
            return true;
        }
        if let Some(index) = self.sessions.iter().position(|s| s.auth.is_some()) {
            self.active = index;
            auth_dialog(ctx, &mut self.sessions[index]);
            return true;
        }
        if self.dialog_open {
            match self.dialog.show(ctx, !self.sessions.is_empty()) {
                DialogResult::Open => {}
                DialogResult::Cancel => self.dialog_open = false,
                DialogResult::Connect(req) => {
                    self.dialog_open = false;
                    self.open_session(req);
                }
            }
            return true;
        }
        false
    }

    fn update_window_title(&mut self, ctx: &egui::Context) {
        let title = match self.sessions.get(self.active) {
            Some(s) => format!("{} - playterm", s.tab_title()),
            None => "playterm".to_owned(),
        };
        if title != self.window_title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.window_title = title;
        }
    }
}

impl eframe::App for PlaytermApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        for session in &mut self.sessions {
            session.poll();
            if let Some(text) = session.clipboard_store.take() {
                ctx.copy_text(text);
            }
        }

        egui::Panel::top("tabs").show(ui, |ui| self.tab_bar(ui));
        let modal_open = self.dialogs(&ctx);

        egui::CentralPanel::no_frame().show(ui, |ui| match self.sessions.get_mut(self.active) {
            Some(session) => view::show(ui, session, !modal_open),
            None => {
                ui.centered_and_justified(|ui| ui.label("[+ 새 연결] 버튼으로 접속하세요"));
            }
        });

        self.update_window_title(&ctx);
    }
}

fn host_key_dialog(ctx: &egui::Context, session: &mut Session) {
    let mut decision = None;
    egui::Modal::new(egui::Id::unique(("host-key", session.id))).show(ctx, |ui| {
        let check = session.host_key.as_ref().expect("checked by caller");
        ui.set_width(460.0);
        ui.heading("알 수 없는 호스트");
        ui.add_space(6.0);
        ui.label(format!(
            "{}:{} 의 호스트 키가 known_hosts에 없습니다.\n처음 접속하는 서버라면 지문이 맞는지 확인한 뒤 진행하세요.",
            check.host, check.port
        ));
        ui.add_space(6.0);
        egui::Grid::new("hk").num_columns(2).show(ui, |ui| {
            ui.label("키 종류");
            ui.monospace(&check.algorithm);
            ui.end_row();
            ui.label("지문");
            ui.monospace(&check.fingerprint);
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("신뢰하고 연결").clicked() {
                decision = Some(true);
            }
            if ui.button("취소").clicked() {
                decision = Some(false);
            }
        });
    });
    if let Some(accept) = decision
        && let Some(check) = session.host_key.take()
    {
        check.answer(accept);
    }
}

fn auth_dialog(ctx: &egui::Context, session: &mut Session) {
    let mut submit = None;
    egui::Modal::new(egui::Id::unique(("auth", session.id))).show(ctx, |ui| {
        let pending = session.auth.as_mut().expect("checked by caller");
        ui.set_width(380.0);
        ui.heading(&pending.prompt.title);
        if !pending.prompt.instructions.trim().is_empty() {
            ui.label(pending.prompt.instructions.trim());
        }
        ui.add_space(6.0);
        let mut enter = false;
        for (i, field) in pending.prompt.fields.iter().enumerate() {
            ui.label(&field.label);
            let edit = ui.add(egui::TextEdit::singleline(&mut pending.answers[i]).password(!field.echo));
            // Check before (re)focusing: Enter makes the field surrender focus this frame.
            enter |= edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            if i == 0 && !pending.focus_requested {
                edit.request_focus();
                pending.focus_requested = true;
            }
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button("확인").clicked() || enter {
                submit = Some(true);
            }
            if ui.button("취소").clicked() {
                submit = Some(false);
            }
        });
    });
    if let Some(ok) = submit
        && let Some(pending) = session.auth.take()
    {
        pending.prompt.answer(ok.then_some(pending.answers));
    }
}
