//! Automatic login: answers prompts in the output (`login:` → user name, `Password:` →
//! password) by the steps saved with a site.

use std::time::{Duration, Instant};

use playterm_config::LoginStep;
use zeroize::Zeroizing;

/// How long an automatic login waits for its next prompt. A login is quick; a matching line much
/// later (a `tail -f` of an attacker-controlled log, a nested `su` asking `Password:`) must not
/// get the site's password typed into it.
const LOGIN_WINDOW: Duration = Duration::from_secs(60);

/// The steps of one connection with what `{user}` and `{password}` stand for.
#[derive(Clone)]
pub struct LoginScript {
    pub steps: Vec<LoginStep>,
    pub user: String,
    pub password: Option<Zeroizing<String>>,
}

pub enum LoginAction {
    /// Type this (the step's text and Enter).
    Send(Zeroizing<String>),
    /// Stop with this message (the step needs something we do not have).
    Stop(String),
}

/// Runs a [`LoginScript`] against the output of one connection.
pub struct LoginRunner {
    script: LoginScript,
    next: usize,
    /// After typing, wait for output on a new line before the next step, so a step can never
    /// answer the prompt the previous one just answered.
    wait_new_line: bool,
    /// The next step must come before this (see [`LOGIN_WINDOW`]).
    expires: Instant,
}

impl LoginRunner {
    pub fn new(script: LoginScript) -> Self {
        Self { script, next: 0, wait_new_line: false, expires: Instant::now() + LOGIN_WINDOW }
    }

    /// The user is still at it (pressed Enter to wake a console): wait a while longer.
    pub fn extend(&mut self, now: Instant) {
        self.expires = now + LOGIN_WINDOW;
    }

    pub fn done(&self) -> bool {
        self.next >= self.script.steps.len()
    }

    /// New output arrived. `newline` says whether it contained a line break; `line` is the text
    /// before the cursor on its line (where a program waiting for input leaves its prompt).
    pub fn on_output(&mut self, newline: bool, line: &str, now: Instant) -> Option<LoginAction> {
        if newline {
            self.wait_new_line = false;
        }
        if self.wait_new_line || self.done() {
            return None;
        }
        if now > self.expires {
            self.next = self.script.steps.len();
            return Some(LoginAction::Stop(
                playterm_i18n::t(
                    "자동 로그인: 1분 동안 다음 프롬프트가 나오지 않아 멈췄습니다",
                    "Automatic login: stopped, no prompt came for a minute",
                )
                .into(),
            ));
        }
        let step = &self.script.steps[self.next];
        if !prompt_matches(line, &step.expect) {
            return None;
        }
        self.next += 1;
        self.wait_new_line = true;
        self.expires = now + LOGIN_WINDOW;
        let mut text = step.send.replace("{user}", &self.script.user);
        if text.contains("{password}") {
            let Some(password) = &self.script.password else {
                self.next = self.script.steps.len();
                return Some(LoginAction::Stop(
                    playterm_i18n::t(
                        "자동 로그인: 저장된 비밀번호가 없어 멈춥니다. 직접 입력하세요",
                        "Automatic login: stopped, no saved password. Type it yourself",
                    )
                    .into(),
                ));
            };
            text = text.replace("{password}", password);
        }
        text.push('\r');
        Some(LoginAction::Send(Zeroizing::new(text)))
    }
}

/// A prompt matches when the line ends with the expected text, ignoring case and surrounding
/// spaces. Only the end counts: "Last login: Mon ..." is not a `login:` prompt.
fn prompt_matches(line: &str, expect: &str) -> bool {
    let expect = expect.trim().to_lowercase();
    !expect.is_empty() && line.trim_end().to_lowercase().ends_with(&expect)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runner(steps: &[(&str, &str)], password: Option<&str>) -> LoginRunner {
        let steps = steps.iter().map(|(e, s)| LoginStep { expect: e.to_string(), send: s.to_string() }).collect();
        LoginRunner::new(LoginScript {
            steps,
            user: "admin".into(),
            password: password.map(|p| Zeroizing::new(p.to_owned())),
        })
    }

    fn sent(action: Option<LoginAction>) -> String {
        match action {
            Some(LoginAction::Send(text)) => text.to_string(),
            Some(LoginAction::Stop(msg)) => panic!("stopped: {msg}"),
            None => panic!("nothing sent"),
        }
    }

    #[test]
    fn answers_login_and_password() {
        let mut r = runner(&[("login:", "{user}"), ("Password:", "{password}")], Some("s3cret"));
        assert!(r.on_output(true, "Welcome to router", Instant::now()).is_none());
        assert!(r.on_output(false, "Last login: Mon Oct 30 from", Instant::now()).is_none(), "only the end of a line");
        assert_eq!(sent(r.on_output(true, "router LOGIN: ", Instant::now())), "admin\r", "case and trailing space ignored");
        // The echo of the user name on the same line does not trigger anything.
        assert!(r.on_output(false, "router login: admin", Instant::now()).is_none());
        assert_eq!(sent(r.on_output(true, "Password:", Instant::now())), "s3cret\r");
        assert!(r.done());
        assert!(r.on_output(true, "Password:", Instant::now()).is_none(), "each step runs once");
    }

    #[test]
    fn waits_for_a_new_line_between_steps() {
        let mut r = runner(&[("$", "sudo -i"), ("$", "cd /srv")], None);
        assert_eq!(sent(r.on_output(true, "user@host:~$ ", Instant::now())), "sudo -i\r");
        assert!(r.on_output(false, "user@host:~$ ", Instant::now()).is_none(), "the same prompt again, no new line yet");
        assert_eq!(sent(r.on_output(true, "user@host:~$", Instant::now())), "cd /srv\r");
    }

    #[test]
    fn stops_without_a_saved_password() {
        let mut r = runner(&[("Password:", "{password}"), ("#", "show ver")], None);
        assert!(matches!(r.on_output(true, "Password: ", Instant::now()), Some(LoginAction::Stop(_))));
        assert!(r.done() && r.on_output(true, "#", Instant::now()).is_none());
    }

    #[test]
    fn gives_up_after_a_while() {
        let mut r = runner(&[("login:", "{user}"), ("Password:", "{password}")], Some("s3cret"));
        let start = Instant::now();
        assert_eq!(sent(r.on_output(true, "login:", start)), "admin\r");
        // A "Password:" long after the user name is not the login's any more.
        let late = start + LOGIN_WINDOW + Duration::from_secs(1);
        assert!(matches!(r.on_output(true, "Password:", late), Some(LoginAction::Stop(_))));
        assert!(r.done());

        // Pressing Enter (to wake a console) gives it more time.
        let mut r = runner(&[("login:", "{user}")], None);
        r.extend(late);
        assert_eq!(sent(r.on_output(true, "login:", late + Duration::from_secs(30))), "admin\r");
    }

    #[test]
    fn empty_expect_never_matches() {
        assert!(!prompt_matches("anything", "  "));
        assert!(prompt_matches("Username:", "username:"));
        assert!(!prompt_matches("username: x", "username:"));
    }
}
