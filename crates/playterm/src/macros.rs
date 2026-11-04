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
//! @if error             the lines up to @else / @end run only if "error" came in the output
//! @ifnot OK             since the last line typed (or did not come); blocks can nest
//! @else
//! @end
//! @stop                 end here; @fail text: end here as failed, saying so
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
    /// Go on if `text` came (or with `negate`, did not come) in the output since the last line
    /// typed; else go to step `skip_to` (after the matching @else or @end).
    If {
        text: String,
        negate: bool,
        skip_to: usize,
    },
    /// The end of an @if's first part: go to step `skip_to` (after the @end).
    Else {
        skip_to: usize,
    },
    End,
    /// End the macro here: done, or with `Some(message)` failed.
    Stop(Option<String>),
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
    // Open @if blocks: the step of the @if, and of its @else once seen.
    let mut open: Vec<(usize, Option<usize>, usize)> = Vec::new();
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
        let needs_text = |what: &str| {
            if arg.trim().is_empty() {
                Err(tr!("{line}번째 줄: @{what} 다음에 글자를 쓰세요", "Line {line}: @{what} needs some text"))
            } else {
                Ok(arg.trim().to_owned())
            }
        };
        let word = word.to_ascii_lowercase();
        let step = match word.as_str() {
            "wait" => Step::Wait(needs_text("wait")?),
            "sleep" => Step::Sleep(seconds(arg, line)?),
            "timeout" => Step::Timeout(seconds(arg, line)?),
            "send" => Step::Send(arg.to_owned()),
            "enter" => Step::Send("\r".into()),
            "if" | "ifnot" => {
                open.push((steps.len(), None, line));
                Step::If { text: needs_text(&word)?, negate: word == "ifnot", skip_to: 0 }
            }
            "else" => match open.last_mut() {
                Some((_, els @ None, _)) => {
                    *els = Some(steps.len());
                    Step::Else { skip_to: 0 }
                }
                _ => return Err(tr!("{line}번째 줄: 짝이 되는 @if 가 없는 @else", "Line {line}: @else without its @if")),
            },
            "end" => {
                let Some((at_if, at_else, _)) = open.pop() else {
                    return Err(tr!("{line}번째 줄: 짝이 되는 @if 가 없는 @end", "Line {line}: @end without its @if"));
                };
                let after_end = steps.len() + 1;
                let after_else = at_else.map_or(after_end, |e| e + 1);
                if let Step::If { skip_to, .. } = &mut steps[at_if] {
                    *skip_to = after_else;
                }
                if let Some(e) = at_else
                    && let Step::Else { skip_to } = &mut steps[e]
                {
                    *skip_to = after_end;
                }
                Step::End
            }
            "stop" => Step::Stop(None),
            "fail" => Step::Stop(Some(if arg.trim().is_empty() { t("@fail", "@fail").to_owned() } else { arg.trim().to_owned() })),
            _ => {
                return Err(tr!(
                    "{line}번째 줄: 모르는 명령 @{word} (@wait, @sleep, @timeout, @send, @enter, @if, @ifnot, @else, @end, @stop, @fail; 글자 @ 로 시작하는 줄은 @@)",
                    "Line {line}: unknown @{word} (@wait, @sleep, @timeout, @send, @enter, @if, @ifnot, @else, @end, @stop, @fail; a line starting with @ is written @@)"
                ));
            }
        };
        steps.push(step);
    }
    if let Some((_, _, line)) = open.last() {
        return Err(tr!("{line}번째 줄의 @if 에 @end 가 없습니다", "The @if on line {line} has no @end"));
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
    /// Output since the last line typed, in lower case (matching ignores case), for @wait and
    /// @if; @wait looks only after `wait_from`, where the last @wait matched.
    seen: String,
    wait_from: usize,
    timeout: Duration,
    waiting_since: Option<Instant>,
    sleep_until: Option<Instant>,
}

impl MacroRun {
    pub fn new(name: String, steps: Vec<Step>) -> Self {
        Self { name, steps, next: 0, seen: String::new(), wait_from: 0, timeout: DEFAULT_TIMEOUT, waiting_since: None, sleep_until: None }
    }

    /// Output of the remote side (as text).
    pub fn output(&mut self, text: &str) {
        self.seen.push_str(&text.to_lowercase());
        if self.seen.len() > SEEN_MAX {
            let mut cut = self.seen.len() - SEEN_MAX / 2;
            while !self.seen.is_char_boundary(cut) {
                cut += 1;
            }
            self.seen.drain(..cut);
            self.wait_from = self.wait_from.saturating_sub(cut);
        }
    }

    /// Goes as far as it can now: what to type, then whether it is over.
    pub fn step(&mut self, now: Instant) -> Vec<Action> {
        let mut actions = Vec::new();
        while let Some(step) = self.steps.get(self.next) {
            let mut next = self.next + 1;
            match step {
                Step::Send(text) => {
                    actions.push(Action::Send(text.clone()));
                    self.seen.clear();
                    self.wait_from = 0;
                }
                Step::Timeout(d) => self.timeout = *d,
                Step::Sleep(d) => {
                    let until = *self.sleep_until.get_or_insert(now + *d);
                    if now < until {
                        return actions;
                    }
                    self.sleep_until = None;
                }
                Step::Wait(text) => match self.seen[self.wait_from..].find(&text.to_lowercase()) {
                    Some(at) => {
                        self.wait_from += at + text.to_lowercase().len();
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
                },
                Step::If { text, negate, skip_to } => {
                    if self.seen.contains(&text.to_lowercase()) == *negate {
                        next = *skip_to;
                    }
                }
                Step::Else { skip_to } => next = *skip_to,
                Step::End => {}
                Step::Stop(failure) => {
                    self.next = self.steps.len();
                    actions.push(match failure {
                        Some(message) => Action::Failed(message.clone()),
                        None => Action::Done,
                    });
                    return actions;
                }
            }
            self.next = next;
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

const LIST_WIDTH: f32 = 170.0;
const EDITOR_WIDTH: f32 = 460.0;
/// The list and the editor (name, script, help) side by side.
const BODY_HEIGHT: f32 = 400.0;

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
        let modal = egui::Modal::new(egui::Id::unique("macro-dialog")).backdrop_color(crate::app::theme::MODAL_BACKDROP).show(ctx, |ui| {
            ui.heading(t("매크로 관리", "Macros"));
            ui.add_space(6.0);
            // A modal has no height of its own: a body of a fixed size, or the separator between
            // the list and the editor would take the whole height, making the modal taller on
            // every frame (it then crept up off the screen).
            let body = egui::vec2(LIST_WIDTH + EDITOR_WIDTH + 2.0 * ui.spacing().item_spacing.x + 6.0, BODY_HEIGHT);
            ui.allocate_ui_with_layout(body, egui::Layout::left_to_right(egui::Align::Min), |ui| {
                ui.set_min_size(body);
                ui.vertical(|ui| {
                    ui.set_width(LIST_WIDTH);
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
                    ui.set_width(EDITOR_WIDTH);
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
                             @if 글자 / @ifnot 글자 … @else … @end: 마지막 입력 뒤 출력에 따라 · @stop · @fail 메시지\n\
                             {user} {password}: 그 탭의 사용자·비밀번호 (비밀번호를 직접 적지 마세요).",
                            "Each line is typed with Enter · @wait text: wait until it comes in the output\n\
                             @timeout s: how long to wait (default 30) · @sleep s: pause\n\
                             @send text: type without Enter · @enter: Enter alone · # comment · @@: a line starting with @\n\
                             @if text / @ifnot text … @else … @end: by the output since the last line · @stop · @fail message\n\
                             {user} {password}: the tab's user and password (do not write passwords here).",
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
                result = MacroDialogResult::Save(
                    self.macros.iter().map(|m| Macro { name: m.name.trim().to_owned(), script: m.script.clone() }).collect(),
                );
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
        let mut dialog =
            MacroDialog::new(&[Macro { name: "a".into(), script: "ls".into() }, Macro { name: " a ".into(), script: "pwd".into() }]);
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
    fn branches_on_the_output() {
        let script = "show int\n@wait #\n@if down\nno shutdown\n@else\n@ifnot UP\n@fail neither\n@end\n@end\nexit";
        let run = |output: &str| {
            let mut run = MacroRun::new("m".into(), parse(script).unwrap());
            let now = Instant::now();
            let mut sent: Vec<String> = sends(&run.step(now)).iter().map(|s| s.to_string()).collect();
            run.output(output);
            let actions = run.step(now);
            sent.extend(sends(&actions).iter().map(|s| s.to_string()));
            let failed = actions.iter().find_map(|a| match a {
                Action::Failed(m) => Some(m.clone()),
                _ => None,
            });
            (sent, failed)
        };
        assert_eq!(run("Gi0/1 is down\r\nrouter#"), (vec!["show int\r".into(), "no shutdown\r".into(), "exit\r".into()], None));
        assert_eq!(run("Gi0/1 is up\r\nrouter#"), (vec!["show int\r".into(), "exit\r".into()], None));
        assert_eq!(run("router#"), (vec!["show int\r".into()], Some("neither".into())));

        assert!(parse("a\n@if x\nb").is_err(), "no @end");
        assert!(parse("a\n@end").is_err());
        assert!(parse("a\n@if x\n@else\n@else\n@end").is_err());
        assert!(matches!(
            MacroRun::new("m".into(), parse("a\n@stop\nb").unwrap()).step(Instant::now()).as_slice(),
            [Action::Send(_), Action::Done]
        ));
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
