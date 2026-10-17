// Capabilities of the official historical distribution, independent of the
// latest Python layout. Missing a feature that did not yet exist is not damage.
pub(crate) fn has_bundled_pip(version: &str) -> bool {
    let Some((major, minor, patch)) = release_numbers(version) else {
        return false;
    };
    major > 3 || (major == 3 && minor >= 4) || (major == 2 && minor == 7 && patch >= 9)
}

pub(crate) fn has_builtin_venv(version: &str) -> bool {
    release_numbers(version)
        .is_some_and(|(major, minor, _)| major > 3 || (major == 3 && minor >= 3))
}

fn release_numbers(version: &str) -> Option<(u32, u32, u32)> {
    let mut parts = version.split('.');
    let major = parts.next()?.parse().ok()?;
    let minor = parts.next()?.parse().ok()?;
    let patch = parts.next().unwrap_or("0");
    let digits = patch.bytes().take_while(u8::is_ascii_digit).count();
    let patch = patch.get(..digits)?.parse().ok()?;
    Some((major, minor, patch))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn historical_features_follow_their_introduction_versions() {
        for (version, pip, venv) in [
            ("1.6.1", false, false),
            ("2.7.8", false, false),
            ("2.7.9", true, false),
            ("2.7.18", true, false),
            ("3.2.5", false, false),
            ("3.3.5", false, true),
            ("3.4.0", true, true),
            ("3.10.11", true, true),
            ("3.15.0rc2", true, true),
        ] {
            assert_eq!(has_bundled_pip(version), pip, "{version}");
            assert_eq!(has_builtin_venv(version), venv, "{version}");
        }
        assert!(!has_bundled_pip("unknown"));
    }
}
