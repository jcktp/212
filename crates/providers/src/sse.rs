//! Incremental Server-Sent Events parser. Robust to events split across
//! arbitrary chunk boundaries and to CRLF line endings.

#[derive(Debug, Default)]
pub struct SseParser {
    buf: Vec<u8>,
    event: Option<String>,
    data: String,
    has_data: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SseEvent {
    pub event: Option<String>,
    pub data: String,
}

impl SseParser {
    pub fn push(&mut self, chunk: &[u8]) -> Vec<SseEvent> {
        self.buf.extend_from_slice(chunk);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let mut line: Vec<u8> = self.buf.drain(..=pos).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            let line = String::from_utf8_lossy(&line);
            if line.is_empty() {
                if self.has_data {
                    out.push(SseEvent { event: self.event.take(), data: std::mem::take(&mut self.data) });
                    self.has_data = false;
                } else {
                    self.event = None;
                }
                continue;
            }
            if line.starts_with(':') {
                continue;
            }
            let (field, value) = match line.split_once(':') {
                Some((f, v)) => (f, v.strip_prefix(' ').unwrap_or(v)),
                None => (line.as_ref(), ""),
            };
            match field {
                "event" => self.event = Some(value.to_string()),
                "data" => {
                    if self.has_data {
                        self.data.push('\n');
                    }
                    self.data.push_str(value);
                    self.has_data = true;
                }
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_split_events() {
        let mut p = SseParser::default();
        let input = b"data: {\"a\":1}\r\n\r\nevent: ping\ndata: x\ndata: y\n\n: comment\ndata: [DONE]\n\n";
        let mut events = Vec::new();
        for c in input.chunks(3) {
            events.extend(p.push(c));
        }
        assert_eq!(
            events,
            vec![
                SseEvent { event: None, data: "{\"a\":1}".into() },
                SseEvent { event: Some("ping".into()), data: "x\ny".into() },
                SseEvent { event: None, data: "[DONE]".into() },
            ]
        );
    }

    #[test]
    fn multibyte_utf8_across_chunks() {
        let mut p = SseParser::default();
        let s = "data: héllo ▓▒░\n\n".as_bytes();
        let mut ev = Vec::new();
        for b in s {
            ev.extend(p.push(&[*b]));
        }
        assert_eq!(ev[0].data, "héllo ▓▒░");
    }
}
