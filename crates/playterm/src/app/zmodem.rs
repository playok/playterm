//! The questions of ZMODEM transfers: the folder for a download, the files for an upload.

use super::*;

impl PlaytermApp {
    /// Asks where to save the ZMODEM downloads that just started, and hands the answers to
    /// their tabs. The question is a window of its own: the app (and the transfer) go on.
    pub(super) fn zmodem_folders(&mut self, ctx: &egui::Context) {
        let start = self.zmodem_last.clone().filter(|dir| dir.is_dir()).unwrap_or_else(|| self.zmodem_dir.clone());
        for session in &mut self.sessions {
            if !session.take_zmodem_question() {
                continue;
            }
            let title = tr!("app.zmodem.folder_for_the_files_received_with_zmodem", arg0 = session.tab_title());
            let (start, ctx) = (start.clone(), ctx.clone());
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(crate::picker::Picker::new(crate::picker::Kind::Folder, title).directory(&start).pick_one());
                ctx.request_repaint();
            });
            self.zmodem_questions.push((session.id, rx));
        }
        let mut answers = Vec::new();
        self.zmodem_questions.retain(|(id, rx)| match rx.try_recv() {
            Ok(dir) => {
                answers.push((*id, dir));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                answers.push((*id, None));
                false
            }
        });
        for (id, dir) in answers {
            if let Some(dir) = &dir {
                self.zmodem_last = Some(dir.clone());
            }
            if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                session.zmodem_folder_chosen(dir);
            }
        }
        self.zmodem_uploads(ctx);
    }

    /// Asks which files to send to the ZMODEM uploads (`rz`) that just started, and hands the
    /// answers to their tabs.
    pub(super) fn zmodem_uploads(&mut self, ctx: &egui::Context) {
        let start = self.upload_last.clone().filter(|dir| dir.is_dir()).unwrap_or_else(local_fs::home);
        for session in &mut self.sessions {
            if !session.take_upload_question() {
                continue;
            }
            let title = tr!("app.zmodem.files_to_send_with_zmodem", arg0 = session.tab_title());
            let (start, ctx) = (start.clone(), ctx.clone());
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let _ = tx.send(crate::picker::Picker::new(crate::picker::Kind::Files, title).directory(&start).pick());
                ctx.request_repaint();
            });
            self.upload_questions.push((session.id, rx));
        }
        let mut answers = Vec::new();
        self.upload_questions.retain(|(id, rx)| match rx.try_recv() {
            Ok(files) => {
                answers.push((*id, files));
                false
            }
            Err(mpsc::TryRecvError::Empty) => true,
            Err(mpsc::TryRecvError::Disconnected) => {
                answers.push((*id, None));
                false
            }
        });
        for (id, files) in answers {
            if let Some(dir) = files.as_ref().and_then(|f| f.first()).and_then(|f| f.parent()) {
                self.upload_last = Some(dir.to_owned());
            }
            if let Some(session) = self.sessions.iter_mut().find(|s| s.id == id) {
                session.zmodem_files_chosen(files);
            }
        }
    }
}
