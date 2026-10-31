//! Modal dialogs. Each `show` returns what the user decided; the app performs the action.

use eframe::egui;
use playterm_config::SessionProfile;
use playterm_i18n::{t, tr};
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
            ui.heading(t("빠른 연결", "Quick Connect"));
            ui.add_space(8.0);
            let mut submit =
                self.form.ui(ui, &FormOptions { meta: false, save_password: false, folders: &[] });
            ui.add_space(4.0);
            ui.checkbox(&mut self.save, t("사이트 목록에 저장", "Save to the site list"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                submit |= ui.button(t("연결", "Connect")).clicked();
                if ui.button(t("취소", "Cancel")).clicked() {
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
                VaultMode::Unlock => t("비밀번호 볼트 잠금 해제", "Unlock password vault"),
                VaultMode::Create => t("마스터 비밀번호 설정", "Set master password"),
            });
            if !self.reason.is_empty() {
                ui.label(&self.reason);
            }
            if self.mode == VaultMode::Create {
                ui.label(t(
                    "저장된 비밀번호는 이 마스터 비밀번호로 암호화됩니다.\n잊어버리면 복구할 수 없습니다.",
                    "Saved passwords are encrypted with this master password.\nIf you forget it, they cannot be recovered.",
                ));
            }
            ui.add_space(6.0);

            let mut enter = false;
            ui.label(t("마스터 비밀번호", "Master password"));
            let first = ui.add(egui::TextEdit::singleline(&mut *self.master).password(true));
            enter |= first.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.focus_requested {
                first.request_focus();
                self.focus_requested = true;
            }
            if self.mode == VaultMode::Create {
                ui.label(t("마스터 비밀번호 확인", "Confirm master password"));
                let second = ui.add(egui::TextEdit::singleline(&mut *self.confirm).password(true));
                enter |= second.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            }

            if let Some(err) = &self.error {
                ui.colored_label(ui.visuals().error_fg_color, err);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let label = if self.mode == VaultMode::Unlock { t("잠금 해제", "Unlock") } else { t("만들기", "Create") };
                if ui.button(label).clicked() || enter {
                    if self.master.is_empty() {
                        self.error = Some(t("마스터 비밀번호를 입력하세요", "Enter the master password").into());
                    } else if self.mode == VaultMode::Create && *self.master != *self.confirm {
                        self.error = Some(t("두 비밀번호가 다릅니다", "The passwords do not match").into());
                    } else {
                        result = VaultResult::Submit(self.master.clone());
                    }
                }
                if ui.button(t("취소", "Cancel")).clicked() {
                    result = VaultResult::Cancel;
                }
            });

            if self.mode == VaultMode::Unlock {
                ui.add_space(6.0);
                if !self.confirm_reset {
                    if ui.small_button(t("마스터 비밀번호를 잊었나요?", "Forgot the master password?")).clicked() {
                        self.confirm_reset = true;
                    }
                } else {
                    ui.colored_label(
                        ui.visuals().warn_fg_color,
                        t(
                            "볼트를 초기화하면 저장된 모든 비밀번호가 삭제됩니다.\n세션 목록은 유지됩니다.",
                            "Resetting the password vault deletes every saved password.\nThe site list is kept.",
                        ),
                    );
                    ui.horizontal(|ui| {
                        if ui.button(t("초기화", "Reset")).clicked() {
                            result = VaultResult::Reset;
                        }
                        if ui.button(t("돌아가기", "Back")).clicked() {
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
            if ui.button(t("취소", "Cancel")).clicked() {
                answer = Some(false);
            }
        });
    });
    if close && answer.is_none() { Some(false) } else { answer }
}

// ---------------------------------------------------------------------------------------------
// Help

/// Keys and what they do, in the current language.
fn shortcut_list() -> [(&'static str, &'static str); 17] {
    [
        ("Ctrl+Shift+N", t("새 탭 (로컬 명령 프롬프트)", "New tab (local prompt)")),
        ("Ctrl+Shift+T", t("빠른 연결", "Quick Connect")),
        ("Ctrl+Shift+S", t("사이트 관리", "Site Manager")),
        ("Ctrl+Shift+B", t("사이트 목록 표시/숨김", "Show/hide the site list")),
        (
            "Ctrl+Shift+F",
            t(
                "찾기 (스크롤백 포함, Enter: 위로 · Shift+Enter: 아래로)",
                "Find (including scrollback, Enter: up · Shift+Enter: down)",
            ),
        ),
        ("Ctrl+Shift+E", t("SFTP 파일 브라우저", "SFTP file browser")),
        ("Ctrl+Shift+I", t("모든 탭에 동시 입력 (켜기/끄기)", "Type into all tabs (on/off)")),
        ("Ctrl+Tab / Ctrl+Shift+Tab", t("다음 / 이전 탭", "Next / previous tab")),
        ("Ctrl+Shift+D", t("탭 복제", "Duplicate tab")),
        ("Ctrl+Shift+W", t("탭 닫기", "Close tab")),
        ("Ctrl+Shift+C / Ctrl+Shift+V", t("복사 / 붙여넣기", "Copy / paste")),
        ("Ctrl+C", t("선택 영역이 있으면 복사, 없으면 ^C 전송", "Copy if there is a selection, otherwise send ^C")),
        ("Ctrl+V, Shift+Insert", t("붙여넣기", "Paste")),
        (
            t("Shift+드래그", "Shift+drag"),
            t("마우스를 쓰는 프로그램 안에서도 로컬 선택", "Select locally even in programs that use the mouse"),
        ),
        ("F11", t("전체 화면", "Full screen")),
        ("Ctrl+= / Ctrl+- / Ctrl+0", t("글꼴 크게 / 작게 / 기본 크기", "Larger / smaller / default font size")),
        (t("Ctrl+휠", "Ctrl+wheel"), t("글꼴 크기", "Font size")),
    ]
}

/// Keyboard shortcut list. Returns `true` once closed.
pub fn shortcuts(ctx: &egui::Context) -> bool {
    let mut done = false;
    let close = modal("shortcuts", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(t("단축키", "Keyboard shortcuts"));
        ui.add_space(6.0);
        egui::Grid::new("shortcut-list").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
            for (keys, what) in shortcut_list() {
                ui.monospace(keys);
                ui.label(what);
                ui.end_row();
            }
        });
        ui.add_space(8.0);
        done = ui.button(t("닫기", "Close")).clicked();
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
            ui.label(tr!("버전 {}", "Version {}", env!("CARGO_PKG_VERSION")));
            ui.add_space(6.0);
            ui.label(t("Windows용 SSH · Telnet · 시리얼 터미널", "SSH · Telnet · serial terminal for Windows"));
            ui.weak(t(
                "SFTP, 포트 포워딩, 세션 로그, 한글(CP949) 지원",
                "SFTP, port forwarding, session logs, Korean (CP949) support",
            ));
            ui.add_space(8.0);
            done = ui.button(t("확인", "OK")).clicked();
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
        ui.heading(t("알 수 없는 호스트", "Unknown host"));
        ui.add_space(6.0);
        ui.label(tr!(
            "{}:{} 의 호스트 키가 known_hosts에 없습니다.\n처음 접속하는 서버라면 지문이 맞는지 확인한 뒤 진행하세요.",
            "The host key of {}:{} is not in known_hosts.\nIf this is your first connection to the server, check that the fingerprint is correct before continuing.",
            check.host,
            check.port
        ));
        ui.add_space(6.0);
        egui::Grid::new("hk").num_columns(2).show(ui, |ui| {
            ui.label(t("키 종류", "Key type"));
            ui.monospace(&check.algorithm);
            ui.end_row();
            ui.label(t("지문", "Fingerprint"));
            ui.monospace(&check.fingerprint);
            ui.end_row();
        });
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            if ui.button(t("신뢰하고 연결", "Trust and connect")).clicked() {
                decision = Some(true);
            }
            if ui.button(t("취소", "Cancel")).clicked() {
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

/// Log in again as another user: the user name was changed in the password dialog.
pub struct Relogin {
    pub user: String,
    /// The password typed for that user, if the dialog asked for exactly a password.
    pub password: Option<String>,
    /// Also change the user name of the saved site.
    pub save: bool,
}

/// The credentials dialog. For a login (password, keyboard-interactive) the user name can be
/// corrected too; that answers with a [`Relogin`] instead of the server's questions.
/// `saved_site`: the tab was opened from a saved site, whose user name could be updated.
pub fn auth(ctx: &egui::Context, session: &mut Session, saved_site: bool) -> Option<Relogin> {
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
        let original_user = pending.prompt.user.clone();
        if let Some(original) = &original_user {
            ui.label(t("사용자", "User"));
            let edit = ui.add(egui::TextEdit::singleline(&mut pending.user));
            enter |= edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            if pending.user.trim() != original.as_str() {
                ui.weak(t(
                    "사용자를 바꾸면 새로 연결해서 그 사용자로 로그인합니다.",
                    "Changing the user connects again and logs in as that user.",
                ));
                if saved_site {
                    ui.checkbox(&mut pending.save_user, t("사이트 정보에도 저장", "Save it in the site too"));
                }
            }
            ui.add_space(4.0);
        }
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
        let user_ok = original_user.is_none() || !pending.user.trim().is_empty();
        ui.horizontal(|ui| {
            if ui.add_enabled(user_ok, egui::Button::new(t("확인", "OK"))).clicked() || (enter && user_ok) {
                submit = Some(true);
            }
            if ui.button(t("취소", "Cancel")).clicked() {
                submit = Some(false);
            }
        });
    });
    let ok = submit?;
    let pending = session.auth.take()?;
    let user = pending.user.trim().to_owned();
    if ok && let Some(original) = &pending.prompt.user && &user != original {
        // A password prompt's answer is that user's password; other questions start over.
        let password = match pending.prompt.fields.as_slice() {
            [only] if !only.echo => pending.answers.into_iter().next().filter(|p| !p.is_empty()),
            _ => None,
        };
        pending.prompt.answer(None);
        return Some(Relogin { user, password, save: pending.save_user });
    }
    pending.prompt.answer(ok.then_some(pending.answers));
    None
}
