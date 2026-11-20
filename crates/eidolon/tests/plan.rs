//! The guest setup is data: its argument lists and units are pinned in
//! golden files, and it never builds a shell string.

#![expect(clippy::unwrap_used, reason = "a failing test should say where; helpers here are not #[test] fns")]

use adytum::testing::{failed, ok, DriverCall, FakeDriver};
use eidolon::cell::{Cell, Output};
use eidolon::facet::firefox::{self, stream_argv, UNITS, USER_JS};
use eidolon::facet::plan::{self, Plan, MOZILLA_KEY_FINGERPRINT};
use eidolon::testing::running;
use eidolon::Error;
use std::path::PathBuf;
use std::sync::Arc;

struct Run {
    argv: Vec<String>,
    root: bool,
    stdin: Vec<u8>,
}

fn answering(answer: impl Fn(&[String]) -> Output + Send + 'static) -> (Arc<FakeDriver>, Cell) {
    let fake = Arc::new(FakeDriver::new());
    fake.on_exec(move |exec| answer(&exec.argv));
    let cell = running(&fake, &"adytum-test-plan".parse().unwrap());
    (fake, cell)
}

fn runs(fake: &FakeDriver) -> Vec<Run> {
    fake.calls()
        .into_iter()
        .filter_map(|c| match c {
            DriverCall::Exec(e) => Some(Run { argv: e.argv, root: e.root, stdin: e.stdin }),
            _ => None,
        })
        .collect()
}

fn golden_path(name: &str) -> PathBuf {
    PathBuf::from(format!("{}/tests/golden/{name}", env!("CARGO_MANIFEST_DIR")))
}

/// Compares with the golden file; `UPDATE_GOLDEN=1` rewrites it.
fn check_golden(name: &str, actual: &str) {
    let path = golden_path(name);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
    }
    let want = std::fs::read_to_string(&path).unwrap_or_default();
    assert_eq!(actual, want, "{name} differs from the golden file; run with UPDATE_GOLDEN=1 if the change is meant");
}

#[test]
fn the_provisioning_plan_matches_its_golden_file() {
    let plan = plan::provision();
    let mut json = serde_json::to_string_pretty(&plan).unwrap();
    json.push('\n');
    check_golden("provision.json", &json);
}

#[test]
fn the_units_match_their_golden_files() {
    for (name, text) in UNITS {
        check_golden(&format!("units/{name}"), text);
    }
    check_golden("units/user.js", USER_JS);
}

#[test]
fn no_argument_is_a_shell_string() {
    let mut plans = vec![plan::provision(), plan::start_browser(), plan::start_view("a-secret")];
    plans.push(Plan { steps: vec![] });
    for plan in plans {
        for step in &plan.steps {
            for arg in step.argv.iter().chain(step.skip_if.iter().flatten()) {
                for bad in ["&&", ";", "|", "$(", "`", ">", "<"] {
                    assert!(!arg.contains(bad), "{:?} has {bad:?}: {arg:?}", step.why);
                }
            }
        }
    }
    assert!(stream_argv(9222).iter().all(|a| !a.contains(';') && !a.contains('|')));
}

#[test]
fn every_command_asks_the_driver_for_root_or_is_a_read_and_none_says_sudo() {
    for plan in [plan::provision(), plan::start_browser(), plan::start_view("s")] {
        for step in plan.steps {
            assert!(!step.argv.contains(&"sudo".to_owned()), "{:?}", step.argv);
            let program = step.argv.first().map(String::as_str);
            assert!(step.root || program == Some("gpg"), "{:?} would run unprivileged", step.argv);
        }
    }
}

#[test]
fn firefox_comes_from_mozillas_repository_not_the_snap() {
    let plan = plan::provision();
    let text = serde_json::to_string(&plan).unwrap();
    assert!(text.contains("https://packages.mozilla.org/apt"));
    assert!(text.contains(MOZILLA_KEY_FINGERPRINT));
    assert!(!text.contains("snap"), "nothing installs a snap");
}

#[test]
fn the_view_password_is_stdin_and_never_an_argument() {
    let plan = plan::start_view("hunter-two");
    for step in &plan.steps {
        assert!(!step.argv.iter().any(|a| a.contains("hunter-two")), "{:?}", step.argv);
    }
    assert_eq!(plan.steps.iter().filter(|s| s.stdin.as_deref() == Some("hunter-two")).count(), 1);
    // And a step's debug text, which may reach a log, does not show it.
    assert!(!format!("{plan:?}").contains("hunter-two"));
}

#[test]
fn nothing_hides_automation() {
    let all = format!("{USER_JS}{}", UNITS.iter().map(|(_, t)| *t).collect::<String>()).to_lowercase();
    for word in ["webdriver", "useragent", "user-agent", "stealth", "resistfingerprinting", "marionette"] {
        assert!(!all.contains(word), "{word}");
    }
}

#[test]
fn the_browser_unit_has_automation_and_the_view_unit_has_none() {
    let text = |n: &str| UNITS.iter().find(|(name, _)| *name == n).unwrap().1;
    let browser = text(firefox::BROWSER_UNIT);
    assert!(browser.contains("--remote-debugging-port 9222"));
    assert!(browser.contains("Restart=always"));
    assert!(browser.contains("Conflicts=eidolon-view.service"));
    let view = text(firefox::VIEW_UNIT);
    assert!(!view.contains("remote-debugging") && !view.contains("--headless"));
    assert!(view.contains("Conflicts=eidolon-browser.service"));
    let vnc = text(firefox::VNC_UNIT);
    assert!(vnc.contains("-localhost") && vnc.contains("-rfbport 5900") && vnc.contains("-passwdfile /run/eidolon/vnc.pass"));
}

#[test]
fn the_stream_reaches_the_port_through_socat_on_the_guests_loopback() {
    assert_eq!(stream_argv(9222), ["socat", "-", "TCP:127.0.0.1:9222"]);
}

#[tokio::test]
async fn running_the_plan_sends_each_file_as_stdin() {
    let (driver, cell) = answering(|argv| {
        if argv.first().map(String::as_str) == Some("gpg") {
            ok(&format!("pub:-:2048:1:C0BA5CE6DC6315A3:1620162483:::-:::scSC::::::23::0:\nfpr:::::::::{MOZILLA_KEY_FINGERPRINT}:\n"))
        } else if argv.first().map(String::as_str) == Some("id") {
            failed(1, "")
        } else {
            ok("")
        }
    });
    plan::provision().run(&cell).await.unwrap();
    let execs = runs(&driver);
    let tee = |path: &str| execs.iter().find(|e| e.root && e.argv == ["tee", path]).map(|e| String::from_utf8(e.stdin.clone()).unwrap_or_default()).unwrap_or_default();
    assert_eq!(tee("/etc/systemd/system/eidolon-browser.service"), UNITS[0].1);
    assert!(tee("/etc/apt/sources.list.d/mozilla.sources").contains("Signed-By: /etc/apt/keyrings/packages.mozilla.org.asc"));
    // The user was missing, so the guard failed and the user was made.
    assert!(execs.iter().any(|e| e.argv.contains(&"useradd".to_owned())));
    // Changes to the system ask for root; the guard and the key check do not.
    assert!(execs.iter().filter(|e| e.argv.contains(&"useradd".to_owned())).all(|e| e.root));
    assert!(execs.iter().filter(|e| matches!(e.argv.first().map(String::as_str), Some("id" | "gpg"))).all(|e| !e.root));
}

#[tokio::test]
async fn a_step_whose_guard_succeeds_is_skipped() {
    let (driver, cell) = answering(|argv| if argv.first().map(String::as_str) == Some("gpg") { ok(&format!("fpr:::::::::{MOZILLA_KEY_FINGERPRINT}:\n")) } else { ok("") });
    plan::provision().run(&cell).await.unwrap();
    assert!(!runs(&driver).iter().any(|e| e.argv.contains(&"useradd".to_owned())), "id -u succeeded, so the user exists");
}

#[tokio::test]
async fn a_wrong_signing_key_stops_the_plan_before_the_repository_is_added() {
    let (driver, cell) = answering(|argv| if argv.first().map(String::as_str) == Some("gpg") { ok("fpr:::::::::0000000000000000000000000000000000000000:\n") } else { ok("") });
    let err = plan::provision().run(&cell).await.unwrap_err();
    assert!(matches!(err, Error::Fingerprint { .. }), "{err}");
    assert!(!runs(&driver).iter().any(|e| e.argv.iter().any(|a| a.contains("mozilla.sources"))));
}

#[tokio::test]
async fn a_failed_step_says_which_and_never_quotes_its_stdin() {
    let (_, cell) = answering(|argv| if argv.contains(&"/run/eidolon/vnc.pass".to_owned()) || argv.iter().any(|a| a.starts_with("of=")) { failed(1, "disk full") } else { ok("") });
    let err = plan::start_view("hunter-two").run(&cell).await.unwrap_err();
    let text = err.to_string();
    assert!(text.contains("give the window its password") && text.contains("disk full"), "{text}");
    assert!(!text.contains("hunter-two"));
}
