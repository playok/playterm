//! Modal dialogs. Each `show` returns what the user decided; the app performs the action.

use eframe::egui;
use playterm_config::{ClipboardKeys, SessionProfile};
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
            secret_field(ui, &first);
            enter |= first.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if !self.focus_requested {
                first.request_focus();
                self.focus_requested = true;
            }
            if self.mode == VaultMode::Create {
                ui.label(t("마스터 비밀번호 확인", "Confirm master password"));
                let second = ui.add(egui::TextEdit::singleline(&mut *self.confirm).password(true));
                secret_field(ui, &second);
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
                            "볼트를 초기화하면 저장된 비밀번호 없이 새로 시작합니다. 세션 목록은 유지됩니다.\n기존 볼트 파일은 vault.toml.bak 으로 남겨 둡니다.",
                            "Resetting the password vault starts over without the saved passwords. The site list is kept.\nThe old vault file is kept as vault.toml.bak.",
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

/// Asks before pasting several lines that would each run as a command. `Some(true)`: paste.
pub fn confirm_paste(ctx: &egui::Context, text: &str) -> Option<bool> {
    const SHOWN_LINES: usize = 8;
    const SHOWN_CHARS: usize = 100;
    let lines: Vec<&str> = text.lines().collect();
    let mut preview: Vec<String> = lines
        .iter()
        .take(SHOWN_LINES)
        .map(|l| {
            let l = crate::session::printable(l);
            if l.chars().count() > SHOWN_CHARS { format!("{}…", l.chars().take(SHOWN_CHARS).collect::<String>()) } else { l }
        })
        .collect();
    if lines.len() > SHOWN_LINES {
        preview.push(tr!("… ({}줄 더)", "… ({} more lines)", lines.len() - SHOWN_LINES));
    }
    let mut answer = None;
    let close = modal("confirm-paste", ctx, |ui| {
        ui.set_width(520.0);
        ui.heading(t("여러 줄 붙여넣기", "Paste several lines"));
        ui.label(tr!(
            "{}줄을 붙여넣습니다. 줄마다 Enter 가 입력되어 각 줄이 명령으로 바로 실행될 수 있습니다.",
            "This pastes {} lines. Each ends with Enter, so every line may run as a command right away.",
            lines.len()
        ));
        ui.add_space(6.0);
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.add(egui::Label::new(egui::RichText::new(preview.join("\n")).monospace()).wrap());
        });
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button(t("붙여넣기", "Paste")).clicked() {
                answer = Some(true);
            }
            if ui.button(t("취소", "Cancel")).clicked() {
                answer = Some(false);
            }
        });
        ui.weak(t(
            "도구 → 설정의 '붙여넣기 확인' 에서 끌 수 있습니다.",
            "Can be turned off with 'Confirm paste' in Tools → Settings.",
        ));
    });
    if close && answer.is_none() {
        answer = Some(false);
    }
    answer
}

/// Keys and what they do, in the current language.
fn shortcut_list(keys: &ClipboardKeys) -> Vec<(String, &'static str)> {
    let mut list: Vec<(String, &'static str)> = [
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
        ("Ctrl+Shift+X / Ctrl+Shift+E", t("SFTP 탐색기 (옆 패널) 열기/닫기", "SFTP explorer (side panel) on/off")),
        ("Ctrl+Shift+M", t("명령 입력창 (하단) 열기/닫기", "Command input bar on/off")),
        ("Ctrl+Shift+I", t("모든 탭에 동시 입력 (켜기/끄기)", "Type into all tabs (on/off)")),
        ("Ctrl+Tab / Ctrl+Shift+Tab", t("다음 / 이전 탭", "Next / previous tab")),
        ("Alt+→ / Alt+←", t("다음 / 이전 탭", "Next / previous tab")),
        ("Ctrl+Shift+D", t("탭 복제", "Duplicate tab")),
        ("Ctrl+Shift+W", t("탭 닫기", "Close tab")),
    ]
    .into_iter()
    .map(|(k, what)| (k.to_owned(), what))
    .collect();
    let copy: Vec<&str> = [(keys.ctrl_shift_c, "Ctrl+Shift+C"), (keys.ctrl_insert, "Ctrl+Insert")]
        .into_iter()
        .filter_map(|(on, k)| on.then_some(k))
        .collect();
    if !copy.is_empty() {
        list.push((copy.join(", "), t("복사", "Copy")));
    }
    if keys.ctrl_c {
        let what = t("선택 영역이 있으면 복사, 없으면 ^C 전송", "Copy if there is a selection, otherwise send ^C");
        list.push(("Ctrl+C".into(), what));
    }
    if keys.copy_on_select {
        list.push((t("드래그로 선택", "Select by dragging").into(), t("선택하면 바로 복사", "Copies right away")));
    }
    let paste: Vec<&str> = [
        (keys.ctrl_shift_v, "Ctrl+Shift+V"),
        (keys.ctrl_v, "Ctrl+V"),
        (keys.shift_insert, "Shift+Insert"),
        (keys.right_click_paste, t("오른쪽 클릭", "Right click")),
        (keys.middle_click_paste, t("가운데 클릭", "Middle click")),
    ]
    .into_iter()
    .filter_map(|(on, k)| on.then_some(k))
    .collect();
    if !paste.is_empty() {
        list.push((paste.join(", "), t("붙여넣기", "Paste")));
    }
    list.extend(
        [
            (
                t("Shift+드래그", "Shift+drag"),
                t("마우스를 쓰는 프로그램 안에서도 로컬 선택", "Select locally even in programs that use the mouse"),
            ),
            ("F11", t("전체 화면", "Full screen")),
            ("Ctrl+= / Ctrl+- / Ctrl+0", t("글꼴 크게 / 작게 / 기본 크기", "Larger / smaller / default font size")),
            (t("Ctrl+휠", "Ctrl+wheel"), t("글꼴 크기", "Font size")),
        ]
        .into_iter()
        .map(|(k, what)| (k.to_owned(), what)),
    );
    list
}

/// Keyboard shortcut list. Returns `true` once closed.
pub fn shortcuts(ctx: &egui::Context, keys: &ClipboardKeys) -> bool {
    let mut done = false;
    let close = modal("shortcuts", ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(t("단축키", "Keyboard shortcuts"));
        ui.add_space(6.0);
        egui::Grid::new("shortcut-list").num_columns(2).spacing([16.0, 6.0]).striped(true).show(ui, |ui| {
            for (combo, what) in shortcut_list(keys) {
                ui.monospace(combo);
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
            ui.label(t(
                "Windows용 SSH · Telnet · 시리얼 · 원격 데스크톱 터미널",
                "SSH · Telnet · serial · Remote Desktop terminal for Windows",
            ));
            ui.weak(t(
                "SFTP, 포트 포워딩, 세션 로그, 한글(CP949) 지원",
                "SFTP, port forwarding, session logs, Korean (CP949) support",
            ));
            ui.add_space(4.0);
            ui.weak(t(
                "MIT 라이선스 · 사용한 오픈소스는 도움말 → 오픈소스 라이선스",
                "MIT License · open-source components: Help → Open-source licenses",
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
        if check.certificate {
            ui.label(tr!(
                "{}:{} 의 RDP 서버 인증서를 처음 봅니다 (known_rdp_hosts에 없음).\n처음 접속하는 서버라면 지문이 맞는지 확인한 뒤 진행하세요. 신뢰하면 기억해 두고, 나중에 인증서가 바뀌면 경고합니다.",
                "The RDP server certificate of {}:{} is new (not in known_rdp_hosts).\nIf this is your first connection to the server, check the fingerprint before continuing. A trusted certificate is remembered, and a later change is reported.",
                check.host,
                check.port
            ));
        } else {
            ui.label(tr!(
                "{}:{} 의 호스트 키가 known_hosts에 없습니다.\n처음 접속하는 서버라면 지문이 맞는지 확인한 뒤 진행하세요.",
                "The host key of {}:{} is not in known_hosts.\nIf this is your first connection to the server, check that the fingerprint is correct before continuing.",
                check.host,
                check.port
            ));
        }
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
/// `saved_site`: the tab was opened from a saved site, whose user name could be updated and
/// whose password can be saved ("save password", stored once the login succeeds).
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
            let edit = ui.add(egui::TextEdit::singleline(&mut *pending.answers[i]).password(!field.echo));
            if !field.echo {
                secret_field(ui, &edit);
            }
            // Check before (re)focusing: Enter makes the field surrender focus this frame.
            enter |= edit.lost_focus() && ui.input(|inp| inp.key_pressed(egui::Key::Enter));
            if i == 0 && !pending.focus_requested {
                edit.request_focus();
                pending.focus_requested = true;
            }
        }
        if saved_site && pending.is_password() {
            ui.add_space(4.0);
            ui.checkbox(&mut pending.save_password, t("비밀번호 저장", "Save password")).on_hover_text(t(
                "로그인에 성공하면 마스터 비밀번호로 암호화한 볼트에 저장합니다. 다음부터는 묻지 않습니다.",
                "Once the login succeeds, it is saved in the vault (encrypted with the master password) and not asked again.",
            ));
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
    // Saved only after a successful login; a later prompt (wrong password) replaces it.
    let save = (ok && saved_site && pending.save_password && pending.is_password())
        .then(|| pending.answers.first().cloned().filter(|p| !p.is_empty()))
        .flatten();
    session.save_password_on_login(save);
    let user = pending.user.trim().to_owned();
    if ok && let Some(original) = &pending.prompt.user && &user != original {
        // A password prompt's answer is that user's password; other questions start over.
        let password = match pending.prompt.fields.as_slice() {
            [only] if !only.echo => pending.answers.first().map(|p| p.to_string()).filter(|p| !p.is_empty()),
            _ => None,
        };
        pending.prompt.answer(None);
        return Some(Relogin { user, password, save: pending.save_user });
    }
    // The answers move to the network task; what stays here is zeroed when dropped.
    let answers = ok.then(|| pending.answers.iter().map(|a| a.to_string()).collect());
    pending.prompt.answer(answers);
    None
}

/// Called each frame right after a password field.
///
/// - egui keeps an undo history for every text field, password fields too: copies of what was
///   typed (the master password included) that stay in memory after the dialog is closed. It is
///   cleared, so no copy outlives the frame.
/// - The input method is switched off while the field has focus, as Windows does for its own
///   password boxes: with Korean input on, `secret` would be typed as `ㄴㄷㅊㄱㄷㅅ`, hidden
///   behind the dots, and the login would fail for no visible reason.
pub fn secret_field(ui: &egui::Ui, field: &egui::Response) {
    if let Some(mut state) = egui::text_edit::TextEditState::load(ui.ctx(), field.id) {
        state.clear_undoer();
        state.store(ui.ctx(), field.id);
    }
    if field.has_focus() {
        ui.ctx().output_mut(|o| o.ime = None);
    }
}
