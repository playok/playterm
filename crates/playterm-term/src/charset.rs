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
    ShiftJis,
    EucJp,
    Gbk,
    Gb18030,
    Big5,
    Windows1250,
    Windows1251,
    Windows1252,
    Windows1253,
    Windows1254,
    Windows1255,
    Windows1256,
    Windows1257,
    Windows1258,
    Windows874,
    Ibm866,
    Koi8R,
    Koi8U,
    Macintosh,
    MacCyrillic,
    Iso88592,
    Iso88593,
    Iso88594,
    Iso88595,
    Iso88596,
    Iso88597,
    Iso88598,
    Iso88598I,
    Iso885910,
    Iso885913,
    Iso885914,
    Iso885915,
    Iso885916,
    /// UTF-8 until the host's output shows which of the two it uses (see `detect`).
    Auto,
}

impl Charset {
    pub const ALL: [Charset; 36] = [
        Charset::Auto,
        Charset::Utf8,
        Charset::Cp949,
        Charset::ShiftJis,
        Charset::EucJp,
        Charset::Gbk,
        Charset::Gb18030,
        Charset::Big5,
        Charset::Windows1250,
        Charset::Windows1251,
        Charset::Windows1252,
        Charset::Windows1253,
        Charset::Windows1254,
        Charset::Windows1255,
        Charset::Windows1256,
        Charset::Windows1257,
        Charset::Windows1258,
        Charset::Windows874,
        Charset::Ibm866,
        Charset::Koi8R,
        Charset::Koi8U,
        Charset::Macintosh,
        Charset::MacCyrillic,
        Charset::Iso88592,
        Charset::Iso88593,
        Charset::Iso88594,
        Charset::Iso88595,
        Charset::Iso88596,
        Charset::Iso88597,
        Charset::Iso88598,
        Charset::Iso88598I,
        Charset::Iso885910,
        Charset::Iso885913,
        Charset::Iso885914,
        Charset::Iso885915,
        Charset::Iso885916,
    ];

    /// Resolve a supported encoding label, including WHATWG aliases.
    pub fn from_label(label: &str) -> Option<Self> {
        if label.trim().eq_ignore_ascii_case("auto") {
            return Some(Self::Auto);
        }
        let normalized = label.trim().to_ascii_lowercase();
        let label = normalized.strip_prefix("cp").unwrap_or(&normalized);
        let alias = match label {
            "65001" => "utf-8",
            "949" | "51949" => "euc-kr",
            "932" => "shift_jis",
            "936" => "gbk",
            "950" => "big5",
            "54936" => "gb18030",
            "51932" => "euc-jp",
            "866" => "ibm866",
            "874" => "windows-874",
            "1250" => "windows-1250",
            "1251" => "windows-1251",
            "1252" => "windows-1252",
            "1253" => "windows-1253",
            "1254" => "windows-1254",
            "1255" => "windows-1255",
            "1256" => "windows-1256",
            "1257" => "windows-1257",
            "1258" => "windows-1258",
            _ => label,
        };
        let encoding = Encoding::for_label(alias.as_bytes())?;
        if encoding == encoding_rs::UTF_8 {
            return Some(Self::Utf8);
        }
        Self::ALL.into_iter().find(|charset| charset.encoding() == Some(encoding))
    }

    fn encoding(self) -> Option<&'static Encoding> {
        match self {
            Charset::Utf8 | Charset::Auto => None,
            // encoding_rs' EUC_KR is the WHATWG "euc-kr" = windows-949 (UHC).
            Charset::Cp949 => Some(EUC_KR),
            Charset::ShiftJis => Some(encoding_rs::SHIFT_JIS),
            Charset::EucJp => Some(encoding_rs::EUC_JP),
            Charset::Gbk => Some(encoding_rs::GBK),
            Charset::Gb18030 => Some(encoding_rs::GB18030),
            Charset::Big5 => Some(encoding_rs::BIG5),
            Charset::Windows1250 => Some(encoding_rs::WINDOWS_1250),
            Charset::Windows1251 => Some(encoding_rs::WINDOWS_1251),
            Charset::Windows1252 => Some(encoding_rs::WINDOWS_1252),
            Charset::Windows1253 => Some(encoding_rs::WINDOWS_1253),
            Charset::Windows1254 => Some(encoding_rs::WINDOWS_1254),
            Charset::Windows1255 => Some(encoding_rs::WINDOWS_1255),
            Charset::Windows1256 => Some(encoding_rs::WINDOWS_1256),
            Charset::Windows1257 => Some(encoding_rs::WINDOWS_1257),
            Charset::Windows1258 => Some(encoding_rs::WINDOWS_1258),
            Charset::Windows874 => Some(encoding_rs::WINDOWS_874),
            Charset::Ibm866 => Some(encoding_rs::IBM866),
            Charset::Koi8R => Some(encoding_rs::KOI8_R),
            Charset::Koi8U => Some(encoding_rs::KOI8_U),
            Charset::Macintosh => Some(encoding_rs::MACINTOSH),
            Charset::MacCyrillic => Some(encoding_rs::X_MAC_CYRILLIC),
            Charset::Iso88592 => Some(encoding_rs::ISO_8859_2),
            Charset::Iso88593 => Some(encoding_rs::ISO_8859_3),
            Charset::Iso88594 => Some(encoding_rs::ISO_8859_4),
            Charset::Iso88595 => Some(encoding_rs::ISO_8859_5),
            Charset::Iso88596 => Some(encoding_rs::ISO_8859_6),
            Charset::Iso88597 => Some(encoding_rs::ISO_8859_7),
            Charset::Iso88598 => Some(encoding_rs::ISO_8859_8),
            Charset::Iso88598I => Some(encoding_rs::ISO_8859_8_I),
            Charset::Iso885910 => Some(encoding_rs::ISO_8859_10),
            Charset::Iso885913 => Some(encoding_rs::ISO_8859_13),
            Charset::Iso885914 => Some(encoding_rs::ISO_8859_14),
            Charset::Iso885915 => Some(encoding_rs::ISO_8859_15),
            Charset::Iso885916 => Some(encoding_rs::ISO_8859_16),
        }
    }
}

impl fmt::Display for Charset {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Charset::Utf8 => "UTF-8",
            Charset::Cp949 => "CP949 (EUC-KR)",
            Charset::ShiftJis => "Shift_JIS (CP932)",
            Charset::EucJp => "EUC-JP",
            Charset::Gbk => "GBK (CP936)",
            Charset::Gb18030 => "GB18030",
            Charset::Big5 => "Big5 (CP950)",
            Charset::Windows1250 => "Windows-1250",
            Charset::Windows1251 => "Windows-1251",
            Charset::Windows1252 => "Windows-1252",
            Charset::Windows1253 => "Windows-1253",
            Charset::Windows1254 => "Windows-1254",
            Charset::Windows1255 => "Windows-1255",
            Charset::Windows1256 => "Windows-1256",
            Charset::Windows1257 => "Windows-1257",
            Charset::Windows1258 => "Windows-1258",
            Charset::Windows874 => "Windows-874",
            Charset::Ibm866 => "IBM866",
            Charset::Koi8R => "KOI8-R",
            Charset::Koi8U => "KOI8-U",
            Charset::Macintosh => "Macintosh",
            Charset::MacCyrillic => "Mac Cyrillic",
            Charset::Iso88592 => "ISO-8859-2",
            Charset::Iso88593 => "ISO-8859-3",
            Charset::Iso88594 => "ISO-8859-4",
            Charset::Iso88595 => "ISO-8859-5",
            Charset::Iso88596 => "ISO-8859-6",
            Charset::Iso88597 => "ISO-8859-7",
            Charset::Iso88598 => "ISO-8859-8",
            Charset::Iso88598I => "ISO-8859-8-I",
            Charset::Iso885910 => "ISO-8859-10",
            Charset::Iso885913 => "ISO-8859-13",
            Charset::Iso885914 => "ISO-8859-14",
            Charset::Iso885915 => "ISO-8859-15",
            Charset::Iso885916 => "ISO-8859-16",

            Charset::Auto => playterm_i18n::t!("charset.auto_detect"),
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
        Self { decoder: charset.encoding().map(|e| e.new_decoder_without_bom_handling()), buf: String::new() }
    }

    /// Converts `input` into UTF-8. For UTF-8 sessions the input is returned unchanged,
    /// since the VT parser handles UTF-8 (including split sequences) itself.
    pub fn decode<'a>(&'a mut self, input: &'a [u8]) -> &'a [u8] {
        let Some(decoder) = self.decoder.as_mut() else {
            return input;
        };
        self.buf.clear();
        let needed = decoder.max_utf8_buffer_length(input.len()).unwrap_or(input.len() * 3 + 16);
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
                // Characters not representable in the selected charset become numeric character references
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
    fn supported_encodings_preserve_controls_and_streaming() {
        let control = "\x1b[31mhello\r\n\x1b[0m";
        for charset in Charset::ALL {
            let bytes = InputEncoder::new(charset).encode(control);
            assert_eq!(bytes, control.as_bytes(), "{charset}");
            let mut decoder = OutputDecoder::new(charset);
            let mut decoded = Vec::new();
            for byte in bytes {
                decoded.extend_from_slice(decoder.decode(&[byte]));
            }
            assert_eq!(decoded, control.as_bytes(), "{charset}");
        }
        for (charset, text) in [
            (Charset::ShiftJis, "日本語"),
            (Charset::EucJp, "日本語"),
            (Charset::Gbk, "中文"),
            (Charset::Gb18030, "中文😀"),
            (Charset::Big5, "中文"),
            (Charset::Windows1251, "Привет"),
            (Charset::Windows1252, "café €"),
            (Charset::Koi8R, "Привет"),
        ] {
            let bytes = InputEncoder::new(charset).encode(text);
            let mut decoder = OutputDecoder::new(charset);
            let mut decoded = Vec::new();
            for byte in bytes {
                decoded.extend_from_slice(decoder.decode(&[byte]));
            }
            assert_eq!(decoded, text.as_bytes(), "{charset}");
        }
    }

    #[test]
    fn labels_and_unsupported_encodings() {
        for (label, expected) in
            [("932", Charset::ShiftJis), (" CP936 ", Charset::Gbk), ("windows-1251", Charset::Windows1251), ("949", Charset::Cp949)]
        {
            assert_eq!(Charset::from_label(label), Some(expected));
        }
        for label in ["utf-16", "iso-2022-jp", "unknown"] {
            assert_eq!(Charset::from_label(label), None);
        }
    }

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
