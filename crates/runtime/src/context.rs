//! Context budgeting.
//!
//! Until the backend reports real token counts we estimate (~4 bytes/token).
//! History is selected newest-first so the most recent exchange always fits;
//! the system prompt is always kept. Dropped messages are reported to the
//! user rather than silently discarded.

use common::{Message, Role};

pub fn estimate_tokens(s: &str) -> u32 {
    (s.len() as u32).div_ceil(4) + 4
}

pub fn message_tokens(m: &Message) -> u32 {
    estimate_tokens(&m.content) + m.tool_calls.iter().map(|c| estimate_tokens(&c.arguments) + 8).sum::<u32>()
}

/// Returns the messages to send and how many history messages were left out.
pub fn select(system: Option<&Message>, history: &[Message], budget: u32) -> (Vec<Message>, usize) {
    let mut used = system.map(message_tokens).unwrap_or(0);
    let mut picked: Vec<&Message> = Vec::new();
    for (i, m) in history.iter().enumerate().rev() {
        let t = message_tokens(m);
        let is_latest = i == history.len() - 1;
        if !is_latest && used + t > budget {
            break;
        }
        used += t;
        picked.push(m);
    }
    // Never start the window with an orphaned tool result or assistant turn.
    while picked.len() > 1 && picked.last().is_some_and(|m| m.role != Role::User) {
        picked.pop();
    }
    let dropped = history.len() - picked.len();
    let mut out: Vec<Message> = system.into_iter().cloned().collect();
    out.extend(picked.into_iter().rev().cloned());
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_everything_when_it_fits() {
        let h = vec![Message::user("a"), Message::assistant("b"), Message::user("c")];
        let (out, dropped) = select(Some(&Message::system("s")), &h, 1000);
        assert_eq!(out.len(), 4);
        assert_eq!(dropped, 0);
    }

    #[test]
    fn drops_oldest_and_starts_at_a_user_turn() {
        let big = "x".repeat(400); // ~104 tokens
        let h = vec![
            Message::user(big.clone()),
            Message::assistant(big.clone()),
            Message::user(big.clone()),
            Message::assistant(big.clone()),
            Message::user("latest"),
        ];
        let (out, dropped) = select(Some(&Message::system("s")), &h, 250);
        assert_eq!(out[0].role, Role::System);
        assert_eq!(out[1].role, Role::User);
        assert_eq!(out.last().unwrap().content, "latest");
        assert!(dropped >= 2);
    }

    #[test]
    fn latest_message_is_always_sent() {
        let h = vec![Message::user("y".repeat(10_000))];
        let (out, dropped) = select(None, &h, 10);
        assert_eq!(out.len(), 1);
        assert_eq!(dropped, 0);
    }
}
