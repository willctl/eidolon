//! A target: a folder to capture, with include and exclude globs and, for an
//! app that must be closed first, the lock file the app holds while it runs.
//! Presets are the same TOML as custom targets, compiled in.

use crate::error::{Code, Error, Result};
use crate::rules::pattern_is_inside;
use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

const PRESETS: &[(&str, &str)] = &[("firefox", include_str!("presets/firefox.toml"))];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Target {
    #[serde(default)]
    pub about: String,
    #[serde(default = "everything")]
    pub include: Vec<String>,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub closed: Option<Closed>,
}

/// The app must be closed: it holds `unix_lock` with fcntl, or keeps
/// `windows_lock` open without sharing, while it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Closed {
    pub app: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unix_lock: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub windows_lock: Option<String>,
}

impl Closed {
    pub fn lock_here(&self) -> Option<&str> {
        if cfg!(windows) {
            self.windows_lock.as_deref()
        } else {
            self.unix_lock.as_deref()
        }
    }
}

fn everything() -> Vec<String> {
    vec!["**".to_owned()]
}

impl Target {
    pub fn presets() -> impl Iterator<Item = &'static str> {
        PRESETS.iter().map(|(name, _)| *name)
    }

    pub fn preset(name: &str) -> Option<Target> {
        PRESETS.iter().find(|(n, _)| *n == name).and_then(|(_, text)| Target::parse(text).ok())
    }

    pub fn parse(text: &str) -> Result<Target> {
        let target: Target = toml::from_str(text)
            .map_err(|e| Error::refused(Code::BadTarget, format!("The target does not read: {}", e.message()), "Fix the TOML; `eidolon target show firefox` prints an example."))?;
        target.rules()?;
        Ok(target)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string(self).unwrap_or_default()
    }

    pub fn rules(&self) -> Result<Rules> {
        let locks: Vec<String> = self.closed.iter().flat_map(|c| [c.unix_lock.clone(), c.windows_lock.clone()]).flatten().collect();
        for lock in &locks {
            if lock.is_empty() || lock.contains(['/', '\\', ':', '\0']) || lock == "." || lock == ".." {
                return Err(Error::refused(
                    Code::BadTarget,
                    format!("The lock {lock:?} is not a file name in the target's folder."),
                    "Name a file directly in the folder, such as `.parentlock`.",
                ));
            }
        }
        Ok(Rules { include: globs(&self.include)?, exclude: globs(&self.exclude)?, locks })
    }
}

fn globs(patterns: &[String]) -> Result<GlobSet> {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        pattern_is_inside(pattern)?;
        let glob = GlobBuilder::new(pattern)
            .literal_separator(true)
            .backslash_escape(true)
            .build()
            .map_err(|e| Error::refused(Code::BadTarget, format!("The pattern {pattern:?} does not read: {e}."), "Write a glob such as `cache2` or `storage/**/*.sqlite`."))?;
        set.add(glob);
    }
    set.build().map_err(|e| Error::refused(Code::BadTarget, format!("The patterns do not build: {e}."), "Simplify them."))
}

/// A target's rules, ready to match paths relative to its folder, written with `/`.
#[derive(Debug, Clone)]
pub struct Rules {
    include: GlobSet,
    exclude: GlobSet,
    locks: Vec<String>,
}

impl Rules {
    /// Left out, with everything below it: an exclude glob matches the path
    /// or a folder above it, or it is the app's lock file.
    pub fn excludes(&self, rel: &str) -> bool {
        self.locks.iter().any(|l| l == rel) || prefixes(rel).any(|p| self.exclude.is_match(p))
    }

    /// Captured: an include glob matches the path or a folder above it, and
    /// nothing excludes it.
    pub fn keeps(&self, rel: &str) -> bool {
        !self.excludes(rel) && prefixes(rel).any(|p| self.include.is_match(p))
    }
}

/// `a`, `a/b`, `a/b/c` for `a/b/c`.
fn prefixes(rel: &str) -> impl Iterator<Item = &str> {
    rel.match_indices('/').map(|(i, _)| &rel[..i]).chain(std::iter::once(rel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_firefox_preset_keeps_sign_ins_and_leaves_out_caches_and_its_lock() {
        let rules = Target::preset("firefox").unwrap().rules().unwrap();
        for kept in ["cookies.sqlite", "cookies.sqlite-wal", "key4.db", "logins.json", "prefs.js", "storage/default/https+++example.com/ls/data.sqlite", "extensions/a@b.xpi"] {
            assert!(rules.keeps(kept), "{kept}");
        }
        for left in [
            ".parentlock",
            "parent.lock",
            "lock",
            "cache2/entries/x",
            "startupCache/startupCache.8.little",
            "storage/temporary/x",
            "gmp-widevinecdm/1/x",
            "crashes/events/x",
            "a.tmp",
            "safebrowsing",
        ] {
            assert!(!rules.keeps(left), "{left}");
        }
        assert!(rules.excludes("cache2"), "a left-out folder is pruned whole");
    }

    #[test]
    fn a_custom_target_reads_from_toml_and_globs_stay_inside_their_folder() {
        let target = Target::parse("include = [\"notes/**\", \"todo.txt\"]\nexclude = [\"notes/*.bak\"]\n").unwrap();
        let rules = target.rules().unwrap();
        assert!(rules.keeps("notes/a.md") && rules.keeps("notes/deep/b.md") && rules.keeps("todo.txt"));
        assert!(!rules.keeps("notes/a.bak") && !rules.keeps("other.txt"));
        assert!(rules.keeps("notes/deep/a.bak"), "`*` does not cross a folder");
        assert_eq!(Target::parse(&target.to_toml()).unwrap(), target);
        assert_eq!(Target::parse("include = [\"../x\"]").err().and_then(|e| e.code()), Some(Code::BadTarget));
        assert_eq!(Target::parse("surprise = 1").err().and_then(|e| e.code()), Some(Code::BadTarget));
        assert_eq!(Target::parse("[closed]\napp = \"x\"\nunix_lock = \"../lock\"").err().and_then(|e| e.code()), Some(Code::BadTarget));
    }

    #[test]
    fn a_lock_file_is_never_captured_even_if_the_rules_forget_it() {
        let rules = Target::parse("[closed]\napp = \"App\"\nunix_lock = \"app.lock\"").unwrap().rules().unwrap();
        assert!(!rules.keeps("app.lock") && rules.keeps("data"));
    }
}
