//! Finds web links in terminal text and opens them in the default browser.

use std::process::{Command, Stdio};

/// Only web links are opened. Other schemes, such as `file://`, could launch
/// local programs, and the text may come from a remote server.
const SCHEMES: &[&str] = &["https://", "http://"];

pub fn is_openable(uri: &str) -> bool {
    let lower = uri.to_ascii_lowercase();
    SCHEMES
        .iter()
        .any(|scheme| lower.starts_with(scheme) && lower.len() > scheme.len())
}

fn is_url_char(c: char) -> bool {
    // Non-ASCII letters are kept for international domains and paths. Quotes
    // and symbols such as box drawing end the link.
    c.is_alphanumeric() || "-._~:/?#[]@!$&*+,;=%()".contains(c)
}

/// Removes punctuation that ends a sentence rather than the link, and
/// closing brackets that have no opening one inside the link.
fn trim_end(chars: &[char], start: usize, mut end: usize) -> usize {
    loop {
        let Some(&last) = chars[start..end].last() else {
            return end;
        };
        let unbalanced = |open: char, close: char| {
            let text = &chars[start..end];
            text.iter().filter(|&&c| c == close).count()
                > text.iter().filter(|&&c| c == open).count()
        };
        let drop = match last {
            '.' | ',' | ':' | ';' | '!' | '?' => true,
            ')' => unbalanced('(', ')'),
            ']' => unbalanced('[', ']'),
            _ => false,
        };
        if !drop {
            return end;
        }
        end -= 1;
    }
}

/// Every link in `chars`, as half-open index ranges.
pub fn find_urls(chars: &[char]) -> Vec<(usize, usize)> {
    let lower: Vec<char> = chars.iter().map(|c| c.to_ascii_lowercase()).collect();
    let mut found = Vec::new();
    let mut index = 0;
    while index < chars.len() {
        let scheme = SCHEMES.iter().find(|scheme| {
            let scheme: Vec<char> = scheme.chars().collect();
            lower[index..].starts_with(&scheme)
        });
        // A scheme only counts at the start of a word, so "xhttps://" doesn't.
        let at_word_start = index == 0 || !chars[index - 1].is_alphanumeric();
        let Some(scheme) = scheme.filter(|_| at_word_start) else {
            index += 1;
            continue;
        };
        let body = index + scheme.len();
        let mut end = body;
        while end < chars.len() && is_url_char(chars[end]) {
            end += 1;
        }
        let end = trim_end(chars, index, end);
        if end > body {
            found.push((index, end));
            index = end;
        } else {
            index = body;
        }
    }
    found
}

/// Opens `url` with the system's default handler. Returns an error message
/// if the opener couldn't be started.
pub fn open(url: &str) -> Result<(), String> {
    if !is_openable(url) {
        return Err(format!(
            "Not opening {url}: only http and https links are opened"
        ));
    }
    let program = if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    let mut child = Command::new(program)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("Could not open the link with {program}: {e}"))?;
    // Reaped in the background so it doesn't linger as a zombie.
    std::thread::spawn(move || child.wait());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn urls(text: &str) -> Vec<String> {
        let chars: Vec<char> = text.chars().collect();
        find_urls(&chars)
            .into_iter()
            .map(|(start, end)| chars[start..end].iter().collect())
            .collect()
    }

    #[test]
    fn finds_links_in_text() {
        assert_eq!(
            urls("see https://example.com/a?b=1#c and http://x.org"),
            ["https://example.com/a?b=1#c", "http://x.org"]
        );
    }

    #[test]
    fn trims_sentence_punctuation() {
        assert_eq!(urls("Go to https://example.com."), ["https://example.com"]);
        assert_eq!(urls("(https://example.com/x)"), ["https://example.com/x"]);
        assert_eq!(
            urls("https://en.wikipedia.org/wiki/Rust_(language)"),
            ["https://en.wikipedia.org/wiki/Rust_(language)"]
        );
    }

    #[test]
    fn stops_at_quotes_and_box_drawing() {
        assert_eq!(urls("\"https://a.io/x\""), ["https://a.io/x"]);
        assert_eq!(urls("│https://a.io│"), ["https://a.io"]);
    }

    #[test]
    fn ignores_bare_schemes_and_mid_word_matches() {
        assert!(urls("https:// nothing").is_empty());
        assert!(urls("xhttps://example.com").is_empty());
    }

    #[test]
    fn only_web_links_are_opened() {
        assert!(is_openable("HTTPS://example.com"));
        assert!(!is_openable("file:///etc/passwd"));
        assert!(!is_openable("javascript:alert(1)"));
        assert!(!is_openable("https://"));
    }
}
