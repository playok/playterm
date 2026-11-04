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
