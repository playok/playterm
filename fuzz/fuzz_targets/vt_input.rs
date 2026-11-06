#![no_main]

use libfuzzer_sys::fuzz_target;
use playterm_term::{Charset, Terminal};

fuzz_target!(|data: &[u8]| {
    if data.is_empty() || data.len() > 64 * 1024 {
        return;
    }
    let chunk = usize::from(data[0] % 32) + 1;
    for charset in [Charset::Utf8, Charset::Cp949] {
        // Fixed dimensions and bounded history; no sockets, GUI, logging or filesystem.
        let mut terminal = Terminal::new(80, 24, charset, 128);
        for bytes in data[1..].chunks(chunk) {
            terminal.feed(bytes);
            // Replies and clipboard/title events are inspected as data, never executed.
            let _ = terminal.take_events();
        }
        assert_eq!((terminal.cols(), terminal.rows()), (80, 24));
    }
});
