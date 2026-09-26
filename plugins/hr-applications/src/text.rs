//! Text in and out: a question's choices as typed, and long answers cut to
//! what one value on a page may hold.

/// Most choices a question has, and how long one may be.
pub const MAX_CHOICES: usize = 20;
pub const MAX_CHOICE: usize = 200;

/// One choice per line, blank lines skipped; none means a written answer.
pub fn parse_choices(text: &str) -> Result<Vec<String>, String> {
    let choices: Vec<String> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect();
    if choices.len() > MAX_CHOICES {
        return Err(format!("A question has at most {MAX_CHOICES} choices."));
    }
    if let Some(long) = choices.iter().find(|c| c.chars().count() > MAX_CHOICE) {
        let start: String = long.chars().take(30).collect();
        return Err(format!(
            "\"{start}…\" is too long: a choice is at most {MAX_CHOICE} characters."
        ));
    }
    Ok(choices)
}

/// `text` in pieces of at most `max` bytes, cut between characters (a
/// page value holds at most 2 KiB).
pub fn pieces(text: &str, max: usize) -> Vec<&str> {
    let mut out = Vec::new();
    let mut rest = text;
    while rest.len() > max {
        let mut cut = max;
        while !rest.is_char_boundary(cut) {
            cut -= 1;
        }
        let (piece, tail) = rest.split_at(cut);
        out.push(piece);
        rest = tail;
    }
    if !rest.is_empty() || out.is_empty() {
        out.push(rest);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn choices_are_one_per_line() {
        assert_eq!(
            parse_choices("  EU \n\nUS\r\nAU\n"),
            Ok(vec!["EU".to_owned(), "US".to_owned(), "AU".to_owned()])
        );
        assert_eq!(parse_choices(" \n "), Ok(vec![]));
        assert!(parse_choices(&"x\n".repeat(MAX_CHOICES + 1)).is_err());
        assert!(parse_choices(&"x".repeat(MAX_CHOICE + 1)).is_err());
    }

    #[test]
    fn long_text_is_cut_between_characters() {
        assert_eq!(pieces("", 4), vec![""]);
        assert_eq!(pieces("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // "é" is two bytes: never split in half.
        let cut = pieces("aéééé", 4);
        assert_eq!(cut.concat(), "aéééé");
        assert!(cut.iter().all(|p| p.len() <= 4));
    }
}
