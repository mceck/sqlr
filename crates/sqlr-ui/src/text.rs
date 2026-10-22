//! Small text helpers the widgets and the app share.

use std::borrow::Cow;

/// The first non-blank line of `text`, trimmed, cut to `max_chars` chars with "…".
pub fn first_line(text: &str, max_chars: usize) -> Cow<'_, str> {
    let line = text
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    match line.char_indices().nth(max_chars) {
        Some((cut, _)) => Cow::Owned(format!("{}…", &line[..cut])),
        None => Cow::Borrowed(line),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_line_skips_blank_lines_trims_and_cuts() {
        assert_eq!(first_line("\n  \n  select 1  \nfrom t", 80), "select 1");
        let long = "é".repeat(300);
        let cut = first_line(&long, 200);
        assert_eq!(cut.chars().count(), 201);
        assert!(cut.ends_with('…'));
        assert_eq!(first_line("x", 1), "x");
        assert_eq!(first_line("", 5), "");
    }
}
