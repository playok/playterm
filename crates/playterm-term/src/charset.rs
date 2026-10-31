use std::fmt;

use encoding_rs::{EUC_KR, Encoding};

/// Character set used on the wire for a session.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Charset {
    #[default]
    Utf8,
    /// Windows code page 949 (superset of EUC-KR), common on older Korean hosts and devices.
    Cp949,
}

impl Charset {
    pub const ALL: [Charset; 2] = [Charset::Utf8, Charset::Cp949];

    fn encoding(self) -> Option<&'static Encoding> {
        match self {
            Charset::Utf8 => None,
            // encoding_rs' EUC_KR is the WHATWG "euc-kr" = windows-949 (UHC).
            Charset::Cp949 => Some(EUC_KR),
        }
    }
}

impl fmt::Display for Charset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Charset::Utf8 => "UTF-8",
            Charset::Cp949 => "CP949 (EUC-KR)",
        })
    }
}

/// Streaming decoder from the wire charset into UTF-8 bytes for the VT parser.
///
/// Multi-byte sequences split across network reads are carried over between calls.
pub struct OutputDecoder {
    decoder: Option<encoding_rs::Decoder>,
    buf: String,
}

impl OutputDecoder {
    pub fn new(charset: Charset) -> Self {
        Self {
            decoder: charset.encoding().map(|e| e.new_decoder_without_bom_handling()),
            buf: String::new(),
        }
    }

    /// Converts `input` into UTF-8. For UTF-8 sessions the input is returned unchanged,
    /// since the VT parser handles UTF-8 (including split sequences) itself.
    pub fn decode<'a>(&'a mut self, input: &'a [u8]) -> &'a [u8] {
        let Some(decoder) = self.decoder.as_mut() else {
            return input;
        };
        self.buf.clear();
        let needed = decoder
            .max_utf8_buffer_length(input.len())
            .unwrap_or(input.len() * 3 + 16);
        self.buf.reserve(needed);
        let (_, _, _) = decoder.decode_to_string(input, &mut self.buf, false);
        self.buf.as_bytes()
    }
}

/// Encodes user input (UTF-8 text) into the wire charset.
#[derive(Clone, Copy, Debug)]
pub struct InputEncoder {
    charset: Charset,
}

impl InputEncoder {
    pub fn new(charset: Charset) -> Self {
        Self { charset }
    }

    pub fn encode(&self, text: &str) -> Vec<u8> {
        match self.charset.encoding() {
            None => text.as_bytes().to_vec(),
            Some(enc) => {
                // Characters not representable in CP949 become numeric character references
                // in encoding_rs; replace them with '?' instead, like most terminals do.
                let mut out = Vec::with_capacity(text.len());
                let mut tmp = [0u8; 4];
                for ch in text.chars() {
                    let s = ch.encode_utf8(&mut tmp);
                    let (bytes, _, unmappable) = enc.encode(s);
                    if unmappable {
                        out.push(b'?');
                    } else {
                        out.extend_from_slice(&bytes);
                    }
                }
                out
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn utf8_passthrough() {
        let mut d = OutputDecoder::new(Charset::Utf8);
        assert_eq!(d.decode("한글".as_bytes()), "한글".as_bytes());
        assert_eq!(InputEncoder::new(Charset::Utf8).encode("abc"), b"abc");
    }

    #[test]
    fn cp949_roundtrip() {
        let enc = InputEncoder::new(Charset::Cp949);
        let bytes = enc.encode("한글 ok");
        assert_eq!(bytes, vec![0xC7, 0xD1, 0xB1, 0xDB, b' ', b'o', b'k']);

        let mut d = OutputDecoder::new(Charset::Cp949);
        assert_eq!(d.decode(&bytes), "한글 ok".as_bytes());
    }

    #[test]
    fn cp949_split_sequence() {
        let mut d = OutputDecoder::new(Charset::Cp949);
        assert_eq!(d.decode(&[b'a', 0xC7]), b"a");
        assert_eq!(d.decode(&[0xD1, b'\x1b', b'[', b'm']), "한\x1b[m".as_bytes());
    }

    #[test]
    fn cp949_unmappable() {
        assert_eq!(InputEncoder::new(Charset::Cp949).encode("a😀b"), b"a?b");
    }
}
