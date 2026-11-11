//! Versions, and the names of the branches and tags made from them.

use std::fmt;
use std::str::FromStr;

/// A version of a repository, `X.Y.Z`, which names its development branch, `vX.Y.Z`, and its release tag,
/// `releaseX.Y.Z`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Version {
    pub major: u64,
    pub minor: u64,
    pub patch: u64,
}

impl Version {
    pub const fn new(major: u64, minor: u64, patch: u64) -> Self {
        Version { major, minor, patch }
    }

    /// The branch the version is developed on.
    pub fn branch(&self) -> String {
        format!("v{self}")
    }

    /// The tag on `main` of the commit released as the version.
    pub fn tag(&self) -> String {
        format!("release{self}")
    }

    /// The version developed after this one is released, `X.(Y+1).0`.
    pub fn next_minor(&self) -> Version {
        Version::new(self.major, self.minor + 1, 0)
    }

    /// The release this version fixes, `X.Y.(Z-1)`, if it is a fix.
    pub fn fixes(&self) -> Option<Version> {
        (self.patch > 0).then(|| Version::new(self.major, self.minor, self.patch - 1))
    }

    /// The version a development branch's name gives, e.g. 0.2.0 from `v0.2.0`.
    pub fn from_branch(name: &str) -> Option<Version> {
        name.strip_prefix('v')?.parse().ok()
    }

    /// The version a release tag's name gives, e.g. 0.1.0 from `release0.1.0`.
    pub fn from_tag(name: &str) -> Option<Version> {
        name.strip_prefix("release")?.parse().ok()
    }

    /// Whether a requirement as written in Cargo.toml, e.g. `"0.1.0"`, `"^0.1"`, `"~0.1.2"` or `"=0.1.0"`, accepts this
    /// version, by Cargo's rules, or None for a requirement this does not read, e.g. one with a range or a wildcard.
    pub fn satisfies(&self, requirement: &str) -> Option<bool> {
        let requirement = requirement.trim();
        let (operator, rest) = match requirement.chars().next()? {
            '^' | '=' | '~' => (requirement.chars().next(), requirement[1..].trim()),
            _ => (None, requirement),
        };
        let parts: Vec<u64> = rest.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        let lower = Version::new(parts[0], *parts.get(1).unwrap_or(&0), *parts.get(2).unwrap_or(&0));
        let upper = match operator {
            Some('=') => match parts.len() {
                1 => Version::new(lower.major + 1, 0, 0),
                2 => Version::new(lower.major, lower.minor + 1, 0),
                _ => return Some(*self == lower),
            },
            Some('~') if parts.len() == 1 => Version::new(lower.major + 1, 0, 0),
            Some('~') => Version::new(lower.major, lower.minor + 1, 0),
            _ if lower.major > 0 || parts.len() == 1 => Version::new(lower.major + 1, 0, 0),
            _ if lower.minor > 0 || parts.len() == 2 => Version::new(0, lower.minor + 1, 0),
            _ => Version::new(0, 0, lower.patch + 1),
        };
        Some(lower <= *self && *self < upper)
    }

    /// The lowest version a requirement accepts, e.g. 0.2.0 for `"^0.2"`, the one which must be published for it to be
    /// met from crates.io, or None for a requirement this does not read.
    pub fn lowest(requirement: &str) -> Option<Version> {
        let rest = requirement.trim().trim_start_matches(['^', '=', '~']).trim();
        let parts: Vec<u64> = rest.split('.').map(|part| part.parse().ok()).collect::<Option<_>>()?;
        if parts.is_empty() || parts.len() > 3 {
            return None;
        }
        Some(Version::new(
            parts[0],
            *parts.get(1).unwrap_or(&0),
            *parts.get(2).unwrap_or(&0),
        ))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)
    }
}

impl FromStr for Version {
    type Err = String;

    /// Reads `X.Y.Z`, three numbers and nothing else, as the method has no pre-release or build versions.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let parts: Vec<&str> = text.split('.').collect();
        let number = |part: &str| part.parse::<u64>().ok().filter(|_| !part.starts_with('+'));
        match parts.as_slice() {
            [major, minor, patch] => match (number(major), number(minor), number(patch)) {
                (Some(major), Some(minor), Some(patch)) => Ok(Version::new(major, minor, patch)),
                _ => Err(format!("{text} is not a version like 0.1.0")),
            },
            _ => Err(format!("{text} is not a version like 0.1.0")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(text: &str) -> Version {
        text.parse().unwrap()
    }

    #[test]
    fn names_its_branch_and_tag() {
        assert_eq!(v("0.1.0").branch(), "v0.1.0");
        assert_eq!(v("0.1.0").tag(), "release0.1.0");
        assert_eq!(Version::from_branch("v0.2.0"), Some(v("0.2.0")));
        assert_eq!(Version::from_tag("release0.4.4"), Some(v("0.4.4")));
        assert_eq!(Version::from_branch("main"), None);
        assert_eq!(Version::from_tag("test0.5.0"), None);
    }

    #[test]
    fn reads_only_three_numbers() {
        assert!("0.1".parse::<Version>().is_err());
        assert!("0.1.0-beta".parse::<Version>().is_err());
        assert!("0.1.0.1".parse::<Version>().is_err());
        assert!("a.b.c".parse::<Version>().is_err());
    }

    #[test]
    fn knows_the_next_version_and_what_a_fix_fixes() {
        assert_eq!(v("0.1.3").next_minor(), v("0.2.0"));
        assert_eq!(v("0.1.1").fixes(), Some(v("0.1.0")));
        assert_eq!(v("0.2.0").fixes(), None);
    }

    #[test]
    fn a_requirement_before_1_0_accepts_fixes_but_not_the_next_minor_version() {
        assert_eq!(v("0.1.1").satisfies("0.1.0"), Some(true));
        assert_eq!(v("0.1.0").satisfies("^0.1.0"), Some(true));
        assert_eq!(v("0.2.0").satisfies("0.1.0"), Some(false));
        assert_eq!(v("0.1.0").satisfies("0.1.1"), Some(false));
        assert_eq!(v("0.1.9").satisfies("0.1"), Some(true));
        assert_eq!(v("0.0.2").satisfies("0.0.1"), Some(false));
        assert_eq!(v("0.9.0").satisfies("0"), Some(true));
    }

    #[test]
    fn a_requirement_from_1_0_accepts_the_same_major_version() {
        assert_eq!(v("1.4.0").satisfies("1.2"), Some(true));
        assert_eq!(v("2.0.0").satisfies("1.2"), Some(false));
        assert_eq!(v("1.2.5").satisfies("~1.2.3"), Some(true));
        assert_eq!(v("1.3.0").satisfies("~1.2.3"), Some(false));
        assert_eq!(v("1.2.3").satisfies("=1.2.3"), Some(true));
        assert_eq!(v("1.2.4").satisfies("=1.2.3"), Some(false));
    }

    #[test]
    fn does_not_guess_at_ranges() {
        assert_eq!(v("0.1.0").satisfies(">=0.1, <0.3"), None);
        assert_eq!(v("0.1.0").satisfies("*"), None);
        assert_eq!(Version::lowest("^0.2"), Some(v("0.2.0")));
        assert_eq!(Version::lowest("*"), None);
    }
}
