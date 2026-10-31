//! Automatic login: answers prompts in the output (`login:` → user name, `Password:` →
//! password) by the steps saved with a site.

use playterm_config::LoginStep;
use zeroize::Zeroizing;

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
}

impl LoginRunner {
    pub fn new(script: LoginScript) -> Self {
        Self { script, next: 0, wait_new_line: false }
    }

    pub fn done(&self) -> bool {
        self.next >= self.script.steps.len()
    }

    /// New output arrived. `newline` says whether it contained a line break; `line` is the text
    /// before the cursor on its line (where a program waiting for input leaves its prompt).
    pub fn on_output(&mut self, newline: bool, line: &str) -> Option<LoginAction> {
        if newline {
            self.wait_new_line = false;
        }
        if self.wait_new_line || self.done() {
            return None;
        }
        let step = &self.script.steps[self.next];
        if !prompt_matches(line, &step.expect) {
            return None;
        }
        self.next += 1;
        self.wait_new_line = true;
        let mut text = step.send.replace("{user}", &self.script.user);
        if text.contains("{password}") {
            let Some(password) = &self.script.password else {
                self.next = self.script.steps.len();
                return Some(LoginAction::Stop(
                    "자동 로그인: 저장된 비밀번호가 없어 멈춥니다. 직접 입력하세요".into(),
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
        assert!(r.on_output(true, "Welcome to router").is_none());
        assert!(r.on_output(false, "Last login: Mon Oct 30 from").is_none(), "only the end of a line");
        assert_eq!(sent(r.on_output(true, "router LOGIN: ")), "admin\r", "case and trailing space ignored");
        // The echo of the user name on the same line does not trigger anything.
        assert!(r.on_output(false, "router login: admin").is_none());
        assert_eq!(sent(r.on_output(true, "Password:")), "s3cret\r");
        assert!(r.done());
        assert!(r.on_output(true, "Password:").is_none(), "each step runs once");
    }

    #[test]
    fn waits_for_a_new_line_between_steps() {
        let mut r = runner(&[("$", "sudo -i"), ("$", "cd /srv")], None);
        assert_eq!(sent(r.on_output(true, "user@host:~$ ")), "sudo -i\r");
        assert!(r.on_output(false, "user@host:~$ ").is_none(), "the same prompt again, no new line yet");
        assert_eq!(sent(r.on_output(true, "user@host:~$")), "cd /srv\r");
    }

    #[test]
    fn stops_without_a_saved_password() {
        let mut r = runner(&[("Password:", "{password}"), ("#", "show ver")], None);
        assert!(matches!(r.on_output(true, "Password: "), Some(LoginAction::Stop(_))));
        assert!(r.done() && r.on_output(true, "#").is_none());
    }

    #[test]
    fn empty_expect_never_matches() {
        assert!(!prompt_matches("anything", "  "));
        assert!(prompt_matches("Username:", "username:"));
        assert!(!prompt_matches("username: x", "username:"));
    }
}
