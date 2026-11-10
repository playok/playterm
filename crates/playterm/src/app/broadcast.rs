//! Typing into several tabs: simultaneous input, the command input bar and running macros.

use super::*;

pub(super) struct MacroPlan {
    pub(super) name: String,
    pub(super) targets: Vec<u64>,
    steps: Vec<crate::macros::Step>,
}

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
                self.status = Some(tr!("app.broadcast.cannot_run_macro", arg0 = m.name, e = e));
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
        let targets = targets.into_iter().filter_map(|i| self.sessions.get(i).map(|session| session.id)).collect();
        let plan = MacroPlan { name, targets, steps };
        if crate::macros::uses_password(&plan.steps) {
            self.modal = Some(Modal::ConfirmPasswordMacro(plan));
        } else {
            self.execute_macro_plan(plan);
        }
    }

    pub(super) fn execute_macro_plan(&mut self, plan: MacroPlan) {
        for id in plan.targets {
            if let Some(session) = self.sessions.iter_mut().find(|session| session.id == id) {
                session.run_macro_confirmed(&plan.name, plan.steps.clone());
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
        (t!("app.broadcast.typing_into_all_tabs_no_other_tab").into(), t!("app.broadcast.no_other_connected_tab_takes_the_input_connect"))
    } else {
        (tr!("app.broadcast.typing_into_all_tabs_other", others = others), t!("app.broadcast.what_you_type_here_also_goes_to_the"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn broadcast_status_says_when_nothing_receives() {
        playterm_i18n::set_language(playterm_i18n::Language::KOREAN);
        assert!(broadcast_status(0).0.contains("받을 탭 없음"));
        assert!(broadcast_status(3).0.contains("다른 탭 3개"));
    }
}
