//! Bounds what a remote host can make the terminal parser hold: OSC strings (`ESC ]` ... `BEL`
//! or `ESC \`, window titles, clipboard writes, hyperlinks) are buffered by vte without any
//! limit, and alacritty keeps up to 4096 copies of the title on its title stack. A host
//! sending an endless OSC, or a huge title pushed again and again, could take all memory and
//! end the program with every tab in it.
//!
//! [`OscGuard`] sits in front of the parser: it holds an OSC string back until it ends, then
//! passes it on whole, or drops it if it grew past its limit.

/// Limit for OSC 52 (clipboard): about 1 MB of text, base64-encoded.
const CLIPBOARD_LIMIT: usize = 2 * 1024 * 1024;
/// Limit for titles (OSC 0, 1, 2).
const TITLE_LIMIT: usize = 1024;
/// Limit for other OSC strings (colors, hyperlinks, ...).
const OTHER_LIMIT: usize = 16 * 1024;

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    /// After an ESC that has not been passed on yet.
    Escape,
    /// Inside an OSC string, held back in `held`.
    Osc,
    /// Inside an OSC string that grew too long: dropped up to its end.
    Discard,
}

#[derive(Default)]
pub(crate) struct OscGuard {
    state: State,
    /// The OSC string so far, from its `ESC ]`.
    held: Vec<u8>,
    out: Vec<u8>,
}

impl OscGuard {
    /// `input` as the parser should see it: complete OSC strings within their limit, others
    /// dropped. Bytes of an unfinished OSC (or a trailing ESC) are kept for the next call.
    pub(crate) fn filter(&mut self, input: &[u8]) -> &[u8] {
        self.out.clear();
        for &byte in input {
            self.push(byte);
        }
        &self.out
    }

    fn push(&mut self, byte: u8) {
        match self.state {
            State::Ground => {
                if byte == ESC {
                    self.state = State::Escape;
                } else {
                    self.out.push(byte);
                }
            }
            State::Escape => {
                if byte == b']' {
                    self.held.clear();
                    self.held.extend_from_slice(&[ESC, b']']);
                    self.state = State::Osc;
                } else {
                    // Any other escape sequence goes through as it is.
                    self.out.push(ESC);
                    self.state = State::Ground;
                    self.push(byte);
                }
            }
            State::Osc => match byte {
                // Ends the string (CAN / SUB end it too in vte, then act as controls).
                BEL | CAN | SUB => {
                    self.out.append(&mut self.held);
                    self.out.push(byte);
                    self.state = State::Ground;
                }
                // Ends it as well (the start of ST, `ESC \`, or of another sequence).
                ESC => {
                    self.out.append(&mut self.held);
                    self.state = State::Escape;
                }
                _ => {
                    self.held.push(byte);
                    if self.held.len() > limit(&self.held) {
                        self.held = Vec::new();
                        self.state = State::Discard;
                    }
                }
            },
            State::Discard => match byte {
                BEL => self.state = State::Ground,
                CAN | SUB => {
                    self.out.push(byte);
                    self.state = State::Ground;
                }
                ESC => self.state = State::Escape,
                _ => {}
            },
        }
    }
}

/// The size limit of the OSC string `held` (`ESC ] Ps ; ...`), by its number.
fn limit(held: &[u8]) -> usize {
    let number = held[2..].split(|&b| b == b';').next().unwrap_or(&[]);
    match number {
        b"52" => CLIPBOARD_LIMIT,
        b"0" | b"1" | b"2" => TITLE_LIMIT,
        _ => OTHER_LIMIT,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(guard: &mut OscGuard, chunks: &[&[u8]]) -> Vec<u8> {
        let mut out = Vec::new();
        for chunk in chunks {
            out.extend_from_slice(guard.filter(chunk));
        }
        out
    }

    #[test]
    fn ordinary_output_passes_unchanged() {
        let mut g = OscGuard::default();
        let text: &[u8] = b"hello \x1b[1;31mred\x1b[0m \x1b]0;title\x07 \x1b]8;;http://x\x1b\\link\x1b]8;;\x1b\\ end\x1b";
        assert_eq!(run(&mut g, &[text, b"[K"]), [text, b"[K"].concat());
    }

    #[test]
    fn strings_split_across_reads_are_passed_on_whole() {
        let mut g = OscGuard::default();
        assert_eq!(g.filter(b"a\x1b]2;ti"), b"a");
        assert_eq!(g.filter(b"tle"), b"");
        assert_eq!(g.filter(b"\x07b"), b"\x1b]2;title\x07b");
    }

    #[test]
    fn oversized_strings_are_dropped_up_to_their_end() {
        let mut g = OscGuard::default();
        let title = [b"\x1b]0;".as_slice(), &vec![b'x'; TITLE_LIMIT + 10], b"\x07after"].concat();
        assert_eq!(run(&mut g, &[&title]), b"after");

        // An endless string holds no more than its limit, however much arrives.
        let mut g = OscGuard::default();
        g.filter(b"\x1b]777;");
        for _ in 0..1000 {
            assert!(g.filter(&[b'y'; 4096]).is_empty());
            assert!(g.held.len() <= OTHER_LIMIT);
        }
        assert_eq!(g.filter(b"\x1b\\ok"), b"\x1b\\ok", "after the end, output flows again");
    }

    #[test]
    fn clipboard_writes_get_room() {
        let mut g = OscGuard::default();
        let copy = [b"\x1b]52;c;".as_slice(), &vec![b'Q'; 100_000], b"\x07"].concat();
        assert_eq!(run(&mut g, &[&copy]), copy);
        let too_big = [b"\x1b]52;c;".as_slice(), &vec![b'Q'; CLIPBOARD_LIMIT + 1], b"\x07"].concat();
        assert!(run(&mut g, &[&too_big]).is_empty());
    }

    #[test]
    fn cancel_ends_a_dropped_string_and_still_acts() {
        let mut g = OscGuard::default();
        let input = [b"\x1b]2;".as_slice(), &vec![b'z'; TITLE_LIMIT + 1], b"\x18x"].concat();
        assert_eq!(run(&mut g, &[&input]), b"\x18x");
    }
}
