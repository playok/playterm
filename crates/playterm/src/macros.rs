//! Macros: named lines to type into a tab (Tools → Macros), with simple steps between them that
//! wait for output, pause or type without Enter. A macro runs in the current tab and, with
//! simultaneous input on, in the tabs that receive it.
//!
//! ```text
//! # a comment
//! show version          typed, then Enter
//! @wait #               wait until `#` comes in the output (ignoring case)
//! @timeout 60           seconds the next @wait steps wait (default 30) before the macro stops
//! @sleep 1.5            pause
//! @send y               typed without Enter
//! @enter                Enter alone
//! @@reload              a line that starts with @ (typed as "@reload")
//! ```
//!
//! A macro holds no passwords: it is plain text in the settings file.

use std::time::{Duration, Instant};

use eframe::egui;
use playterm_config::Macro;
use playterm_i18n::{t, tr};

use crate::icon;

/// How long `@wait` waits unless `@timeout` says otherwise.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);
/// Output kept for `@wait` to look in (the end of it).
const SEEN_MAX: usize = 64 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub enum Step {
    /// Type this (already with its Enter, if any).
    Send(String),
    Wait(String),
    Sleep(Duration),
    Timeout(Duration),
}

fn seconds(arg: &str, line: usize) -> Result<Duration, String> {
    arg.trim()
        .parse::<f64>()
        .ok()
        .filter(|s| s.is_finite() && *s >= 0.0 && *s <= 24.0 * 3600.0)
        .map(Duration::from_secs_f64)
        .ok_or_else(|| tr!("{line}번째 줄: 초를 숫자로 쓰세요 ({arg})", "Line {line}: seconds must be a number ({arg})", arg = arg.trim()))
}

/// Reads a macro's text into steps, or says which line is wrong.
pub fn parse(script: &str) -> Result<Vec<Step>, String> {
    let mut steps = Vec::new();
    for (i, raw) in script.lines().enumerate() {
        let line = i + 1;
        let text = raw.trim_end_matches('\r');
        if text.trim().is_empty() || text.trim_start().starts_with('#') {
            continue;
        }
        if let Some(rest) = text.strip_prefix("@@") {
            steps.push(Step::Send(format!("@{rest}\r")));
            continue;
        }
        let Some(directive) = text.strip_prefix('@') else {
            steps.push(Step::Send(format!("{text}\r")));
            continue;
        };
        let (word, arg) = directive.split_once(char::is_whitespace).unwrap_or((directive, ""));
        let step = match word.to_ascii_lowercase().as_str() {
            "wait" if !arg.trim().is_empty() => Step::Wait(arg.trim().to_owned()),
            "wait" => return Err(tr!("{line}번째 줄: @wait 다음에 기다릴 글자를 쓰세요", "Line {line}: @wait needs the text to wait for")),
            "sleep" => Step::Sleep(seconds(arg, line)?),
            "timeout" => Step::Timeout(seconds(arg, line)?),
            "send" => Step::Send(arg.to_owned()),
            "enter" => Step::Send("\r".into()),
            _ => {
                return Err(tr!(
                    "{line}번째 줄: 모르는 명령 @{word} (@wait, @sleep, @timeout, @send, @enter; 글자 @ 로 시작하는 줄은 @@)",
                    "Line {line}: unknown @{word} (@wait, @sleep, @timeout, @send, @enter; a line starting with @ is written @@)"
                ));
            }
        };
        steps.push(step);
    }
    if !steps.iter().any(|s| matches!(s, Step::Send(_))) {
        return Err(t("보낼 줄이 없습니다", "There is nothing to type").into());
    }
    Ok(steps)
}

pub enum Action {
    /// Type this.
    Send(String),
    Done,
    /// Stopped, with why.
    Failed(String),
}

/// A macro running in one tab.
pub struct MacroRun {
    pub name: String,
    steps: Vec<Step>,
    next: usize,
    /// Output since the last line typed (or the last @wait that matched), for @wait.
    seen: String,
    timeout: Duration,
    waiting_since: Option<Instant>,
    sleep_until: Option<Instant>,
}

impl MacroRun {
    pub fn new(name: String, steps: Vec<Step>) -> Self {
        Self { name, steps, next: 0, seen: String::new(), timeout: DEFAULT_TIMEOUT, waiting_since: None, sleep_until: None }
    }

    /// Output of the remote side (as text).
    pub fn output(&mut self, text: &str) {
        self.seen.push_str(text);
        if self.seen.len() > SEEN_MAX {
            let mut cut = self.seen.len() - SEEN_MAX / 2;
            while !self.seen.is_char_boundary(cut) {
                cut += 1;
            }
            self.seen.drain(..cut);
        }
    }

    /// Goes as far as it can now: what to type, then whether it is over.
    pub fn step(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Some(step) = self.steps.get(self.next) {
            match step {
                Step::Send(text) => {
                    actions.push(Action::Send(text.clone()));
                    self.seen.clear();
                }
                Step::Timeout(d) => self.timeout = *d,
                Step::Sleep(d) => {
                    let until = *self.sleep_until.get_or_insert(now + *d);
                    if now < until {
                        return actions;
                    }
                    self.sleep_until = None;
                }
                Step::Wait(text) => {
                    let found = self.seen.to_lowercase().find(&text.to_lowercase()).map(|at| at + text.len());
                    match found {
                        Some(end) => {
                            // The next @wait looks only after this match.
                            let end = (end..=self.seen.len()).find(|&i| self.seen.is_char_boundary(i)).unwrap_or(self.seen.len());
                            self.seen.drain(..end);
                            self.waiting_since = None;
                        }
                        None => {
                            let since = *self.waiting_since.get_or_insert(now);
                            if now.duration_since(since) >= self.timeout {
                                let secs = self.timeout.as_secs_f64();
                                actions.push(Action::Failed(tr!(
                                    "'{text}' 이(가) {secs}초 안에 나오지 않았습니다",
                                    "'{text}' did not come within {secs} s"
                                )));
                                self.next = self.steps.len();
                            }
                            return actions;
                        }
                    }
                }
            }
            self.next += 1;
        }
        actions.push(Action::Done);
        actions
    }

    /// When it next has something to do by itself (a pause or a time limit ending).
    pub fn wake_at(&self) -> Option<Instant> {
        self.sleep_until.or_else(|| self.waiting_since.map(|since| since + self.timeout))
    }
}

// -- the editor -------------------------------------------------------------------------------

pub enum MacroDialogResult {
    Open,
    Cancel,
    Save(Vec<Macro>),
}

/// "매크로 관리": the list on the left, the selected macro's name and text on the right.
pub struct MacroDialog {
    macros: Vec<Macro>,
    selected: Option<usize>,
}

impl MacroDialog {
    pub fn new(macros: &[Macro]) -> Self {
        let selected = (!macros.is_empty()).then_some(0);
        Self { macros: macros.to_vec(), selected }
    }

    /// Why the macros cannot be saved, if they cannot.
    fn problem(&self) -> Option<String> {
        for (i, m) in self.macros.iter().enumerate() {
            let name = m.name.trim();
            if name.is_empty() {
                return Some(tr!("{}번째 매크로에 이름이 없습니다", "Macro {} has no name", i + 1));
            }
            if self.macros[..i].iter().any(|o| o.name.trim() == name) {
                return Some(tr!("이름이 같은 매크로가 있습니다: {name}", "Two macros are named {name}"));
            }
            if let Err(e) = parse(&m.script) {
                return Some(format!("{name}: {e}"));
            }
        }
        None
    }

    pub fn show(&mut self, ctx: &egui::Context) -> MacroDialogResult {
        let mut result = MacroDialogResult::Open;
        let problem = self.problem();
        let modal = egui::Modal::new(egui::Id::unique("macro-dialog")).show(ctx, |ui| {
            ui.heading(t("매크로 관리", "Macros"));
            ui.add_space(6.0);
            ui.horizontal_top(|ui| {
                ui.vertical(|ui| {
                    ui.set_width(170.0);
                    ui.horizontal(|ui| {
                        if ui.add(icon::button(icon::ADD, t("추가", "Add"))).clicked() {
                            let name = tr!("매크로 {}", "Macro {}", self.macros.len() + 1);
                            self.macros.push(Macro { name, script: String::new() });
                            self.selected = Some(self.macros.len() - 1);
                        }
                        if ui.add_enabled(self.selected.is_some(), icon::button(icon::DELETE, t("삭제", "Delete"))).clicked()
                            && let Some(i) = self.selected
                        {
                            self.macros.remove(i);
                            self.selected = (!self.macros.is_empty()).then(|| i.min(self.macros.len() - 1));
                        }
                    });
                    ui.add_space(4.0);
                    egui::ScrollArea::vertical().id_salt("macro-list").max_height(330.0).show(ui, |ui| {
                        if self.macros.is_empty() {
                            ui.weak(t("매크로가 없습니다. [추가] 로 만드세요.", "No macros yet. Add one."));
                        }
                        for (i, m) in self.macros.iter().enumerate() {
                            let name = if m.name.trim().is_empty() { t("(이름 없음)", "(no name)").to_owned() } else { m.name.clone() };
                            if ui.selectable_label(self.selected == Some(i), name).clicked() {
                                self.selected = Some(i);
                            }
                        }
                    });
                });
                ui.separator();
                ui.vertical(|ui| {
                    ui.set_width(460.0);
                    match self.selected.and_then(|i| self.macros.get_mut(i)) {
                        Some(m) => {
                            ui.horizontal(|ui| {
                                ui.label(t("이름", "Name"));
                                ui.add(egui::TextEdit::singleline(&mut m.name).desired_width(f32::INFINITY));
                            });
                            ui.add_space(4.0);
                            egui::ScrollArea::vertical().id_salt("macro-script").max_height(230.0).show(ui, |ui| {
                                ui.add(
                                    egui::TextEdit::multiline(&mut m.script)
                                        .code_editor()
                                        .desired_rows(11)
                                        .desired_width(f32::INFINITY)
                                        .hint_text(t("한 줄에 하나씩 입력할 명령", "One command per line")),
                                );
                            });
                        }
                        None => {
                            ui.weak(t("왼쪽에서 매크로를 고르거나 추가하세요.", "Pick a macro on the left, or add one."));
                        }
                    }
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(t(
                            "줄마다 입력하고 Enter · @wait 글자: 출력에 나올 때까지 기다림\n\
                             @timeout 초: 기다리는 시간(기본 30) · @sleep 초: 잠깐 멈춤\n\
                             @send 글자: Enter 없이 입력 · @enter: Enter 만 · # 주석 · @@: @ 로 시작하는 줄\n\
                             비밀번호는 넣지 마세요 (설정 파일에 그대로 저장됩니다).",
                            "Each line is typed with Enter · @wait text: wait until it comes in the output\n\
                             @timeout s: how long to wait (default 30) · @sleep s: pause\n\
                             @send text: type without Enter · @enter: Enter alone · # comment · @@: a line starting with @\n\
                             Do not put passwords here (the settings file keeps them as they are).",
                        ))
                        .small()
                        .weak(),
                    );
                });
            });
            ui.add_space(6.0);
            if let Some(problem) = &problem {
                ui.colored_label(ui.visuals().error_fg_color, problem);
            }
            ui.separator();
            let (save, cancel) =
                icon::ok_cancel_row(ui, icon::primary(ui, icon::SAVE, t("저장", "Save")), problem.is_none(), icon::cancel());
            if save.clicked() {
                result = MacroDialogResult::Save(self.macros.iter().map(|m| Macro { name: m.name.trim().to_owned(), script: m.script.clone() }).collect());
            }
            if cancel.clicked() {
                result = MacroDialogResult::Cancel;
            }
        });
        if matches!(result, MacroDialogResult::Open) && modal.should_close() {
            result = MacroDialogResult::Cancel;
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sends(actions: &[Action]) -> Vec<&str> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(s) => Some(s.as_str()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn reads_macros() {
        let steps = parse("# check\nshow version\n\n@wait #\n@timeout 5\n@sleep 0.5\n@send y\n@enter\n@@reload\n").unwrap();
        assert_eq!(
            steps,
            [
                Step::Send("show version\r".into()),
                Step::Wait("#".into()),
                Step::Timeout(Duration::from_secs(5)),
                Step::Sleep(Duration::from_millis(500)),
                Step::Send("y".into()),
                Step::Send("\r".into()),
                Step::Send("@reload\r".into()),
            ]
        );
        assert!(parse("@wait").is_err());
        assert!(parse("ls\n@sleep soon").unwrap_err().contains('2'), "the line is named");
        assert!(parse("@reboot").is_err(), "unknown steps are not typed");
        assert!(parse("# only a comment").is_err());
    }

    #[test]
    fn waits_for_output_and_gives_up() {
        let steps = parse("terminal length 0\n@wait #\nshow run\n@timeout 2\n@wait end\nexit").unwrap();
        let mut run = MacroRun::new("m".into(), steps);
        let start = Instant::now();
        assert_eq!(sends(&run.step(start)), ["terminal length 0\r"]);
        // Nothing yet: it waits.
        assert!(run.step(start).is_empty());
        run.output("terminal length 0\r\nrouter");
        assert!(run.step(start).is_empty());
        run.output("#");
        assert_eq!(sends(&run.step(start)), ["show run\r"]);
        // Output from before "show run" does not count, and the time limit ends it.
        run.output("...");
        assert!(run.step(start + Duration::from_secs(1)).is_empty());
        let actions = run.step(start + Duration::from_secs(4));
        assert!(matches!(actions.as_slice(), [Action::Failed(_)]));
        assert!(matches!(run.step(start + Duration::from_secs(5)).as_slice(), [Action::Done]));
    }

    #[test]
    fn the_editor_checks_before_saving() {
        let mut dialog = MacroDialog::new(&[
            Macro { name: "a".into(), script: "ls".into() },
            Macro { name: " a ".into(), script: "pwd".into() },
        ]);
        assert!(dialog.problem().is_some(), "two macros of one name");
        dialog.macros[1].name = "b".into();
        assert!(dialog.problem().is_none());
        dialog.macros[1].script = "@bogus".into();
        assert!(dialog.problem().is_some(), "a script with a mistake");
        dialog.macros[1].script = "pwd".into();
        // It draws (a modal takes a few frames to settle).
        let ctx = egui::Context::default();
        for _ in 0..3 {
            let mut out = ctx.run_ui(Default::default(), |ui| {
                assert!(matches!(dialog.show(ui.ctx()), MacroDialogResult::Open));
            });
            out.textures_delta.clear();
        }
    }

    #[test]
    fn pauses_and_finishes() {
        let mut run = MacroRun::new("m".into(), parse("a\n@sleep 1\nb\n@wait OK\n@wait ok").unwrap());
        let start = Instant::now();
        assert_eq!(sends(&run.step(start)), ["a\r"]);
        assert_eq!(run.wake_at(), Some(start + Duration::from_secs(1)));
        assert!(run.step(start + Duration::from_millis(500)).is_empty());
        assert_eq!(sends(&run.step(start + Duration::from_secs(1))), ["b\r"]);
        // Two waits need two matches (ignoring case).
        run.output("ok");
        assert!(run.step(start + Duration::from_secs(1)).is_empty());
        run.output(" OK");
        assert!(matches!(run.step(start + Duration::from_secs(1)).as_slice(), [Action::Done]));
    }
}
