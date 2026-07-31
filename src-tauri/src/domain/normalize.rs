use sha2::{Digest, Sha256};

pub fn normalize_content(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}

pub fn content_hash(normalized: &str) -> String {
    hex::encode(Sha256::digest(normalized.as_bytes()))
}

pub fn normalize_domain(value: &str) -> Option<String> {
    let normalized = value.trim().trim_end_matches('.').to_lowercase();
    let normalized = normalized.strip_prefix("www.").unwrap_or(&normalized);
    if normalized.is_empty() || normalized.len() > 253 || normalized.contains(['/', ':', ' ', '\\'])
    {
        return None;
    }
    if normalized.split('.').any(|part| {
        part.is_empty()
            || part.starts_with('-')
            || part.ends_with('-')
            || !part.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    }) {
        return None;
    }
    Some(normalized.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn normalizes_line_endings_and_outer_space() {
        assert_eq!(normalize_content("  a\r\nb  "), "a\nb");
    }
    #[test]
    fn normalizes_domains_consistently() {
        assert_eq!(
            normalize_domain(" WWW.YouTube.COM. ").as_deref(),
            Some("youtube.com")
        );
        assert_eq!(normalize_domain("https://example.com"), None);
    }
}
