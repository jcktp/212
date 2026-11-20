//! Moves inline thinking out of the answer text.
//!
//! Some local models (Gemma 4's `<|channel>thought … <channel|>`, Qwen/DeepSeek
//! `<think> … </think>`) sometimes stream their reasoning as ordinary content
//! when the server's reasoning parser doesn't catch it. This splits a text
//! stream into answer and reasoning parts, coping with markers that are cut
//! across chunks.

const PAIRS: &[(&str, &str)] = &[("<|channel>thought", "<channel|>"), ("<think>", "</think>")];

#[derive(Debug, Clone, PartialEq)]
pub enum Part {
    Text(String),
    Reasoning(String),
}

#[derive(Debug, Default)]
pub struct ThinkSplit {
    buf: String,
    /// Closing marker we are waiting for while inside a thought block.
    inside: Option<&'static str>,
}

impl ThinkSplit {
    pub fn push(&mut self, chunk: &str) -> Vec<Part> {
        self.buf.push_str(chunk);
        let mut out = Vec::new();
        loop {
            match self.inside {
                Some(close) => match self.buf.find(close) {
                    Some(i) => {
                        emit(&mut out, Part::Reasoning(self.buf[..i].to_string()));
                        self.buf.drain(..i + close.len());
                        self.inside = None;
                    }
                    None => {
                        let keep = partial_suffix(&self.buf, &[close]);
                        let cut = self.buf.len() - keep;
                        emit(&mut out, Part::Reasoning(self.buf[..cut].to_string()));
                        self.buf.drain(..cut);
                        return out;
                    }
                },
                None => {
                    let hit = PAIRS.iter().filter_map(|(o, c)| self.buf.find(o).map(|i| (i, *o, *c))).min_by_key(|h| h.0);
                    match hit {
                        Some((i, open, close)) => {
                            emit(&mut out, Part::Text(self.buf[..i].to_string()));
                            self.buf.drain(..i + open.len());
                            self.inside = Some(close);
                        }
                        None => {
                            let opens: Vec<&str> = PAIRS.iter().map(|p| p.0).collect();
                            let keep = partial_suffix(&self.buf, &opens);
                            let cut = self.buf.len() - keep;
                            emit(&mut out, Part::Text(self.buf[..cut].to_string()));
                            self.buf.drain(..cut);
                            return out;
                        }
                    }
                }
            }
        }
    }

    /// Release whatever is held back at the end of the stream.
    pub fn finish(&mut self) -> Vec<Part> {
        let rest = std::mem::take(&mut self.buf);
        let mut out = Vec::new();
        emit(&mut out, if self.inside.is_some() { Part::Reasoning(rest) } else { Part::Text(rest) });
        self.inside = None;
        out
    }
}

fn emit(out: &mut Vec<Part>, p: Part) {
    let empty = match &p {
        Part::Text(t) | Part::Reasoning(t) => t.is_empty(),
    };
    if !empty {
        out.push(p);
    }
}

/// Length of the longest suffix of `s` that is a proper prefix of a marker.
fn partial_suffix(s: &str, markers: &[&str]) -> usize {
    let mut best = 0;
    for m in markers {
        for n in (1..m.len()).rev() {
            if n > best && s.len() >= n && s.is_char_boundary(s.len() - n) && m.starts_with(&s[s.len() - n..]) {
                best = n;
                break;
            }
        }
    }
    best
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(chunks: &[&str]) -> (String, String) {
        let mut sp = ThinkSplit::default();
        let mut parts: Vec<Part> = chunks.iter().flat_map(|c| sp.push(c)).collect();
        parts.extend(sp.finish());
        let (mut t, mut r) = (String::new(), String::new());
        for p in parts {
            match p {
                Part::Text(x) => t.push_str(&x),
                Part::Reasoning(x) => r.push_str(&x),
            }
        }
        (t, r)
    }

    #[test]
    fn splits_inline_thoughts_across_chunks() {
        assert_eq!(run(&["Hello ", "world"]), ("Hello world".into(), String::new()));
        assert_eq!(run(&["<|chan", "nel>thought\nplan it", "<chan", "nel|>Answer"]), ("Answer".into(), "\nplan it".into()));
        assert_eq!(run(&["a<think>x</think>b<think>y"]), ("ab".into(), "xy".into()));
        assert_eq!(run(&["1 < 2 and <b>bold</b> <"]), ("1 < 2 and <b>bold</b> <".into(), String::new()));
        assert_eq!(run(&["héllo <thi", "nk>ü</think>é"]), ("héllo é".into(), "ü".into()));
    }
}
