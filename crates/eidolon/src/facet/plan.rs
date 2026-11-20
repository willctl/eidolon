use crate::cell::{succeeded, Cell, Run};
use crate::error::{Error, Result};
use crate::facet::firefox::{self, BROWSER_UNIT, PROFILE_PATH, USER, VIEW_UNIT};
use serde::Serialize;
use std::fmt;
use std::time::Duration;

/// The fingerprint of the key Mozilla signs its APT repository with, as
/// published at <https://support.mozilla.org/kb/install-firefox-linux>.
pub const MOZILLA_KEY_FINGERPRINT: &str = "35BAA0B33E9EB396F59CA838C0BA5CE6DC6315A3";

const KEY_PATH: &str = "/etc/apt/keyrings/packages.mozilla.org.asc";
const STEP_DEADLINE: Duration = Duration::from_secs(20 * 60);

/// One command to run in a cell, as data, so a golden file can pin every
/// command a plan runs.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct Step {
    pub why: String,
    pub argv: Vec<String>,
    pub root: bool,
    /// A file's contents, or a secret: never an argument.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stdin: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub skip_if: Option<Vec<String>>,
    /// The step prints `gpg --with-colons` listings, whose only `fpr` must be this.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub expect_fingerprint: Option<String>,
}

impl fmt::Debug for Step {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Step")
            .field("why", &self.why)
            .field("argv", &self.argv)
            .field("root", &self.root)
            .field("stdin_bytes", &self.stdin.as_ref().map(String::len))
            .field("skip_if", &self.skip_if)
            .finish_non_exhaustive()
    }
}

fn strings(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|p| (*p).to_owned()).collect()
}

impl Step {
    fn root(why: &str, argv: &[&str]) -> Step {
        Step { why: why.to_owned(), argv: strings(argv), root: true, stdin: None, skip_if: None, expect_fingerprint: None }
    }

    fn write(why: &str, path: &str, text: &str) -> Step {
        Step { stdin: Some(text.to_owned()), ..Step::root(why, &["tee", path]) }
    }

    fn skip_if(mut self, guard: &[&str]) -> Step {
        self.skip_if = Some(strings(guard));
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Plan {
    pub steps: Vec<Step>,
}

impl Plan {
    pub async fn run(&self, cell: &Cell) -> Result<()> {
        fn argv(a: &[String]) -> Vec<&str> {
            a.iter().map(String::as_str).collect()
        }
        for step in &self.steps {
            if let Some(guard) = &step.skip_if {
                if cell.run(&argv(guard)).await?.success() {
                    continue;
                }
            }
            let how = Run { root: step.root, stdin: step.stdin.as_ref().map(|s| s.as_bytes().to_vec()), deadline: Some(STEP_DEADLINE) };
            let out = cell.run_with(&argv(&step.argv), how).await?;
            succeeded(&step.why, &out)?;
            if let Some(expected) = &step.expect_fingerprint {
                let found = fingerprints(&out.stdout_lossy());
                if found.len() != 1 || !found[0].eq_ignore_ascii_case(expected) {
                    return Err(Error::Fingerprint { expected: expected.clone(), found });
                }
            }
        }
        Ok(())
    }
}

fn fingerprints(listing: &str) -> Vec<String> {
    listing.lines().filter(|l| l.starts_with("fpr:")).map(|l| l.split(':').nth(9).unwrap_or_default().to_owned()).collect()
}

pub fn provision() -> Plan {
    let apt = |why: &str, rest: &[&str]| {
        let mut argv = vec!["env", "DEBIAN_FRONTEND=noninteractive", "apt-get"];
        argv.extend_from_slice(rest);
        Step::root(why, &argv)
    };
    let mut steps = vec![
        apt("refresh the package lists", &["update"]),
        apt(
            "install the tools the browser and its window need",
            &["install", "-y", "--no-install-recommends", "ca-certificates", "curl", "gnupg", "socat", "xvfb", "x11vnc", "fonts-dejavu-core"],
        ),
        Step::root("make the keyring folder", &["install", "-d", "-m", "0755", "/etc/apt/keyrings"]),
        Step::root("fetch Mozilla's signing key", &["curl", "-fsSL", "--proto", "=https", "-o", KEY_PATH, "https://packages.mozilla.org/apt/repo-signing-key.gpg"]),
        Step {
            why: "check the signing key's fingerprint".to_owned(),
            argv: strings(&["gpg", "--show-keys", "--with-colons", "--fingerprint", KEY_PATH]),
            root: false,
            stdin: None,
            skip_if: None,
            expect_fingerprint: Some(MOZILLA_KEY_FINGERPRINT.to_owned()),
        },
        Step::write(
            "add Mozilla's repository",
            "/etc/apt/sources.list.d/mozilla.sources",
            &format!("Types: deb\nURIs: https://packages.mozilla.org/apt\nSuites: mozilla\nComponents: main\nSigned-By: {KEY_PATH}\n"),
        ),
        Step::write("prefer Mozilla's Firefox package", "/etc/apt/preferences.d/mozilla", "Package: *\nPin: origin packages.mozilla.org\nPin-Priority: 1000\n"),
        apt("refresh the package lists with Mozilla's repository", &["update"]),
        apt("install Firefox", &["install", "-y", "--no-install-recommends", "firefox"]),
        Step::root("make the browser's user", &["useradd", "--create-home", "--shell", "/usr/sbin/nologin", USER]).skip_if(&["id", "-u", USER]),
        // The host's copy-out reads as a user other than the browser's, who must walk into the home folder.
        Step::root("let the home folder be entered", &["chmod", "0755", "/home/eidolon"]),
        Step::root("make the profile folder", &["install", "-d", "-o", USER, "-g", USER, "-m", "0700", PROFILE_PATH]),
        Step::root("make the files folders", &["install", "-d", "-o", USER, "-g", USER, "-m", "0755", "/home/eidolon/files", firefox::DOWNLOADS_PATH, firefox::UPLOADS_PATH]),
        Step {
            stdin: Some(firefox::USER_JS.to_owned()),
            ..Step::root("write the profile's preferences", &["runuser", "-u", USER, "--", "tee", &format!("{PROFILE_PATH}/user.js")])
        },
    ];
    for (name, text) in firefox::UNITS {
        steps.push(Step::write("install a unit", &format!("/etc/systemd/system/{name}"), text));
    }
    steps.push(Step::root("load the units", &["systemctl", "daemon-reload"]));
    steps.push(Step::root("start the browser at boot", &["systemctl", "enable", BROWSER_UNIT]));
    Plan { steps }
}

pub fn start_browser() -> Plan {
    Plan { steps: vec![Step::root("start the browser", &["systemctl", "start", BROWSER_UNIT])] }
}

/// Starts the sign-in window, which stops the automation browser. The
/// screen share's `secret` reaches the guest as stdin of one command.
pub fn start_view(secret: &str) -> Plan {
    // `dd` and not `tee`: tee would print the secret back on stdout.
    let write = Step {
        stdin: Some(secret.to_owned()),
        ..Step::root("give the window its password", &["runuser", "-u", USER, "--", "dd", &format!("of={}", firefox::VNC_PASSWORD_PATH), "status=none"])
    };
    Plan {
        steps: vec![
            Step::root("make the window's folder", &["install", "-d", "-o", USER, "-g", USER, "-m", "0700", firefox::RUN_PATH]),
            write,
            Step::root("keep the password private", &["chmod", "0600", firefox::VNC_PASSWORD_PATH]),
            Step::root("start the sign-in window", &["systemctl", "start", VIEW_UNIT]),
        ],
    }
}
