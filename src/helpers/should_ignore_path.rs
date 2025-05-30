use lazy_static::lazy_static;
use regex::Regex;

const ALLOWED_EXTS: [&str; 11] = [
    ".mkv", ".mp4", ".avi", ".mov", ".wmv", ".flv", ".webm", ".mpeg", ".mpg", // video files
    ".srt", ".sub", // subtitles
];

lazy_static! {
    static ref PART_FILTERS: Vec<Regex> = vec![
        Regex::new(r"^lore$").unwrap(),
        Regex::new(r"^histories(( and| &) lore)?$").unwrap(),
        Regex::new(r"sample").unwrap(),
        Regex::new(r"^behind.the.scenes$").unwrap(),
        Regex::new(r"^deleted.and.extended.scenes$").unwrap(),
        Regex::new(r"^deleted.scenes$").unwrap(),
        Regex::new(r"^extras?$").unwrap(),
        Regex::new(r"^featurettes$").unwrap(),
        Regex::new(r"^interviews$").unwrap(),
        Regex::new(r"^scenes$").unwrap(),
        Regex::new(r"^shorts$").unwrap(),
    ];
}

pub fn should_ignore_path(input: &str) -> bool {
    if !ALLOWED_EXTS.iter().any(|ext| input.ends_with(ext)) {
        return true;
    }

    // todo: these should be optional, some people might want to keep these,
    // this is more of a hold over from the old implementation.
    let path_parts = input.split('/');
    for path_part in path_parts {
        if path_part.is_empty() {
            continue;
        }

        let path_part = path_part.to_lowercase();
        let is_filtered = PART_FILTERS.iter().any(|regex| regex.is_match(&path_part));
        if is_filtered {
            return true;
        }
    }

    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_should_ignore_path() {
        assert_eq!(should_ignore_path("torrent/samples/video.mp4"), true);
        assert_eq!(
            should_ignore_path("trailer park boys/season 1/episode 1.mp4"),
            false
        );
    }
}
