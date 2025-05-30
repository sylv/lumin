pub struct ParsedMagnet {
    pub hash: String,
    pub name: Option<String>,
}

pub fn parse_magnet_uri(magnet_uri: &str) -> Option<ParsedMagnet> {
    let parts = url::Url::parse(magnet_uri).ok()?;
    let mut hash = None;
    let mut name = None;
    for (key, value) in parts.query_pairs() {
        match key.as_ref() {
            "xt" if value.starts_with("urn:btih:") => {
                hash = Some(value[9..].to_lowercase().to_string());
            }
            "dn" => {
                name = Some(value.to_string());
            }
            _ => {}
        }
    }

    if let Some(hash) = hash {
        Some(ParsedMagnet { hash, name })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_magnet_uri() {
        let magnet = "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678&dn=example_file";
        let parsed = parse_magnet_uri(magnet).unwrap();
        assert_eq!(parsed.hash, "1234567890abcdef1234567890abcdef12345678");
        assert_eq!(parsed.name, Some("example_file".to_string()));
    }

    #[test]
    fn test_parse_magnet_uri_without_name() {
        let magnet = "magnet:?xt=urn:btih:1234567890abcdef1234567890abcdef12345678";
        let parsed = parse_magnet_uri(magnet).unwrap();
        assert_eq!(parsed.hash, "1234567890abcdef1234567890abcdef12345678");
        assert!(parsed.name.is_none());
    }

    #[test]
    fn test_parse_invalid_magnet_uri() {
        let magnet = "invalid_magnet_uri";
        assert!(parse_magnet_uri(magnet).is_none());
    }
}
