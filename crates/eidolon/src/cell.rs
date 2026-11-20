//! The cells an eidolon opens in. Everything Eidolon takes from Adytum
//! passes through this file.

use crate::error::{Error, Result};
use crate::model::{RefusalCode, Wall};
use adytum::exec::Runner as _;
use adytum::{CellState, Driver, ExecOpts};
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

pub use adytum::exec::{Duplex, Output};
pub use adytum::{CellName, GuestUser};

/// Adytum's cells, which record every start in Adytum's ledger.
pub struct Cells(adytum::Cells);

impl Cells {
    /// Adytum's cells on this machine: the ledger and Lima under
    /// `$ADYTUM_HOME`, else `~/.adytum`. With the `testing` feature,
    /// `EIDOLON_TEST_CELLS` names a folder whose subfolders stand in for the
    /// cells' file systems.
    pub fn system() -> Result<Cells> {
        let home = adytum::home().ok_or(Error::BadValue { what: "Adytum home", rule: "set: give ADYTUM_HOME a folder" })?;
        #[cfg(feature = "testing")]
        if let Some(root) = std::env::var_os("EIDOLON_TEST_CELLS") {
            let driver = Arc::new(crate::testing::folder_cells(root, |_| None));
            let cells = adytum::Cells::open(&home, driver.clone())?;
            // The test recorded them from another process.
            for cell in cells.list()? {
                driver.add_cell(&cell.spec.name, CellState::Running);
            }
            return Ok(Cells(cells));
        }
        let lima = adytum::lima::Lima::system(adytum::lima::lima_home_in(&home))?;
        Ok(Cells(adytum::Cells::open(&home, Arc::new(lima))?))
    }

    pub fn wall(&self, name: &CellName) -> Result<Wall> {
        Ok(self.recorded(name)?.spec.wall)
    }

    pub async fn ready(&self, name: &CellName) -> Result<Cell> {
        if self.recorded(name)?.state == CellState::Broken {
            return Err(Error::refused(RefusalCode::CellBroken, format!("{name} is broken."), "Run `adytum doctor` or remove the cell."));
        }
        let listed = self.0.driver().list().await?.into_iter().find(|l| l.name == *name);
        match listed.map(|l| l.state) {
            Some(CellState::Running) => {}
            Some(CellState::Stopped) => self.0.start(name).await?,
            _ => return Err(Error::refused(RefusalCode::CellMissing, format!("The driver has no usable cell named {name}."), "Run `adytum ls` to see what exists.")),
        }
        Ok(Cell { driver: Arc::clone(self.0.driver()), name: name.clone() })
    }

    /// Records that the cell holds the eidolon open, so Adytum will not clone it.
    pub fn hold(&self, name: &CellName, eidolon: &str) -> Result<()> {
        Ok(self.0.hold(name, eidolon)?)
    }

    pub fn release(&self, name: &CellName, eidolon: &str) -> Result<()> {
        Ok(self.0.release(name, eidolon)?)
    }

    fn recorded(&self, name: &CellName) -> Result<adytum::Cell> {
        self.0.get(name).map_err(|e| match e {
            adytum::Error::NotRecorded(_) => Error::refused(RefusalCode::UnknownCell, format!("Adytum has no record of a cell named {name}."), "Run `adytum ls` to see the cells."),
            e => e.into(),
        })
    }
}

#[derive(Clone)]
pub struct Cell {
    driver: Arc<dyn Driver>,
    name: CellName,
}

#[derive(Debug, Clone, Default)]
pub struct Run {
    pub root: bool,
    pub stdin: Option<Vec<u8>>,
    pub deadline: Option<Duration>,
}

impl Cell {
    pub fn new(driver: Arc<dyn Driver>, name: CellName) -> Cell {
        Cell { driver, name }
    }

    pub fn name(&self) -> &CellName {
        &self.name
    }

    pub async fn run(&self, argv: &[&str]) -> Result<Output> {
        self.run_with(argv, Run::default()).await
    }

    pub async fn run_root(&self, argv: &[&str]) -> Result<Output> {
        self.run_with(argv, Run { root: true, ..Run::default() }).await
    }

    pub async fn run_with(&self, argv: &[&str], how: Run) -> Result<Output> {
        let argv: Vec<String> = argv.iter().map(|a| (*a).to_owned()).collect();
        Ok(self.driver.exec(&self.name, &argv, ExecOpts { root: how.root, stdin: how.stdin, deadline: how.deadline, ..ExecOpts::default() }).await?)
    }

    pub async fn must_root(&self, what: &str, argv: &[&str]) -> Result<()> {
        let out = self.run_root(argv).await?;
        succeeded(what, &out)
    }

    pub async fn stream(&self, argv: &[String]) -> Result<Duplex> {
        Ok(self.driver.stream(&self.name, argv).await?)
    }

    pub async fn copy_out(&self, guest: &str, host: &Path) -> Result<()> {
        Ok(self.driver.copy_out(&self.name, guest, host).await?)
    }

    /// `owner` writes the copy and owns it; `None` is the cell's user.
    pub async fn copy_in(&self, host: &Path, guest: &str, owner: Option<&GuestUser>) -> Result<()> {
        Ok(self.driver.copy_in(&self.name, host, guest, owner).await?)
    }
}

pub async fn command_stream(argv: &[String], pass_env: &[&str], deadline: Duration) -> Result<Duplex> {
    let (program, rest) = argv.split_first().ok_or(Error::BadValue { what: "command", rule: "a program and its arguments" })?;
    let command = adytum::exec::Command::new(program).args(rest).pass_env(pass_env).deadline(deadline);
    adytum::exec::SystemRunner::new().duplex(&command).await.map_err(|e| Error::Command { what: format!("starting {program}"), status: None, stderr: e.to_string() })
}

#[cfg(feature = "browser")]
/// Whether the error is a command that ran in the cell and failed, as
/// `socat` does while nothing listens on its port yet.
pub(crate) fn ran_and_failed(e: &Error) -> bool {
    matches!(e, Error::Cell(adytum::Error::Tool { .. }))
}

pub(crate) fn succeeded(what: &str, out: &Output) -> Result<()> {
    if out.success() {
        return Ok(());
    }
    let stderr = out.stderr_lossy();
    let tail: String = stderr.trim().chars().rev().take(1500).collect::<Vec<_>>().into_iter().rev().collect();
    Err(Error::Command { what: what.to_owned(), status: out.status, stderr: tail })
}
