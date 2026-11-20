//! The holder list: the machines that may hold a target's seat, one per line
//! in the format of OpenSSH's allowed_signers, so `ssh-keygen -Y verify -f`
//! reads the same file.

use crate::error::{Code, Error, Result};
use crate::key::{HolderKey, ACK_NAMESPACE, SEAT_NAMESPACE};
use serde::Serialize;
use std::fmt::Write as _;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Holder {
    pub name: String,
    pub key: HolderKey,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Holders(Vec<Holder>);

/// Names of holders and targets: letters, digits, `.`, `_` and `-`, at most
/// 64, not starting with `-` or `.`.
pub fn check_name(what: &str, name: &str) -> Result<()> {
    let ok = !name.is_empty()
        && name.len() <= 64
        && !name.starts_with(['-', '.'])
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if ok {
        Ok(())
    } else {
        Err(Error::refused(Code::BadName, format!("{name:?} is not a {what} name."), "Use up to 64 letters, digits, `.`, `_` or `-`, starting with a letter or digit."))
    }
}

impl Holders {
    pub fn parse(text: &str) -> Result<Holders> {
        let mut holders = Holders::default();
        for (n, line) in text.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let bad = |why: &str| Error::refused(Code::BadHolder, format!("Line {} of the holder list {why}.", n + 1), "Write each line as `name ssh-ed25519 AAAA...`.");
            let (name, rest) = line.split_once(char::is_whitespace).ok_or_else(|| bad("has no key"))?;
            let rest = rest.trim_start();
            let key_text = if rest.starts_with("ssh-") { rest } else { skip_options(rest).ok_or_else(|| bad("has options that do not end"))? };
            check_name("holder", name)?;
            holders.add(Holder { name: name.to_owned(), key: key_text.parse()? })?;
        }
        Ok(holders)
    }

    /// One `name namespaces="..." ssh-ed25519 AAAA...` line per holder.
    pub fn render(&self) -> String {
        let mut text = String::new();
        for h in &self.0 {
            let _ = writeln!(text, "{} namespaces=\"{SEAT_NAMESPACE},{ACK_NAMESPACE}\" {}", h.name, h.key);
        }
        text
    }

    /// Adds a holder. Adding the same name with the same key again changes
    /// nothing; a name or key already listed otherwise is refused.
    pub fn add(&mut self, holder: Holder) -> Result<bool> {
        if self.0.contains(&holder) {
            return Ok(false);
        }
        if let Some(other) = self.0.iter().find(|h| h.name == holder.name || h.key == holder.key) {
            return Err(Error::refused(Code::BadHolder, format!("The holder list already has {} with that name or key.", other.name), "Each machine is listed once, under one name."));
        }
        self.0.push(holder);
        Ok(true)
    }

    pub fn iter(&self) -> impl Iterator<Item = &Holder> {
        self.0.iter()
    }

    pub fn by_key(&self, key: &HolderKey) -> Option<&Holder> {
        self.0.iter().find(|h| h.key == *key)
    }

    pub fn by_name(&self, name: &str) -> Option<&Holder> {
        self.0.iter().find(|h| h.name == name)
    }

    pub fn name_of(&self, key: &HolderKey) -> String {
        self.by_key(key).map_or_else(|| "an unlisted machine".to_owned(), |h| h.name.clone())
    }

    /// The same machines under the same names, in any order.
    pub fn same_as(&self, other: &Holders) -> bool {
        self.0.len() == other.0.len() && self.0.iter().all(|h| other.0.contains(h))
    }
}

/// The text after an allowed_signers options field, which may quote spaces.
fn skip_options(rest: &str) -> Option<&str> {
    let mut quoted = false;
    for (i, c) in rest.char_indices() {
        match c {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => return Some(rest[i..].trim_start()),
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::key::MachineKey;

    #[test]
    fn the_list_reads_back_what_it_writes_and_what_ssh_keygen_reads() {
        let (desk, tower) = (MachineKey::generate().unwrap(), MachineKey::generate().unwrap());
        let mut list = Holders::default();
        list.add(Holder { name: "desk".into(), key: desk.public().clone() }).unwrap();
        list.add(Holder { name: "tower".into(), key: tower.public().clone() }).unwrap();
        let text = list.render();
        assert_eq!(Holders::parse(&text).unwrap(), list);
        let plain = format!("# comment\n\ndesk {}\ntower namespaces=\"a b\" {} tower@x\n", desk.public(), tower.public());
        assert!(Holders::parse(&plain).unwrap().same_as(&list));
    }

    #[test]
    fn a_name_or_key_is_listed_once() {
        let (desk, tower) = (MachineKey::generate().unwrap(), MachineKey::generate().unwrap());
        let mut list = Holders::default();
        assert!(list.add(Holder { name: "desk".into(), key: desk.public().clone() }).unwrap());
        assert!(!list.add(Holder { name: "desk".into(), key: desk.public().clone() }).unwrap());
        assert!(list.add(Holder { name: "desk".into(), key: tower.public().clone() }).is_err());
        assert!(list.add(Holder { name: "tower".into(), key: desk.public().clone() }).is_err());
    }

    #[test]
    fn names_are_plain() {
        for bad in ["", "-x", ".x", "a b", "a/b", "a@b", &"x".repeat(65)] {
            assert_eq!(check_name("holder", bad).err().and_then(|e| e.code()), Some(Code::BadName), "{bad:?}");
        }
        for good in ["desk", "tower-2", "a.b_c"] {
            assert!(check_name("holder", good).is_ok());
        }
    }
}
