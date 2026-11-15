//! Text out of a PDF, for a provider that has no document block.
//!
//! The OpenAI-compatible chat shape carries images as data URIs and has no
//! equivalent for a document. Sending the PDF's bytes as text would hand the
//! model base64 it cannot read; sending only the file's name loses the file.
//! So the text layer is read here, on this machine: streams are located by
//! their `stream` keyword, FlateDecode streams are inflated, and the
//! text-showing operators (`Tj`, `TJ`, `'`, `"`) are turned back into text.
//!
//! This is deliberately a reader, not a renderer. There are no font tables, so
//! a subset font with a custom encoding yields bytes that are not text. Every
//! result is checked for legibility, and `None` means "this PDF's text is not
//! readable here" — the caller says that plainly instead of shipping mojibake.
//! A scan has no text layer and lands in the same place.

use std::io::Read;

/// Longest text handed to a model from one PDF.
pub const MAX_EXTRACTED_TEXT: usize = 200_000;

/// The text layer of `bytes`, or `None` when nothing legible is in it.
pub fn extract(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"%PDF-") {
        return None;
    }

    let mut out = String::new();
    let mut from = 0;
    while let Some(at) = next_stream(bytes, from) {
        let mut body = at + b"stream".len();
        if bytes.get(body) == Some(&b'\r') {
            body += 1;
        }
        if bytes.get(body) == Some(&b'\n') {
            body += 1;
        }
        let Some(end) = find(bytes, body, b"endstream") else {
            break;
        };

        // The dictionary sits immediately before the keyword; 1 KiB covers a
        // `/Filter` entry even when it carries a decode parameter array.
        let dict = &bytes[at.saturating_sub(1024)..at];
        let decoded = if contains_ignore_ascii_case(dict, b"/FlateDecode") {
            inflate(&bytes[body..end])
        } else {
            Some(bytes[body..end].to_vec())
        };

        if let Some(data) = decoded
            && let Some(text) = text_from_content(&data)
        {
            let text = tidy(&text);
            if legible(&text) {
                if !out.is_empty() {
                    out.push('\n');
                }
                out.push_str(&text);
            }
        }

        from = end + b"endstream".len();
        if out.chars().count() >= MAX_EXTRACTED_TEXT {
            break;
        }
    }

    let text = tidy(&out);
    legible(&text).then(|| cap(text))
}

/// The next `stream` keyword that is not the tail of `endstream`.
fn next_stream(bytes: &[u8], from: usize) -> Option<usize> {
    let mut pos = from;
    while let Some(at) = find(bytes, pos, b"stream") {
        if at >= 3 && &bytes[at - 3..at] == b"end" {
            pos = at + b"stream".len();
            continue;
        }
        return Some(at);
    }
    None
}

fn find(haystack: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= haystack.len() || needle.is_empty() {
        return None;
    }
    haystack[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

fn contains_ignore_ascii_case(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window.eq_ignore_ascii_case(needle))
}

/// Inflate a FlateDecode stream. PDFs use a zlib wrapper, but raw deflate is
/// seen from enough producers that the second attempt is worth the line.
fn inflate(raw: &[u8]) -> Option<Vec<u8>> {
    for zlib in [true, false] {
        let mut out = Vec::new();
        let mut reader: Box<dyn Read> = if zlib {
            Box::new(flate2::read::ZlibDecoder::new(raw))
        } else {
            Box::new(flate2::read::DeflateDecoder::new(raw))
        };
        if reader.read_to_end(&mut out).is_ok() && !out.is_empty() {
            return Some(out);
        }
    }
    None
}

/// Walk a content stream for the operators that put strings on the page.
///
/// Numbers between the strings of a `TJ` array are kerning: a jump wider than
/// a tenth of a space is a word gap, which is how `[(Hel) -150 (lo)]` becomes
/// "Hel lo".
fn text_from_content(data: &[u8]) -> Option<String> {
    let mut out = String::new();
    let mut pending: Vec<String> = Vec::new();
    let mut array_depth = 0usize;
    let mut i = 0;

    while i < data.len() {
        match data[i] {
            b'%' => {
                while i < data.len() && data[i] != b'\n' && data[i] != b'\r' {
                    i += 1;
                }
            }
            b'(' => {
                let (text, next) = literal_string(data, i);
                pending.push(text);
                i = next;
            }
            b'<' if data.get(i + 1) != Some(&b'<') => {
                let (text, next) = hex_string(data, i);
                pending.push(text);
                i = next;
            }
            b'[' => {
                array_depth += 1;
                i += 1;
            }
            b']' => {
                array_depth = array_depth.saturating_sub(1);
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let (value, next) = number_at(data, i);
                if array_depth > 0 && value < -100.0 {
                    pending.push(" ".to_string());
                }
                i = next;
            }
            byte if is_operator_byte(byte) => {
                let start = i;
                while i < data.len() && is_operator_byte(data[i]) {
                    i += 1;
                }
                match &data[start..i] {
                    b"Tj" | b"TJ" => flush(&mut out, &mut pending),
                    b"'" | b"\"" => {
                        newline(&mut out);
                        flush(&mut out, &mut pending);
                    }
                    b"ET" | b"T*" => {
                        newline(&mut out);
                        pending.clear();
                    }
                    _ => pending.clear(),
                }
            }
            _ => i += 1,
        }
    }

    flush(&mut out, &mut pending);
    (!out.trim().is_empty()).then_some(out)
}

fn is_operator_byte(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || matches!(byte, b'\'' | b'"' | b'*')
}

fn flush(out: &mut String, pending: &mut Vec<String>) {
    for text in pending.drain(..) {
        out.push_str(&text);
    }
}

fn newline(out: &mut String) {
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

/// A `( ... )` string, with balanced parentheses and backslash escapes.
fn literal_string(data: &[u8], start: usize) -> (String, usize) {
    let mut bytes = Vec::new();
    let mut depth = 0usize;
    let mut i = start;

    while i < data.len() {
        match data[i] {
            b'(' => {
                depth += 1;
                if depth > 1 {
                    bytes.push(b'(');
                }
                i += 1;
            }
            b')' => {
                depth -= 1;
                i += 1;
                if depth == 0 {
                    break;
                }
                bytes.push(b')');
            }
            b'\\' => {
                i += 1;
                let Some(&escape) = data.get(i) else {
                    break;
                };
                match escape {
                    b'n' => {
                        bytes.push(b'\n');
                        i += 1;
                    }
                    b'r' => {
                        bytes.push(b'\r');
                        i += 1;
                    }
                    b't' => {
                        bytes.push(b'\t');
                        i += 1;
                    }
                    b'b' => {
                        bytes.push(0x08);
                        i += 1;
                    }
                    b'f' => {
                        bytes.push(0x0c);
                        i += 1;
                    }
                    b'(' | b')' | b'\\' => {
                        bytes.push(escape);
                        i += 1;
                    }
                    b'\r' => {
                        i += 1;
                        if data.get(i) == Some(&b'\n') {
                            i += 1;
                        }
                    }
                    b'\n' => i += 1,
                    b'0'..=b'7' => {
                        let mut value = 0u16;
                        let mut digits = 0;
                        while digits < 3 {
                            let Some(&digit) = data.get(i) else {
                                break;
                            };
                            if !(b'0'..=b'7').contains(&digit) {
                                break;
                            }
                            value = value * 8 + (digit - b'0') as u16;
                            digits += 1;
                            i += 1;
                        }
                        bytes.push(value as u8);
                    }
                    other => {
                        bytes.push(other);
                        i += 1;
                    }
                }
            }
            byte => {
                bytes.push(byte);
                i += 1;
            }
        }
    }

    (decode(&bytes), i)
}

/// A `< ... >` hex string. Odd digit counts are padded with a trailing zero,
/// which is what the specification requires.
fn hex_string(data: &[u8], start: usize) -> (String, usize) {
    let mut digits = Vec::new();
    let mut i = start + 1;
    while i < data.len() {
        let byte = data[i];
        i += 1;
        if byte == b'>' {
            break;
        }
        if byte.is_ascii_whitespace() {
            continue;
        }
        if let Some(value) = (byte as char).to_digit(16) {
            digits.push(value as u8);
        }
    }
    if digits.len() % 2 == 1 {
        digits.push(0);
    }
    let bytes: Vec<u8> = digits
        .as_chunks::<2>()
        .0
        .iter()
        .map(|pair| (pair[0] << 4) | pair[1])
        .collect();
    (decode(&bytes), i)
}

/// Text with a leading UTF-16BE byte-order mark is decoded as UTF-16; anything
/// else is UTF-8 when it is valid and Latin-1 when it is not.
fn decode(bytes: &[u8]) -> String {
    let cleaned: Vec<u8> = bytes
        .iter()
        .copied()
        .filter(|&byte| byte == b'\n' || byte == b'\t' || byte >= 0x20)
        .collect();
    if cleaned.starts_with(&[0xfe, 0xff]) {
        let units: Vec<u16> = cleaned[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect();
        return String::from_utf16_lossy(&units);
    }
    match std::str::from_utf8(&cleaned) {
        Ok(text) => text.to_string(),
        Err(_) => cleaned.iter().map(|&byte| byte as char).collect(),
    }
}

fn number_at(data: &[u8], start: usize) -> (f32, usize) {
    let mut i = start;
    if matches!(data.get(i), Some(b'-') | Some(b'+')) {
        i += 1;
    }
    let mut seen_dot = false;
    while i < data.len() {
        match data[i] {
            b'0'..=b'9' => i += 1,
            b'.' if !seen_dot => {
                seen_dot = true;
                i += 1;
            }
            _ => break,
        }
    }
    let text = std::str::from_utf8(&data[start..i]).unwrap_or("0");
    (text.parse().unwrap_or(0.0), i)
}

/// Collapse the runs of spaces and blank lines a content stream leaves behind.
fn tidy(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut last = '\0';
    for ch in text.chars() {
        match ch {
            '\r' => continue,
            ' ' | '\t' => {
                if matches!(last, ' ' | '\n' | '\0') {
                    continue;
                }
                out.push(' ');
                last = ' ';
            }
            '\n' => {
                if matches!(last, '\n' | '\0') {
                    continue;
                }
                out.push('\n');
                last = '\n';
            }
            _ => {
                out.push(ch);
                last = ch;
            }
        }
    }
    out.trim().to_string()
}

/// Whether this reads as text rather than as bytes that were never text.
fn legible(text: &str) -> bool {
    let total = text.chars().count();
    if total < 8 {
        return false;
    }
    let readable = text
        .chars()
        .filter(|ch| {
            ch.is_alphanumeric() || ch.is_whitespace() || ".,;:!?()[]{}'\"-—/&%$#@+=*".contains(*ch)
        })
        .count();
    readable * 2 >= total
}

fn cap(text: String) -> String {
    if text.chars().count() <= MAX_EXTRACTED_TEXT {
        return text;
    }
    let mut capped: String = text.chars().take(MAX_EXTRACTED_TEXT).collect();
    capped.push_str("\n[PDF text truncated]");
    capped
}

#[cfg(test)]
fn pdf_with(content: &[u8]) -> Vec<u8> {
    let mut pdf = format!(
        "%PDF-1.4\n1 0 obj\n<< /Length {} >>\nstream\n",
        content.len()
    )
    .into_bytes();
    pdf.extend_from_slice(content);
    pdf.extend_from_slice(b"\nendstream\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
    pdf
}

/// A one-line PDF, for tests that need a document rather than a fixture.
#[cfg(test)]
pub(crate) fn sample_pdf(text: &str) -> Vec<u8> {
    pdf_with(format!("BT /F1 12 Tf 72 712 Td ({text}) Tj ET").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_uncompressed_stream_gives_back_its_text() {
        let pdf = sample_pdf("Hello from a PDF");
        assert_eq!(extract(&pdf).as_deref(), Some("Hello from a PDF"));
    }

    #[test]
    fn a_flate_compressed_stream_is_inflated() {
        use std::io::Write;
        let content = b"BT /F1 12 Tf 72 712 Td (Compressed words) Tj ET";
        let mut encoder =
            flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(content).unwrap();
        let compressed = encoder.finish().unwrap();

        let mut pdf = format!(
            "%PDF-1.4\n1 0 obj\n<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        pdf.extend_from_slice(&compressed);
        pdf.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");

        let text = extract(&pdf).expect("compressed text");
        assert_eq!(text, "Compressed words");
    }

    #[test]
    fn hex_strings_and_tj_kerning_are_joined() {
        let pdf = pdf_with(b"BT <48656c6c6f> Tj ET BT [(Wor) -200 (ld)] TJ ET");
        let text = extract(&pdf).expect("text");
        assert!(text.contains("Hello"), "{text:?}");
        assert!(text.contains("Wor ld"), "{text:?}");
    }

    #[test]
    fn bytes_that_were_never_text_are_refused() {
        assert!(extract(&[0xff; 200]).is_none(), "not a PDF at all");

        // A PDF whose only stream is binary: an image-only page has no text.
        let mut image_only =
            b"%PDF-1.4\n1 0 obj\n<< /Length 64 /Subtype /Image >>\nstream\n".to_vec();
        image_only.extend((0u8..64).map(|byte| byte.wrapping_mul(7)));
        image_only.extend_from_slice(b"\nendstream\nendobj\n%%EOF\n");
        assert!(extract(&image_only).is_none(), "binary is not text");
    }

    #[test]
    fn escaped_parentheses_and_newlines_survive() {
        let pdf = pdf_with(b"BT (one \\(two\\) three) Tj (four) ' ET");
        let text = extract(&pdf).expect("text");
        assert!(text.contains("one (two) three"), "{text:?}");
        assert!(text.contains("four"), "{text:?}");
    }
}
