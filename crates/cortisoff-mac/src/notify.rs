//! `notify` through `osascript`, the one way a process with no bundle can
//! show a notification without a framework that wants an app identity.

use std::process::{Command, Stdio};

/// Text made safe inside an AppleScript string literal. A backslash or a
/// double quote would end the literal early, so each gets a backslash. Line
/// breaks and tabs are written as their escapes, which keeps the whole
/// script on one line.
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            // Any other control character is noise in a banner and can
            // only confuse the parser.
            c if c.is_control() => {}
            c => out.push(c),
        }
    }
    out
}

pub fn script(title: &str, body: &str) -> String {
    format!(
        "display notification \"{}\" with title \"{}\"",
        escape(body),
        escape(title)
    )
}

/// Spawns the banner and returns. The script goes in as one argument, never
/// through a shell, so escaping for AppleScript is the only escaping there
/// is. The child is reaped on a thread of its own, because the caller is the
/// daemon's one loop and a banner is not worth blocking it for.
pub fn send(title: &str, body: &str) -> Result<(), String> {
    let mut child = Command::new("/usr/bin/osascript")
        .arg("-e")
        .arg(script(title, body))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start osascript for the notification: {e}"))?;
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quotes_and_backslashes_cannot_end_the_string() {
        assert_eq!(escape(r#"say "hi" \o/"#), r#"say \"hi\" \\o/"#);
        assert_eq!(
            script("Night \"light\"", "on\tat 20:00\nuntil 7:00"),
            r#"display notification "on\tat 20:00\nuntil 7:00" with title "Night \"light\"""#
        );
    }

    /// An injection attempt stays inside the literal: the quote is escaped,
    /// so `do shell script` is text in a banner, not a command.
    #[test]
    fn an_injection_stays_text() {
        let s = script("x", "\" & (do shell script \"rm -rf ~\") & \"");
        assert_eq!(
            s,
            r#"display notification "\" & (do shell script \"rm -rf ~\") & \"" with title "x""#
        );
        assert_eq!(s.matches('"').count() - s.matches("\\\"").count(), 4);
    }

    #[test]
    fn control_characters_are_dropped_and_text_is_kept() {
        assert_eq!(escape("a\u{7}b\u{0}c"), "abc");
        assert_eq!(escape("café 3400 K"), "café 3400 K");
    }
}
