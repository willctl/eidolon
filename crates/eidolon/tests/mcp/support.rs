//! Folder cells whose browser is a fake page, and a ready set of tools.

use adytum::testing::{duplex, DriverCall, FakeDriver};
use adytum::GuestUser;
use eidolon::browser::{Browser, ConnectOptions};
use eidolon::cell::CellName;
use eidolon::mcp::{Part, Tool, Tools};
use eidolon::model::RefusalCode;
use eidolon::testing::{folder_cells, running, FakeBidi, Page};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::Arc;
use std::time::Duration;

pub const DOWNLOADS: &str = "/home/eidolon/files/downloads";

pub fn cell() -> CellName {
    "adytum-test-mcp".parse().unwrap()
}

/// A temp folder that goes away with the test.
pub struct Temp(pub PathBuf);

impl Temp {
    pub fn new() -> Temp {
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!("eidolon-mcp-test-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
        std::fs::create_dir_all(&dir).unwrap();
        // Resolved, so a test can compare paths even where the temp folder is a link.
        Temp(dir.canonicalize().unwrap())
    }
}

impl Drop for Temp {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// What the fake cell was asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Call {
    CopyIn {
        host: PathBuf,
        guest: String,
        owner: Option<GuestUser>,
    },
    CopyOut {
        guest: String,
        host: PathBuf,
    },
    Exec(Vec<String>),
    /// A command run as root.
    ExecRoot(Vec<String>),
}

/// A folder cell whose browser is a fake page.
pub struct Cells(Arc<FakeDriver>);

impl Cells {
    pub fn new(root: &Path, page: Page) -> Cells {
        let driver = folder_cells(root, |_| None);
        driver.on_stream(move |_| {
            let (ours, theirs) = duplex();
            tokio::spawn(FakeBidi::on(page.clone()).serve(theirs));
            Ok(ours)
        });
        Cells(Arc::new(driver))
    }

    pub fn cell(&self) -> eidolon::cell::Cell {
        running(&self.0, &cell())
    }

    pub fn guest(&self, path: &str) -> PathBuf {
        self.0.folder(&cell(), path).unwrap()
    }

    pub fn calls(&self) -> Vec<Call> {
        self.0
            .calls()
            .into_iter()
            .filter_map(|c| match c {
                DriverCall::CopyIn { host, guest, owner, .. } => Some(Call::CopyIn { host, guest, owner }),
                DriverCall::CopyOut { guest, host, .. } => Some(Call::CopyOut { guest, host }),
                DriverCall::Exec(e) if e.root => Some(Call::ExecRoot(e.argv)),
                DriverCall::Exec(e) => Some(Call::Exec(e.argv)),
                _ => None,
            })
            .collect()
    }

    pub fn copies(&self) -> usize {
        self.calls().iter().filter(|c| matches!(c, Call::CopyIn { .. } | Call::CopyOut { .. })).count()
    }
}

pub struct Env {
    pub tools: Tools,
    pub page: Page,
    pub driver: Cells,
    /// The host files folder.
    pub files: PathBuf,
    /// The folder holding `files` and the guest root.
    pub dir: Temp,
}

impl Env {
    /// Another set of tools on the same browser, driver and folders, as a
    /// server of its own would have.
    pub fn tools_again(&self) -> Tools {
        tools(&self.driver, &self.files)
    }
}

fn tools(driver: &Cells, files: &Path) -> Tools {
    let options = ConnectOptions { attempts: 1, gap: Duration::ZERO, ready_within: Duration::from_secs(5) };
    Tools::new(Browser::new(driver.cell(), options), driver.cell(), files.to_path_buf())
}

/// Folder cells whose browser is a fake page; every connection to it is
/// served on that same page.
pub async fn env() -> Env {
    let dir = Temp::new();
    let files = dir.0.join("files");
    std::fs::create_dir_all(&files).unwrap();
    let page = FakeBidi::new().page();
    let driver = Cells::new(&dir.0.join("cells"), page.clone());
    let tools = tools(&driver, &files);
    Env { tools, page, driver, files, dir }
}

/// Runs the tool `name` with JSON arguments, as the MCP server does.
pub async fn call(tools: &Tools, name: &str, arguments: Value) -> eidolon::Result<Vec<Part>> {
    tools.run(Tool::parse(name, arguments)?).await
}

/// The refusal code of a result, if it was refused.
pub fn code<T>(r: &eidolon::Result<T>) -> Option<RefusalCode> {
    match r {
        Err(eidolon::Error::Refused(r)) => Some(r.code),
        _ => None,
    }
}

/// Every command the browser was sent beyond connecting.
pub fn commands(page: &Page) -> Vec<String> {
    page.methods().into_iter().filter(|m| m != "session.status" && m != "session.new").collect()
}

/// Writes a file, making its folders.
pub fn write(path: &Path, text: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, text).unwrap();
}
