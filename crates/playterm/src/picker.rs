//! The OS file and folder pickers, behind one door: the app asks with a [`Picker`], and the
//! screen-flow tests answer instead of an OS dialog (see [`fake`]).

use std::path::PathBuf;

/// What to pick.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    File,
    Files,
    Folder,
    /// A file name to save to.
    Save,
}

/// A question for the user: a file, files, a folder, or where to save.
#[derive(Clone, Debug)]
pub struct Picker {
    pub kind: Kind,
    pub title: String,
    pub directory: Option<PathBuf>,
    pub file_name: Option<String>,
    /// (name, extensions)
    pub filters: Vec<(String, Vec<String>)>,
}

impl Picker {
    pub fn new(kind: Kind, title: impl Into<String>) -> Self {
        Self { kind, title: title.into(), directory: None, file_name: None, filters: Vec::new() }
    }

    pub fn directory(mut self, dir: impl Into<PathBuf>) -> Self {
        self.directory = Some(dir.into());
        self
    }

    pub fn file_name(mut self, name: impl Into<String>) -> Self {
        self.file_name = Some(name.into());
        self
    }

    pub fn filter(mut self, name: impl Into<String>, extensions: &[&str]) -> Self {
        self.filters.push((name.into(), extensions.iter().map(|e| (*e).to_owned()).collect()));
        self
    }

    /// Asks (blocking, as an OS dialog does). `None` when the user cancelled.
    pub fn pick(self) -> Option<Vec<PathBuf>> {
        #[cfg(test)]
        if let Some(answer) = fake::answer(&self) {
            return answer;
        }
        let mut dialog = rfd::FileDialog::new().set_title(&self.title);
        if let Some(dir) = &self.directory {
            dialog = dialog.set_directory(dir);
        }
        if let Some(name) = &self.file_name {
            dialog = dialog.set_file_name(name);
        }
        for (name, extensions) in &self.filters {
            dialog = dialog.add_filter(name, extensions);
        }
        match self.kind {
            Kind::File => dialog.pick_file().map(|p| vec![p]),
            Kind::Files => dialog.pick_files(),
            Kind::Folder => dialog.pick_folder().map(|p| vec![p]),
            Kind::Save => dialog.save_file().map(|p| vec![p]),
        }
    }

    /// [`Picker::pick`] for one path.
    pub fn pick_one(self) -> Option<PathBuf> {
        self.pick().and_then(|mut paths| (!paths.is_empty()).then(|| paths.remove(0)))
    }
}

/// Answers for the pickers in tests, in place of the OS dialogs.
#[cfg(test)]
pub mod fake {
    use std::sync::{Mutex, MutexGuard};

    use super::Picker;
    use std::path::PathBuf;

    type Answer = Box<dyn FnMut(&Picker) -> Option<Vec<PathBuf>> + Send>;

    static ANSWER: Mutex<Option<Answer>> = Mutex::new(None);
    /// Tests that answer pickers run one at a time (the pickers are global).
    static SERIAL: Mutex<()> = Mutex::new(());

    /// While the returned guard lives, every picker is answered by `answer` (which also sees
    /// what was asked).
    pub fn answer_with(answer: impl FnMut(&Picker) -> Option<Vec<PathBuf>> + Send + 'static) -> Guard {
        let serial = SERIAL.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        *ANSWER.lock().unwrap() = Some(Box::new(answer));
        Guard { _serial: serial }
    }

    pub struct Guard {
        _serial: MutexGuard<'static, ()>,
    }

    impl Drop for Guard {
        fn drop(&mut self) {
            *ANSWER.lock().unwrap() = None;
        }
    }

    pub(super) fn answer(picker: &Picker) -> Option<Option<Vec<PathBuf>>> {
        let mut answer = ANSWER.lock().unwrap();
        // Without an answer set, a test must not open an OS dialog (it would hang the run).
        let f = answer.as_mut().unwrap_or_else(|| panic!("a test opened a picker without an answer: {picker:?}"));
        Some(f(picker))
    }
}
