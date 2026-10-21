// SPDX-License-Identifier: GPL-3.0-or-later
//! Versions and the install / update / repair / downgrade decision (LAUNCH_PROMPT §B3).

use std::cmp::Ordering;
use std::fmt;

/// A `major.minor.patch[-pre]` version (build metadata after `+` is ignored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Version {
    /// Major.
    pub major: u64,
    /// Minor.
    pub minor: u64,
    /// Patch.
    pub patch: u64,
    /// Pre-release identifiers (`rc.1` → `["rc", "1"]`); empty for a release.
    pub pre: Vec<String>,
}

impl Version {
    /// Parses `1.2.3`, `1.2`, `1.2.3-rc.1`, `1.2.3+build`.
    pub fn parse(s: &str) -> Option<Self> {
        let s = s.trim();
        let s = s.split_once('+').map_or(s, |(v, _)| v);
        let (core, pre) = match s.split_once('-') {
            Some((c, p)) if !p.is_empty() => (c, p.split('.').map(str::to_owned).collect()),
            Some(_) => return None,
            None => (s, Vec::new()),
        };
        let mut nums = core.split('.');
        let num = |p: Option<&str>| -> Option<u64> {
            let p = p?;
            if p.is_empty() || p.len() > 9 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return None;
            }
            p.parse().ok()
        };
        let major = num(nums.next())?;
        let minor = num(nums.next())?;
        let patch = match nums.next() {
            Some(p) => num(Some(p))?,
            None => 0,
        };
        if nums.next().is_some() {
            return None;
        }
        let pre: Vec<String> = pre;
        if pre
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|b| b.is_ascii_alphanumeric()))
        {
            return None;
        }
        Some(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        if !self.pre.is_empty() {
            write!(f, "-{}", self.pre.join("."))?;
        }
        Ok(())
    }
}

fn cmp_pre(a: &[String], b: &[String]) -> Ordering {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => return Ordering::Equal,
        (true, false) => return Ordering::Greater, // a release is newer than its pre-releases
        (false, true) => return Ordering::Less,
        _ => {}
    }
    for (x, y) in a.iter().zip(b) {
        let o = match (x.parse::<u64>(), y.parse::<u64>()) {
            (Ok(i), Ok(j)) => i.cmp(&j),
            (Ok(_), Err(_)) => Ordering::Less,
            (Err(_), Ok(_)) => Ordering::Greater,
            (Err(_), Err(_)) => x.cmp(y),
        };
        if o != Ordering::Equal {
            return o;
        }
    }
    a.len().cmp(&b.len())
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| cmp_pre(&self.pre, &other.pre))
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

/// What this installer will do, given what is installed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Plan {
    /// Nothing installed: the Install screen.
    Fresh,
    /// An older version is installed: the Update screen.
    Update {
        /// The installed version.
        from: String,
    },
    /// The same version: offer Repair and Uninstall.
    Same,
    /// A newer version is installed: ask before installing this older one.
    Downgrade {
        /// The installed version.
        installed: String,
    },
}

/// Decides the [`Plan`]. An installed version that can't be parsed is treated as older
/// (an update repairs it).
pub fn decide(installed: Option<&str>, this: &str) -> Plan {
    let Some(installed) = installed else {
        return Plan::Fresh;
    };
    let (Some(a), Some(b)) = (Version::parse(installed), Version::parse(this)) else {
        return Plan::Update {
            from: installed.to_owned(),
        };
    };
    match a.cmp(&b) {
        Ordering::Less => Plan::Update {
            from: installed.to_owned(),
        },
        Ordering::Equal => Plan::Same,
        Ordering::Greater => Plan::Downgrade {
            installed: installed.to_owned(),
        },
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn v(s: &str) -> Version {
        Version::parse(s).unwrap()
    }

    #[test]
    fn parses_and_orders() {
        assert_eq!(v("0.1.0").to_string(), "0.1.0");
        assert_eq!(v("1.2").to_string(), "1.2.0");
        assert_eq!(v("1.2.3+build.7").to_string(), "1.2.3");
        assert!(v("0.1.0") < v("0.2.0"));
        assert!(v("0.9.0") < v("0.10.0"));
        assert!(v("1.0.0-rc.1") < v("1.0.0"));
        assert!(v("1.0.0-alpha") < v("1.0.0-alpha.1"));
        assert!(v("1.0.0-rc.2") < v("1.0.0-rc.10"));
        assert!(v("1.0.0-1") < v("1.0.0-beta"));
        assert_eq!(v("2.0.0").cmp(&v("2.0.0")), Ordering::Equal);
        for bad in [
            "",
            "1",
            "a.b.c",
            "1.2.3.4",
            "1.-2.3",
            "1.2.3-",
            "1.2.3-a..b",
            "01234567890.1.1",
        ] {
            assert!(Version::parse(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn decides_the_plan() {
        assert_eq!(decide(None, "0.2.0"), Plan::Fresh);
        assert_eq!(
            decide(Some("0.1.0"), "0.2.0"),
            Plan::Update {
                from: "0.1.0".into()
            }
        );
        assert_eq!(decide(Some("0.2.0"), "0.2.0"), Plan::Same);
        assert_eq!(
            decide(Some("0.3.0"), "0.2.0"),
            Plan::Downgrade {
                installed: "0.3.0".into()
            }
        );
        assert_eq!(
            decide(Some("0.2.0-rc.1"), "0.2.0"),
            Plan::Update {
                from: "0.2.0-rc.1".into()
            }
        );
        // Garbage in the registry: offer an update (which rewrites everything).
        assert_eq!(
            decide(Some("banana"), "0.2.0"),
            Plan::Update {
                from: "banana".into()
            }
        );
    }
}
