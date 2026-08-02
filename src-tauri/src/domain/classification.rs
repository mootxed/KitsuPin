use regex::Regex;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use url::Url;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ContentType {
    Text,
    Links,
    Email,
    Numbers,
}

impl ContentType {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Links => "Links",
            Self::Email => "Email",
            Self::Numbers => "Numbers",
        }
    }
}

pub fn classify(content: &str) -> ContentType {
    let value = content.trim();
    static EMAIL: OnceLock<Regex> = OnceLock::new();
    static NUMBER: OnceLock<Regex> = OnceLock::new();
    let email = EMAIL.get_or_init(|| Regex::new(r"(?i)^[a-z0-9.!#$%&'*+/=?^_`{|}~-]+@[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?(?:\.[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?)+$").unwrap());
    let number = NUMBER.get_or_init(|| Regex::new(r"^[+-]?(?:\d+(?:[.,]\d+)?|[.,]\d+)$").unwrap());
    if email.is_match(value) {
        return ContentType::Email;
    }
    if Url::parse(value).is_ok_and(|u| {
        matches!(
            u.scheme(),
            "http" | "https" | "chrome" | "edge" | "about" | "file" | "ftp"
        )
    }) {
        return ContentType::Links;
    }
    if number.is_match(value) {
        return ContentType::Numbers;
    }
    ContentType::Text
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn classifies_all_types() {
        assert_eq!(classify("mail@example.com"), ContentType::Email);
        assert_eq!(classify("https://example.com/a?q=1"), ContentType::Links);
        assert_eq!(classify(" -12.50 "), ContentType::Numbers);
        assert_eq!(classify("echo 42"), ContentType::Text);
        assert_eq!(classify("read example.com"), ContentType::Text);
    }

    #[test]
    fn classifies_extended_url_schemes() {
        assert_eq!(
            classify("https://github.com/mootxed/KitsuPin"),
            ContentType::Links
        );
        assert_eq!(classify("chrome://extensions/?id=abc"), ContentType::Links);
        assert_eq!(classify("edge://extensions/"), ContentType::Links);
        assert_eq!(classify("about:blank"), ContentType::Links);
        assert_eq!(classify("file:///home/user/file.txt"), ContentType::Links);
        assert_eq!(classify("ftp://example.com/file"), ContentType::Links);

        assert_eq!(classify("example.com"), ContentType::Text);
        assert_eq!(classify("read example.com"), ContentType::Text);
        assert_eq!(classify("C:\\Users\\Test"), ContentType::Text);
        assert_eq!(classify("text: value"), ContentType::Text);
    }
}
