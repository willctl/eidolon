//! A Firefox profile as a facet. Stock programs run in the guest (Firefox,
//! Xvfb, x11vnc), kept alive by the systemd units in `guest/`.

use crate::cell::{Cell, GuestUser};
use crate::error::{Error, Result};
use crate::facet::plan;
use std::path::Path;
use std::time::Duration;

pub const USER: &str = "eidolon";
pub const PROFILE_PATH: &str = "/home/eidolon/profile";
pub const DOWNLOADS_PATH: &str = "/home/eidolon/files/downloads";
pub const UPLOADS_PATH: &str = "/home/eidolon/files/uploads";
pub const RUN_PATH: &str = "/run/eidolon";
pub const VNC_PASSWORD_PATH: &str = "/run/eidolon/vnc.pass";
pub const AUTOMATION_PORT: u16 = 9222;
pub const VNC_PORT: u16 = 5900;

pub const BROWSER_UNIT: &str = "eidolon-browser.service";
pub const VIEW_UNIT: &str = "eidolon-view.service";
pub const XVFB_UNIT: &str = "eidolon-xvfb.service";
pub const VNC_UNIT: &str = "eidolon-vnc.service";

pub const UNITS: [(&str, &str); 4] = [
    (BROWSER_UNIT, include_str!("guest/eidolon-browser.service")),
    (XVFB_UNIT, include_str!("guest/eidolon-xvfb.service")),
    (VNC_UNIT, include_str!("guest/eidolon-vnc.service")),
    (VIEW_UNIT, include_str!("guest/eidolon-view.service")),
];

pub const USER_JS: &str = include_str!("guest/user.js");

/// What a profile rebuilds by itself, so it is not carried.
/// `sessionstore-backups/recovery.baklz4` holds the open tabs, so it stays.
pub const PROFILE_LEAVE_OUT: [&str; 10] =
    ["cache2/**", "startupCache/**", "thumbnails/**", "crashes/**", "minidumps/**", "datareporting/**", "saved-telemetry-pings/**", "*.lock", "lock", ".parentlock"];

const STOP: [&str; 6] = ["systemctl", "stop", BROWSER_UNIT, VIEW_UNIT, VNC_UNIT, XVFB_UNIT];
const QUIET_ASKS: u32 = 40;
const QUIET_GAP: Duration = Duration::from_millis(250);

/// The command whose stdin and stdout reach `port` on the guest's own
/// loopback. The port is never forwarded.
pub fn stream_argv(port: u16) -> Vec<String> {
    vec!["socat".to_owned(), "-".to_owned(), format!("TCP:127.0.0.1:{port}")]
}

/// Sets the cell up for a browser unless its units are there already. The
/// plan installs them last, so their presence means it finished.
pub async fn provision(cell: &Cell) -> Result<bool> {
    if cell.run(&["systemctl", "cat", BROWSER_UNIT]).await?.success() {
        return Ok(false);
    }
    plan::provision().run(cell).await?;
    Ok(true)
}

/// Stops every unit of the browser. A cell never set up has none, which is
/// as stopped as it gets, so the status is not looked at.
pub async fn stop(cell: &Cell) -> Result<()> {
    cell.run_root(&STOP).await?;
    Ok(())
}

/// Stops the browser and waits until its user has no process left: Firefox
/// writes its profile while it exits, so copying earlier would tear it.
pub async fn quiet(cell: &Cell) -> Result<()> {
    cell.must_root("stopping the browser", &STOP).await?;
    for ask in 1..=QUIET_ASKS {
        let out = cell.run(&["pgrep", "-u", USER]).await?;
        match out.status {
            Some(1) => return Ok(()),
            Some(0) if ask < QUIET_ASKS => tokio::time::sleep(QUIET_GAP).await,
            Some(0) => break,
            _ => crate::cell::succeeded("looking for the browser's processes", &out)?,
        }
    }
    Err(Error::Command { what: "stopping the browser's processes".to_owned(), status: Some(0), stderr: "processes of the browser's user are still running".to_owned() })
}

/// The profile is private to the browser's user and the copy runs as the
/// cell's user, so it is opened for reading and closed again, also when the
/// copy failed.
pub async fn take(cell: &Cell, path: &str, staging: &Path) -> Result<()> {
    cell.must_root("opening the profile for copying", &["chmod", "-R", "a+rX", path]).await?;
    let copied = cell.copy_out(path, staging).await;
    let closed = cell.must_root("closing the profile again", &["chmod", "-R", "u+rwX,go-rwx", path]).await;
    copied.and(closed)
}

/// The guest user the browser runs as, who owns its profile and uploads.
pub fn user() -> Result<GuestUser> {
    Ok(USER.parse()?)
}

/// Replaces the cell's profile whole: a database journal left beside the
/// wrong database would corrupt it. The files land in a sibling folder
/// first, written by the browser's user, so a kill midway leaves the old
/// profile or the new one, plus a stale `.incoming` folder the next place
/// removes. An empty staged profile (never sealed) places nothing and keeps
/// the provisioned `user.js`.
pub async fn place(cell: &Cell, path: &str, staging: &Path) -> Result<()> {
    if std::fs::read_dir(staging).map_err(Error::io("read the staged profile"))?.next().is_none() {
        return Ok(());
    }
    let path = path.trim_end_matches('/');
    if path.split('/').filter(|p| !p.is_empty()).count() < 2 {
        return Err(Error::BadValue { what: "profile folder", rule: "two folders deep at least, never a top-level folder of the cell" });
    }
    let incoming = format!("{path}.incoming");
    cell.must_root("removing a profile left half placed", &["rm", "-rf", "--", &incoming]).await?;
    cell.copy_in(staging, &incoming, Some(&user()?)).await?;
    cell.must_root("removing the old profile", &["rm", "-rf", "--", path]).await?;
    cell.must_root("moving the new profile into place", &["mv", "--", &incoming, path]).await?;
    cell.must_root("closing the profile", &["chmod", "-R", "u+rwX,go-rwx", path]).await
}
