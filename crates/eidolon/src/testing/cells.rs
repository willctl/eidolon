use crate::cell::{Cell, CellName, Output};
use adytum::testing::{failed, ok, Exec, FakeDriver};
use adytum::CellState;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Adytum's folder-backed [`FakeDriver`], cell `adytum-x` at `<root>/adytum-x`,
/// running the commands Eidolon's flows run on a cell's files: `rm -rf`,
/// `mv`, `find` of a folder's files, and `chmod`, which changes nothing.
/// `answer` takes a command first; any other command exits 127, as a
/// program the guest lacks would.
pub fn folder_cells(root: impl Into<PathBuf>, mut answer: impl FnMut(&Exec) -> Option<Output> + Send + 'static) -> FakeDriver {
    let root = root.into();
    let driver = FakeDriver::in_folders(root.clone());
    driver.on_exec(move |exec| answer(exec).unwrap_or_else(|| on_folders(&root, exec).unwrap_or_else(|e| failed(1, &e.to_string()))));
    driver
}

/// `name` as a running cell of `driver`.
pub fn running(driver: &Arc<FakeDriver>, name: &CellName) -> Cell {
    driver.add_cell(name, CellState::Running);
    Cell::new(Arc::clone(driver) as Arc<dyn adytum::Driver>, name.clone())
}

fn on_folders(root: &Path, exec: &Exec) -> std::io::Result<Output> {
    let at = |guest: &str| root.join(exec.cell.as_str()).join(guest.trim_start_matches('/'));
    let words: Vec<&str> = exec.argv.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["rm", "-rf", "--", paths @ ..] => {
            for p in paths {
                let host = at(p);
                if host.is_dir() {
                    std::fs::remove_dir_all(&host)?;
                } else if host.symlink_metadata().is_ok() {
                    std::fs::remove_file(&host)?;
                }
            }
        }
        ["mv", "--", from, to] => std::fs::rename(at(from), at(to))?,
        ["find", dir, "-maxdepth", "1", "-type", "f", "-printf", _] => {
            let mut out = String::new();
            if let Ok(entries) = std::fs::read_dir(at(dir)) {
                for entry in entries.flatten() {
                    let meta = entry.metadata()?;
                    if meta.is_file() {
                        let _ = write!(out, "{}\0{}\0", entry.file_name().to_string_lossy(), meta.len());
                    }
                }
            }
            return Ok(ok(&out));
        }
        ["chmod", ..] => {}
        _ => return Ok(failed(127, &format!("{}: not run by the folder cells", exec.argv.join(" ")))),
    }
    Ok(ok(""))
}
