use std::cmp::Ordering;

use crate::model::RemoteVersion;

#[derive(Debug, Clone, Copy)]
enum VersionToken<'a> {
    Number(&'a str),
    Text(&'a str),
}

pub fn sort_remote_versions_descending(versions: &mut [RemoteVersion]) {
    versions.sort_by(|left, right| {
        compare_version_strings(&right.version, &left.version)
            .then_with(|| right.published_at.cmp(&left.published_at))
    });
}

fn compare_version_strings(left: &str, right: &str) -> Ordering {
    let left_tokens = tokenize(left);
    let right_tokens = tokenize(right);
    let shared = left_tokens.len().min(right_tokens.len());

    for index in 0..shared {
        let ordering = compare_tokens(left_tokens[index], right_tokens[index]);
        if ordering != Ordering::Equal {
            return ordering;
        }
    }

    match left_tokens.len().cmp(&right_tokens.len()) {
        Ordering::Equal => left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase()),
        Ordering::Less => compare_ended_version(&right_tokens[shared..]).reverse(),
        Ordering::Greater => compare_ended_version(&left_tokens[shared..]),
    }
}

fn tokenize(version: &str) -> Vec<VersionToken<'_>> {
    let bytes = version.as_bytes();
    let start = bytes
        .iter()
        .position(u8::is_ascii_digit)
        .unwrap_or(bytes.len());
    let mut tokens = Vec::new();
    let mut index = start;

    while index < bytes.len() {
        if bytes[index].is_ascii_digit() {
            let token_start = index;
            while index < bytes.len() && bytes[index].is_ascii_digit() {
                index += 1;
            }
            tokens.push(VersionToken::Number(&version[token_start..index]));
        } else if bytes[index].is_ascii_alphabetic() {
            let token_start = index;
            while index < bytes.len() && bytes[index].is_ascii_alphabetic() {
                index += 1;
            }
            tokens.push(VersionToken::Text(&version[token_start..index]));
        } else {
            index += 1;
        }
    }

    tokens
}

fn compare_tokens(left: VersionToken<'_>, right: VersionToken<'_>) -> Ordering {
    match (left, right) {
        (VersionToken::Number(left), VersionToken::Number(right)) => {
            compare_numeric_text(left, right)
        }
        (VersionToken::Text(left), VersionToken::Text(right)) => qualifier_rank(left)
            .cmp(&qualifier_rank(right))
            .then_with(|| left.to_ascii_lowercase().cmp(&right.to_ascii_lowercase())),
        (VersionToken::Number(_), VersionToken::Text(_)) => Ordering::Greater,
        (VersionToken::Text(_), VersionToken::Number(_)) => Ordering::Less,
    }
}

fn compare_numeric_text(left: &str, right: &str) -> Ordering {
    let left = left.trim_start_matches('0');
    let right = right.trim_start_matches('0');
    let left = if left.is_empty() { "0" } else { left };
    let right = if right.is_empty() { "0" } else { right };
    left.len().cmp(&right.len()).then_with(|| left.cmp(right))
}

fn compare_ended_version(remaining: &[VersionToken<'_>]) -> Ordering {
    for token in remaining {
        match token {
            VersionToken::Number(number)
                if compare_numeric_text(number, "0") == Ordering::Equal => {}
            VersionToken::Number(_) => return Ordering::Greater,
            VersionToken::Text(text) if qualifier_rank(text) < qualifier_rank("stable") => {
                return Ordering::Less;
            }
            VersionToken::Text(text)
                if matches!(
                    text.to_ascii_lowercase().as_str(),
                    "stable" | "final" | "release"
                ) => {}
            VersionToken::Text(_) => return Ordering::Less,
        }
    }
    Ordering::Equal
}

fn qualifier_rank(value: &str) -> u8 {
    match value.to_ascii_lowercase().as_str() {
        "dev" | "snapshot" | "nightly" => 0,
        "a" | "alpha" => 1,
        "b" | "beta" => 2,
        "pre" | "preview" => 3,
        "rc" | "candidate" => 4,
        "stable" | "final" | "release" => 5,
        _ => 3,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn remote(version: &str) -> RemoteVersion {
        RemoteVersion {
            version: version.to_string(),
            channel: "stable".to_string(),
            published_at: None,
            architecture: "x86_64".to_string(),
            download_url: None,
            checksum_algorithm: None,
            checksum: None,
            notes_url: None,
        }
    }

    fn sorted(input: &[&str]) -> Vec<String> {
        let mut versions = input
            .iter()
            .map(|version| remote(version))
            .collect::<Vec<_>>();
        sort_remote_versions_descending(&mut versions);
        versions
            .into_iter()
            .map(|version| version.version)
            .collect()
    }

    #[test]
    fn sorts_numeric_versions_descending_instead_of_by_string_or_date() {
        assert_eq!(
            sorted(&["3.13.14", "3.14.6", "3.9.20", "3.14.5"]),
            ["3.14.6", "3.14.5", "3.13.14", "3.9.20"]
        );
    }

    #[test]
    fn places_stable_versions_before_release_candidates() {
        assert_eq!(
            sorted(&["3.14.0rc2", "3.14.0", "3.14.0b3", "3.14.0rc10"]),
            ["3.14.0", "3.14.0rc10", "3.14.0rc2", "3.14.0b3"]
        );
    }

    #[test]
    fn compares_large_numeric_build_and_installer_revisions() {
        assert_eq!(
            sorted(&["21.0.8+9", "21.0.8+10", "21.0.7", "21.0.8-1"]),
            ["21.0.8+10", "21.0.8+9", "21.0.8-1", "21.0.7"]
        );
    }
}
