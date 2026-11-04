//! The local prompt's requests (connect, open a site, settings ...).

use super::*;

impl PlaytermApp {
    /// Carries out commands typed at the prompt of any tab.
    pub(super) fn console_requests(&mut self) {
        let pending: Vec<(u64, Request)> = self
            .sessions
            .iter_mut()
            .flat_map(|s| {
                let id = s.id;
                std::mem::take(&mut s.requests).into_iter().map(move |r| (id, r))
            })
            .collect();
        for (id, request) in pending {
            self.console_request(id, request);
        }
    }

    pub(super) fn console_request(&mut self, id: u64, request: Request) {
        let Some(i) = self.index_of(id) else { return };
        let mut output = None;
        match request {
            Request::Connect(profile) => {
                self.connect_in(Some(id), |app| app.open_session(&profile, None, None));
                return;
            }
            Request::OpenSite(query) => match sidebar::find(&self.numbered_sites(), &query) {
                Ok(site) => {
                    self.connect_in(Some(id), |app| app.connect_saved(&site));
                    return;
                }
                Err(e) => output = Some(format!("\x1b[31m{e}\x1b[0m")),
            },
            Request::Sites(filter) => output = Some(self.site_list(&filter)),
            Request::SiteManager => {
                let selected = self.sidebar.selected().map(str::to_owned);
                self.open_site_manager(selected.as_deref(), false, Some(id));
            }
            Request::NewSite => self.open_site_manager(None, true, Some(id)),
            Request::QuickConnect => self.open_quick_connect(Some(id)),
            Request::Reconnect if self.sessions[i].has_target() => {
                self.reconnect(i);
                return;
            }
            Request::Reconnect => output = Some(t("다시 연결할 대상이 없습니다", "Nothing to reconnect to").into()),
            Request::Ping(host, options) => {
                let waker = self.waker.clone();
                self.sessions[i].start_ping(host, options, waker);
                return;
            }
            Request::Exit => {
                self.close_session(i);
                return;
            }
        }
        let session = &mut self.sessions[i];
        if let Some(text) = output {
            session.print(&text);
        }
        session.prompt();
    }

    /// Saved sites in `sites` order (the numbers `open N` accepts).
    pub(super) fn numbered_sites(&self) -> Vec<&SessionProfile> {
        sidebar::grouped(&self.store, "").into_iter().flat_map(|(_, items)| items).collect()
    }

    /// `sites` output: numbered sites matching `filter`.
    pub(super) fn site_list(&self, filter: &str) -> String {
        use crate::console::pad;
        let sites = self.numbered_sites();
        if sites.is_empty() {
            return t(
                "저장된 사이트가 없습니다. open 으로 사이트 관리 창을 열어 추가하세요.",
                "No saved sites. Type open to add one in the Site Manager.",
            )
            .into();
        }
        let mut out = format!(
            "\x1b[90m{}  {}  {}  {}\x1b[0m\n",
            pad(t("번호", "No."), 4),
            pad(t("이름", "Name"), 20),
            pad(t("접속", "Connection"), 34),
            t("폴더", "Folder")
        );
        let mut shown = 0;
        // Numbers count every site, so `open N` works on a filtered list too.
        for (n, p) in sites.iter().enumerate().filter(|(_, p)| sidebar::matches(p, filter)) {
            shown += 1;
            out.push_str(&format!(
                "{:>4}  {}  {}  {}\n",
                n + 1,
                pad(&printable(&p.display_name()), 20),
                pad(&printable(&sidebar::summary(p)), 34),
                printable(&p.folder)
            ));
        }
        if shown == 0 {
            out.push_str(&tr!("'{}' 와 일치하는 사이트가 없습니다\n", "No sites match '{}'\n", printable(filter.trim())));
        }
        out.push_str(t("\x1b[90mopen 번호|이름 으로 접속\x1b[0m", "\x1b[90mopen <number|name> to connect\x1b[0m"));
        out
    }
}
