//! Typing into several tabs: simultaneous input, the command input bar and running macros.

use super::*;

impl PlaytermApp {
    /// The connected tabs, other than the active one, that receive simultaneous input.
    pub(super) fn broadcast_targets(&self) -> Vec<usize> {
        // Only what is typed into a connected tab is repeated: text typed at a local prompt
        // (`exit`, `ssh root@db`) must not run in every connected shell.
        let active_connected = self.sessions.get(self.active).is_some_and(|s| s.state == SessionState::Connected);
        if !self.broadcast || !active_connected {
            return Vec::new();
        }
        (0..self.sessions.len())
            .filter(|&i| i != self.active)
            .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected)
            .collect()
    }

    /// Types a message from the command input bar: into the current tab (and the tabs receiving
    /// simultaneous input), or into every connected tab.
    pub(super) fn send_composed(&mut self, send: compose_bar::Send) {
        let mut targets = vec![self.active];
        if send.to_all {
            targets.extend(
                (0..self.sessions.len())
                    .filter(|&i| i != self.active)
                    .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected),
            );
        } else {
            targets.extend(self.broadcast_targets());
        }
        for i in targets {
            let Some(session) = self.sessions.get_mut(i) else { continue };
            for line in compose_bar::lines(&send.text) {
                session.send_text(line);
                session.send_bytes(vec![b'\r']);
            }
        }
    }

    /// Runs macro `k` in the active tab and the tabs receiving simultaneous input.
    pub(super) fn run_macro(&mut self, k: usize) {
        self.run_macro_in(k, false);
    }

    /// Runs macro `k` in the active tab and the tabs receiving simultaneous input, or with
    /// `to_all` in every connected tab included in simultaneous input (the command bar's choice).
    pub(super) fn run_macro_in(&mut self, k: usize, to_all: bool) {
        let Some(m) = self.settings.macros.get(k) else { return };
        let steps = match crate::macros::parse(&m.script) {
            Ok(steps) => steps,
            Err(e) => {
                self.status = Some(tr!("매크로 '{}' 를 실행할 수 없습니다: {e}", "Cannot run macro '{}': {e}", m.name));
                return;
            }
        };
        let name = m.name.clone();
        let mut targets = vec![self.active];
        if to_all {
            targets.extend(
                (0..self.sessions.len())
                    .filter(|&i| i != self.active)
                    .filter(|&i| self.sessions[i].in_broadcast && self.sessions[i].state == SessionState::Connected),
            );
        } else {
            targets.extend(self.broadcast_targets());
        }
        for i in targets {
            if let Some(session) = self.sessions.get_mut(i) {
                session.run_macro(&name, steps.clone());
            }
        }
    }

    /// Repeats input typed in the active tab in the other receiving tabs.
    pub(super) fn broadcast_input(&mut self, inputs: &[UserInput]) {
        if inputs.is_empty() {
            return;
        }
        for i in self.broadcast_targets() {
            for input in inputs {
                input.send_to(&mut self.sessions[i]);
            }
        }
    }
}

/// What the simultaneous-input indicator says with `others` tabs receiving, and its tooltip.
pub(super) fn broadcast_status(others: usize) -> (String, &'static str) {
    if others == 0 {
        (
            t("동시 입력 켜짐 · 받을 탭 없음", "Typing into all tabs · no other tab").into(),
            t(
                "입력을 함께 받을 연결된 탭이 없습니다. 다른 탭을 연결하거나, 탭 우클릭 → 동시 입력에 포함으로 넣으세요.",
                "No other connected tab takes the input. Connect another tab, or right-click a tab → Include in simultaneous input.",
            ),
        )
    } else {
        (
            tr!("동시 입력 켜짐 · 다른 탭 {others}개", "Typing into all tabs · {others} other"),
            t(
                "이 탭에 치는 내용이 주황색 테두리의 탭에도 똑같이 입력됩니다. 탭 우클릭으로 뺄 수 있습니다.",
                "What you type here also goes to the tabs framed in orange. Right-click a tab to leave it out.",
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_status_says_when_nothing_receives() {
        playterm_i18n::set_english(false);
        assert!(broadcast_status(0).0.contains("받을 탭 없음"));
        assert!(broadcast_status(3).0.contains("다른 탭 3개"));
    }
}
