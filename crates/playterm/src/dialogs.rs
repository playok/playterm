//! Modal dialogs. Each `show` returns what the user decided; the app performs the action.

use eframe::egui;
use playterm_config::SessionProfile;
use zeroize::Zeroizing;

use crate::profile_form::{FormOptions, ProfileForm};
use crate::session::Session;

fn modal(id: impl std::hash::Hash + std::fmt::Debug, ctx: &egui::Context, add: impl FnOnce(&mut egui::Ui)) -> bool {
    egui::Modal::new(egui::Id::unique(id)).show(ctx, add).should_close()
}

// ---------------------------------------------------------------------------------------------
// Quick connect

pub enum QuickConnectResult {
    Open,
    Cancel,
    Connect { profile: Box<SessionProfile>, password: Option<Zeroizing<String>>, save: bool },
}

pub struct QuickConnect {
    form: ProfileForm,
    save: bool,
}

impl QuickConnect {
    pub fn new() -> Self {
        Self { form: ProfileForm::new(SessionProfile::default(), false), save: false }
    }

    pub fn show(&mut self, ctx: &egui::Context) -> QuickConnectResult {
        let mut result = QuickConnectResult::Open;
        let close = modal("quick-connect", ctx, |ui| {
            ui.set_width(380.0);
            ui.heading("빠른 연결");
            ui.add_space(8.0);
            let mut submit =
                self.form.ui(ui, &FormOptions { meta: false, save_password: false, folders: &[] });
            ui.add_space(4.0);
            ui.checkbox(&mut self.save, "사이트 목록에 저장");
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                submit |= ui.button("연결").clicked();
                if ui.button("취소").clicked() {
                    result = QuickConnectResult::Cancel;
                }
            });
            if submit && let Some((profile, password)) = self.form.validate() {
                result = QuickConnectResult::Connect { profile: Box::new(profile), password, save: self.save };
            }
        });
        if close && matches!(result, QuickConnectResult::Open) {
            result = QuickConnectResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------------------------
// Vault

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum VaultMode {
    Unlock,
    Create,
}

pub enum VaultResult {
    Open,
    Cancel,
    Submit(Zeroizing<String>),
    /// User confirmed deleting the vault after forgetting the master password.
    Reset,
}

pub struct VaultDialog {
    pub mode: VaultMode,
    /// Why the vault is needed, shown at the top.
    reason: String,
    master: Zeroizing<String>,
    confirm: Zeroizing<String>,
    error: Option<String>,
    confirm_reset: bool,
    focus_requested: bool,
}

impl VaultDialog {
    pub fn new(mode: VaultMode, reason: impl Into<String>) -> Self {
        Self {
            mode,
            reason: reason.into(),
            master: Zeroizing::default(),
            confirm: Zeroizing::default(),
            error: None,
            confirm_reset: false,
            focus_requested: false,
        }
    }

    /// Shows an error from the attempted unlock/create and clears the input.
    pub fn fail(&mut self, message: String) {
        self.error = Some(message);
        self.master.clear();
        self.confirm.clear();
        self.focus_requested = false;
    }

    /// Switches to creating a new vault (after a reset).
    pub fn switch_to_create(&mut self) {
        let reason = std::mem::take(&mut self.reason);
        *self = Self::new(VaultMode::Create, reason);
    }

    pub fn show(&mut self, ctx: &egui::Context) -> VaultResult {
        let mut result = VaultResult::Open;
        let close = modal("vault", ctx, |ui| {
            ui.set_width(380.0);
            ui.heading(match self.mode {
                VaultMode::Unlock => "비밀번호 볼트 잠금 해제",
                VaultMode::Create => "마스터 비밀번호 설정",
            });
            if !self.reason.is_empty() {
                ui.label(&self.reason);
            }
            if self.mode == VaultMode::Create {
                ui.label("저장된 비밀번호는 이 마스터 비밀번호로 암호화됩니다.\n잊어버리면 복구할 수 없습니다.");
            }
            ui.add_space(6.0);

            let mut enter = false;
            ui.label("마스터 비밀번호");
            let first = ui.add(egui::TextEdit::singleline(&mut *self.master).password(true));
            enter |= first.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.focus_requested {
                first.request_focus();
                self.focus_requested = true;
            }
            if self.mode == VaultMode::Create {
                ui.label("마스터 비밀번호 확인");
                let second = ui.add(egui::TextEdit::singleline(&mut *self.confirm).password(true));
                enter |= second.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }

            if let Some(err) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if self.mode == VaultMode::Unlock { "잠금 해제" } else { "만들기" };
                if ui.button(label).clicked() || enter {
                    if self.master.is_empty() {
                        self.error = Some("마스터 비밀번호를 입력하세요".into());
                    } else if self.mode == VaultMode::Create && *self.master != *self.confirm {
                        self.error = Some("두 비밀번호가 다릅니다".into());
                    } else {
                        result = VaultResult::Submit(self.master.clone());
                    }
                }
                if ui.button("취소").clicked() {
                    result = VaultResult::Cancel;
                }
            });

            if self.mode == VaultMode::Unlock {
                ui.add_space(6.0);
                if !self.confirm_reset {
                    if ui.small_button("마스터 비밀번호를 잊었나요?").clicked() {
                        self.confirm_reset = true;
                    }
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        "볼트를 초기화하면 저장된 모든 비밀번호가 삭제됩니다.\n세션 목록은 유지됩니다.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("초기화").clicked() {
                            result = VaultResult::Reset;
                        }
                        if ui.button("돌아가기").clicked() {
                            self.confirm_reset = false;
                        }
                    });
                }
            }
        });
        if close && matches!(result, VaultResult::Open) {
            result = VaultResult::Cancel;
        }
        result
    }
}

// ---------------------------------------------------------------------------------------------
// Simple confirmation

/// `Some(true)` = confirmed, `Some(false)` = cancelled, `None` = still open.
pub fn confirm(ctx: &egui::Context, title: &str, text: &str, ok_label: &str) -> Option<bool> {
    let mut answer = None;
    let close = modal(("confirm", title), ctx, |ui| {
        ui.set_width(320.0);
        ui.heading(title);
        ui.label(text);
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(ok_label).clicked() {
                answer = Some(true);
            }
            if ui.button("취소").clicked() {
                answer = Some(false);
            }
        });
    });
    if close && answer.is_none() { Some(false) } else { answer }
}

// ---------------------------------------------------------------------------------------------
// Help

const SHORTCUTS: &[(&str, &str)] = &[
    ("Ctrl+Shift+T", "빠른 연결"),
    ("Ctrl+Shift+S", "사이트 관리"),
    ("Ctrl+Shift+B", "사이트 목록 표시/숨김"),
    ("Ctrl+Shift+F", "SFTP 파일 브라우저"),
    ("Ctrl+Tab / Ctrl+Shift+Tab", "다음 / 이전 탭"),
    ("Ctrl+Shift+D", "탭 복제"),
    ("Ctrl+Shift+W", "탭 닫기"),
    ("Ctrl+Shift+C / Ctrl+Shift+V", "복사 / 붙여넣기"),
    ("Ctrl+C", "선택 영역이 있으면 복사, 없으면 ^C 전송"),
    ("Ctrl+V, Shift+Insert", "붙여넣기"),
    ("Shift+드래그", "마우스를 쓰는 프로그램 안에서도 로컬 선택"),
    ("F11", "전체 화면"),
];

/// Keyboard shortcut list. Returns `true` once closed.
pub fn shortcuts(ctx: &egui::Context) -> bool {
    let mut done = false;
    let close = modal("shortcuts", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading("단축키");
        ui.add_space(6.0);
        egui::Grid::new("shortcut-list").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
            for (keys, what) in SHORTCUTS {
                ui.monospace(*keys);
                ui.label(*what);
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        done = ui.button("닫기").clicked();
    });
    done || close
}

/// "About" box. Returns `true` once closed.
pub fn about(ctx: &egui::Context) -> bool {
    let mut done = false;
    let close = modal("about", ctx, |ui| {
        ui.set_width(340.0);
        ui.vertical_centered(|ui| {
            ui.heading("playterm");
            ui.label(format!("버전 {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label("Windows용 SSH · Telnet · 시리얼 터미널");
            ui.weak("SFTP, 포트 포워딩, 세션 로그, 한글(CP949) 지원");
            ui.add_space(8.0);
            done = ui.button("확인").clicked();
        });
    });
    done || close
}

// ---------------------------------------------------------------------------------------------
// Per-session prompts from the network layer

pub fn host_key(ctx: &egui::Context, session: &mut Session) {
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

pub fn auth(ctx: &egui::Context, session: &mut Session) {
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
