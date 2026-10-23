//! `Content-Length` framing for JSON-RPC over stdio (LSP base protocol).

/// Upper bound for one message body; protects against a corrupt header.
pub const MAX_BODY: usize = 256 * 1024 * 1024;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum FramingError {
    #[error("header is not valid UTF-8")]
    NonUtf8Header,
    #[error("malformed header line `{0}`")]
    MalformedHeader(String),
    #[error("missing Content-Length header")]
    MissingContentLength,
    #[error("Content-Length {0} exceeds limit")]
    TooLarge(usize),
}

/// Frame one message body.
pub fn encode(body: &[u8]) -> Vec<u8> {
    let mut out = format!("Content-Length: {}\r\n\r\n", body.len()).into_bytes();
    out.extend_from_slice(body);
    out
}

/// Incremental decoder: feed arbitrary chunks with [`push`](Self::push), drain complete
/// bodies with [`next_frame`](Self::next_frame). Handles split and merged reads.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    buf: Vec<u8>,
    /// Parsed header of the frame currently being received: (header_len, body_len).
    pending: Option<(usize, usize)>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Bytes buffered but not yet returned as a frame.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    pub fn next_frame(&mut self) -> Result<Option<Vec<u8>>, FramingError> {
        let (header_len, body_len) = match self.pending {
            Some(p) => p,
            None => {
                let Some(end) = find(&self.buf, b"\r\n\r\n") else {
                    return Ok(None);
                };
                let header_len = end + 4;
                let body_len = parse_header(&self.buf[..end])?;
                self.pending = Some((header_len, body_len));
                (header_len, body_len)
            }
        };
        if self.buf.len() < header_len + body_len {
            return Ok(None);
        }
        let body = self.buf[header_len..header_len + body_len].to_vec();
        self.buf.drain(..header_len + body_len);
        self.pending = None;
        Ok(Some(body))
    }
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

fn parse_header(raw: &[u8]) -> Result<usize, FramingError> {
    let text = std::str::from_utf8(raw).map_err(|_| FramingError::NonUtf8Header)?;
    let mut len = None;
    for line in text.split("\r\n") {
        let (name, value) =
            line.split_once(':').ok_or_else(|| FramingError::MalformedHeader(line.to_string()))?;
        if name.trim().eq_ignore_ascii_case("content-length") {
            let n: usize = value
                .trim()
                .parse()
                .map_err(|_| FramingError::MalformedHeader(line.to_string()))?;
            if n > MAX_BODY {
                return Err(FramingError::TooLarge(n));
            }
            len = Some(n);
        }
    }
    len.ok_or(FramingError::MissingContentLength)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn drain(d: &mut FrameDecoder) -> Vec<String> {
        let mut out = vec![];
        while let Some(f) = d.next_frame().expect("frame") {
            out.push(String::from_utf8(f).expect("utf8"));
        }
        out
    }

    #[test]
    fn roundtrip_single() {
        let mut d = FrameDecoder::new();
        d.push(&encode(br#"{"a":1}"#));
        assert_eq!(drain(&mut d), vec![r#"{"a":1}"#]);
        assert_eq!(d.buffered(), 0);
    }

    #[test]
    fn split_reads_byte_by_byte() {
        let msg = encode("{\"text\":\"héllo 😀\"}".as_bytes());
        let mut d = FrameDecoder::new();
        let mut got = vec![];
        for b in &msg {
            d.push(std::slice::from_ref(b));
            got.extend(drain(&mut d));
        }
        assert_eq!(got, vec!["{\"text\":\"héllo 😀\"}"]);
    }

    #[test]
    fn merged_reads_and_partial_tail() {
        let mut all = encode(b"[1]");
        all.extend(encode(b"[2,3]"));
        let third = encode(b"{\"x\":null}");
        all.extend_from_slice(&third[..7]);
        let mut d = FrameDecoder::new();
        d.push(&all);
        assert_eq!(drain(&mut d), vec!["[1]", "[2,3]"]);
        d.push(&third[7..]);
        assert_eq!(drain(&mut d), vec!["{\"x\":null}"]);
    }

    #[test]
    fn extra_headers_and_case() {
        let mut d = FrameDecoder::new();
        d.push(b"content-length: 2\r\nContent-Type: application/vscode-jsonrpc; charset=utf-8\r\n\r\n{}");
        assert_eq!(drain(&mut d), vec!["{}"]);
    }

    #[test]
    fn errors() {
        let mut d = FrameDecoder::new();
        d.push(b"Content-Type: x\r\n\r\n{}");
        assert_eq!(d.next_frame(), Err(FramingError::MissingContentLength));
        let mut d = FrameDecoder::new();
        d.push(b"garbage\r\n\r\n");
        assert!(matches!(d.next_frame(), Err(FramingError::MalformedHeader(_))));
        let mut d = FrameDecoder::new();
        d.push(b"Content-Length: abc\r\n\r\n");
        assert!(matches!(d.next_frame(), Err(FramingError::MalformedHeader(_))));
    }
}
