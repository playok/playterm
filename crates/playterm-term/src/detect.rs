//! Telling UTF-8 from CP949 by the bytes a host sends.
//!
//! UTF-8 has a strict structure: CP949 Hangul almost never forms valid UTF-8 (the lead bytes of
//! most syllables, 0xB0–0xBF, are UTF-8 continuation bytes), and UTF-8 Hangul is almost never
//! valid CP949 (its continuation bytes below 0xA1 cannot follow the lead bytes it uses). So a
//! couple of non-ASCII characters are enough to decide.

use encoding_rs::{DecoderResult, EUC_KR};

use crate::Charset;

/// Non-ASCII bytes needed before deciding (two Hangul syllables in either encoding).
const MIN_EVIDENCE: usize = 4;
/// When text is valid in both encodings, decide for UTF-8 after this many non-ASCII bytes.
const AMBIGUOUS_LIMIT: usize = 32;
/// Give up (UTF-8) if nothing is clear after this much non-ASCII data.
const SAMPLE_LIMIT: usize = 16 * 1024;

/// Collects output until it is clear which encoding it is in.
#[derive(Default)]
pub struct Detector {
    /// Output since the first non-ASCII byte (ASCII before it says nothing).
    sample: Vec<u8>,
    non_ascii: usize,
}

/// Whether `data` is valid UTF-8, allowing a sequence cut off at the end.
fn valid_utf8(data: &[u8]) -> bool {
    match std::str::from_utf8(data) {
        Ok(_) => true,
        Err(e) => e.error_len().is_none(),
    }
}

/// Whether `data` is valid CP949, allowing a character cut off at the end.
fn valid_cp949(data: &[u8]) -> bool {
    let mut decoder = EUC_KR.new_decoder_without_bom_handling();
    let mut out = String::with_capacity(decoder.max_utf8_buffer_length(data.len()).unwrap_or(data.len() * 3));
    let (result, _) = decoder.decode_to_string_without_replacement(data, &mut out, false);
    !matches!(result, DecoderResult::Malformed(..))
}

impl Detector {
    /// Looks at the next output. Returns the encoding once it is clear.
    pub fn push(&mut self, bytes: &[u8]) -> Option<Charset> {
        let start = if self.sample.is_empty() { bytes.iter().position(|&b| b >= 0x80)? } else { 0 };
        let bytes = &bytes[start..];
        self.non_ascii += bytes.iter().filter(|&&b| b >= 0x80).count();
        let room = SAMPLE_LIMIT.saturating_sub(self.sample.len());
        self.sample.extend_from_slice(&bytes[..bytes.len().min(room)]);
        if self.non_ascii < MIN_EVIDENCE {
            return None;
        }
        let full = self.sample.len() >= SAMPLE_LIMIT;
        match (valid_utf8(&self.sample), valid_cp949(&self.sample)) {
            (true, false) => Some(Charset::Utf8),
            (false, true) => Some(Charset::Cp949),
            (true, true) if self.non_ascii >= AMBIGUOUS_LIMIT || full => Some(Charset::Utf8),
            // Neither: binary data or another encoding; UTF-8 is the best guess.
            (false, false) => Some(Charset::Utf8),
            _ if full => Some(Charset::Utf8),
            _ => None,
        }
    }

    /// Like [`push`](Self::push), for output after the first decision: only a clear UTF-8 or
    /// CP949 verdict is returned. Ambiguous, mixed or binary data decides nothing, and a
    /// verdict or a failed window starts the next window.
    pub fn push_strict(&mut self, bytes: &[u8]) -> Option<Charset> {
        let start = if self.sample.is_empty() { bytes.iter().position(|&b| b >= 0x80)? } else { 0 };
        let bytes = &bytes[start..];
        self.non_ascii += bytes.iter().filter(|&&b| b >= 0x80).count();
        let room = SAMPLE_LIMIT.saturating_sub(self.sample.len());
        self.sample.extend_from_slice(&bytes[..bytes.len().min(room)]);
        if self.non_ascii < MIN_EVIDENCE {
            return None;
        }
        let (verdict, keep) = match (valid_utf8(&self.sample), valid_cp949(&self.sample)) {
            (true, false) => (Some(Charset::Utf8), false),
            (false, true) => (Some(Charset::Cp949), false),
            (true, true) => (None, self.non_ascii < AMBIGUOUS_LIMIT),
            (false, false) => (None, false),
        };
        if !keep || self.sample.len() >= SAMPLE_LIMIT {
            self.restart();
        }
        verdict
    }

    /// Starts a new window, keeping a UTF-8 sequence cut off at the end so the next read
    /// does not begin in the middle of a character.
    fn restart(&mut self) {
        *self = self.watcher();
    }

    /// A detector for the output after this one's window: it starts with the UTF-8 sequence
    /// cut off at the end of the sample (if any), so the next read does not begin in the
    /// middle of a character, where continuation bytes could pass for CP949 lead bytes.
    pub fn watcher(&self) -> Detector {
        let sample = match std::str::from_utf8(&self.sample) {
            Err(e) if e.error_len().is_none() => self.sample[e.valid_up_to()..].to_vec(),
            _ => Vec::new(),
        };
        Detector { non_ascii: sample.len(), sample }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cp949(text: &str) -> Vec<u8> {
        EUC_KR.encode(text).0.into_owned()
    }

    #[test]
    fn decides_by_hangul() {
        let mut d = Detector::default();
        assert_eq!(d.push(b"Last login: Mon\r\n\x1b[1m$ \x1b[0m"), None, "ASCII says nothing");
        assert_eq!(d.push("안녕하세요\r\n".as_bytes()), Some(Charset::Utf8));

        let mut d = Detector::default();
        assert_eq!(d.push(&cp949("서버에 오신 것을 환영합니다")), Some(Charset::Cp949));
    }

    #[test]
    fn handles_split_and_little_input() {
        let text = cp949("한글");
        let mut d = Detector::default();
        assert_eq!(d.push(&text[..1]), None, "one byte is not enough");
        assert_eq!(d.push(&text[1..3]), None);
        assert_eq!(d.push(&text[3..]), Some(Charset::Cp949));

        // A UTF-8 character cut in two between reads is still valid.
        let utf8 = "가나다".as_bytes();
        let mut d = Detector::default();
        assert_eq!(d.push(&utf8[..2]), None, "half a character");
        assert_eq!(d.push(&utf8[2..]), Some(Charset::Utf8));
    }

    #[test]
    fn cp949_extended_hangul_with_ascii_trail_bytes() {
        // "똠" and "뷁" use UHC trail bytes in the ASCII range.
        let mut d = Detector::default();
        assert_eq!(d.push(&cp949("똠방각하 뷁")), Some(Charset::Cp949));
    }

    #[test]
    fn garbage_falls_back_to_utf8() {
        let mut d = Detector::default();
        assert_eq!(d.push(&[0xFF, 0xFE, 0x80, 0x80, 0xFF]), Some(Charset::Utf8));
    }

    #[test]
    fn strict_decides_only_clear_cases() {
        let mut d = Detector::default();
        assert_eq!(d.push_strict("안녕하세요".as_bytes()), Some(Charset::Utf8));
        assert_eq!(d.push_strict(&cp949("안녕하세요")), Some(Charset::Cp949), "next window starts fresh");
        // Binary data decides nothing and does not poison the next window.
        assert_eq!(d.push_strict(&[0xFF, 0xFE, 0x80, 0x80, 0xFF]), None);
        assert_eq!(d.push_strict(&cp949("한글")), Some(Charset::Cp949));
    }

    #[test]
    fn strict_handles_ascii_split_and_ambiguity() {
        let mut d = Detector::default();
        assert_eq!(d.push_strict(b"plain ascii"), None);
        let utf8 = "가나다".as_bytes();
        assert_eq!(d.push_strict(&utf8[..3]), None);
        assert_eq!(d.push_strict(&utf8[3..]), Some(Charset::Utf8));

        // A character cut between windows does not look like CP949 at the start of the next.
        let text = "한".repeat(3);
        let bytes = text.as_bytes();
        let mut d = Detector::default();
        assert_eq!(d.push_strict(&bytes[..8]), Some(Charset::Utf8));
        assert_eq!(d.push_strict(&bytes[8..]), None, "tail kept, not enough evidence yet");

        // Text valid in both encodings is not decided, however long.
        let mut d = Detector::default();
        let data = "¡".repeat(20).into_bytes();
        assert!(valid_utf8(&data) && valid_cp949(&data));
        assert_eq!(d.push_strict(&data[..10]), None);
        assert_eq!(d.push_strict(&data[10..]), None);
    }
}
