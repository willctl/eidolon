use crate::bidi::Error as BidiError;
use crate::browser::Browser;
use crate::cell::Cell;
use crate::error::{Error, Result};
use crate::facet::firefox::{self, DOWNLOADS_PATH, UPLOADS_PATH};
use crate::mcp::snapshot::{self, PAGE_TEXT_JS, PREPARE_FILL_JS, SNAPSHOT_JS};
use crate::mcp::{files, keys};
use crate::model::RefusalCode;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

const MAX_TEXT: usize = 100_000;
const COMMAND_LIMIT: Duration = Duration::from_secs(60);

pub mod args {
    use super::{Deserialize, JsonSchema};

    /// No arguments.
    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct Empty {}

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct NewPage {
        /// The address to open. Leave out for a blank page.
        pub url: Option<String>,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct PageId {
        /// The page's id, from list_pages.
        pub id: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct NavigatePage {
        /// The address to open: http, https or about:blank.
        pub url: String,
    }

    /// Which way to go in the page's history.
    #[derive(Debug, Clone, Copy, Deserialize, JsonSchema, PartialEq, Eq)]
    #[serde(rename_all = "lowercase")]
    pub enum Direction {
        Back,
        Forward,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct NavigateHistory {
        /// back or forward.
        pub direction: Direction,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct Uid {
        /// An element's uid from the last take_snapshot.
        pub uid: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct FillByUid {
        /// An element's uid from the last take_snapshot.
        pub uid: String,
        /// The text to put in the field, or the option to choose in a select.
        pub value: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct TypeText {
        /// The text to type into the element that has focus.
        pub text: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct PressKey {
        /// A key name such as Enter, Tab, Escape or ArrowDown, a character, or a combination such as Control+a.
        pub key: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct UploadFileByUid {
        /// The file input's uid from the last take_snapshot.
        pub uid: String,
        /// The file to upload: a path inside the shared files folder, absolute or relative to it.
        #[serde(rename = "filePath", alias = "host_path")]
        pub file_path: String,
    }

    #[derive(Debug, Deserialize, JsonSchema)]
    #[serde(deny_unknown_fields)]
    pub struct FetchDownload {
        /// The file's name from list_downloads.
        pub name: String,
    }
}

#[derive(Debug, Clone)]
pub struct ToolDef {
    pub name: &'static str,
    pub description: &'static str,
    pub schema: Map<String, Value>,
}

fn schema_of<T: JsonSchema>() -> Map<String, Value> {
    let mut map = match serde_json::to_value(schemars::schema_for!(T)) {
        Ok(Value::Object(map)) => map,
        _ => Map::new(),
    };
    map.remove("$schema");
    map.remove("title");
    // Clients read `properties` on every object schema, so a tool with no
    // arguments says so with an empty one.
    map.entry("properties").or_insert_with(|| json!({}));
    map
}

fn bad_arguments(message: String) -> Error {
    Error::refused(RefusalCode::BadArguments, message, "Call the tool with the arguments its schema lists.")
}

macro_rules! tools {
    ($($variant:ident($args:ty) $name:literal $description:literal,)*) => {
        /// A tool call. The names are the Firefox MCP server's, so skills written for it carry over.
        #[derive(Debug)]
        pub enum Tool {
            $($variant($args),)*
        }

        pub fn definitions() -> Vec<ToolDef> {
            vec![$(ToolDef { name: $name, description: $description, schema: schema_of::<$args>() },)*]
        }

        impl Tool {
            pub fn parse(name: &str, arguments: Value) -> Result<Tool> {
                let arguments = if arguments.is_null() { json!({}) } else { arguments };
                let read = |e: serde_json::Error| bad_arguments(format!("The arguments for {name} are not right: {e}."));
                match name {
                    $($name => Ok(Tool::$variant(serde_json::from_value(arguments).map_err(read)?)),)*
                    other => Err(bad_arguments(format!("There is no tool named {other}."))),
                }
            }
        }
    };
}

tools! {
    ListPages(args::Empty) "list_pages" "List the open pages with their ids, addresses and titles.",
    NewPage(args::NewPage) "new_page" "Open a new page, optionally at an address, and select it.",
    SelectPage(args::PageId) "select_page" "Make a page the one the other tools act on.",
    ClosePage(args::PageId) "close_page" "Close a page.",
    NavigatePage(args::NavigatePage) "navigate_page" "Open an address in the selected page and wait for it to finish loading.",
    NavigateHistory(args::NavigateHistory) "navigate_history" "Go back or forward in the selected page's history.",
    TakeSnapshot(args::Empty) "take_snapshot" "List the elements on the selected page with a uid for each; the uids work until the page navigates.",
    ClickByUid(args::Uid) "click_by_uid" "Click an element from the last snapshot.",
    HoverByUid(args::Uid) "hover_by_uid" "Move the pointer over an element from the last snapshot.",
    FillByUid(args::FillByUid) "fill_by_uid" "Replace a field's content with a value, or choose an option in a select.",
    TypeText(args::TypeText) "type_text" "Type text into the element that has focus.",
    PressKey(args::PressKey) "press_key" "Press a key or a combination such as Control+a.",
    GetPageText(args::Empty) "get_page_text" "Return the visible text of the selected page.",
    ScreenshotPage(args::Empty) "screenshot_page" "Take a PNG screenshot of the selected page.",
    UploadFileByUid(args::UploadFileByUid) "upload_file_by_uid" "Choose a file from the shared files folder in a file input from the last snapshot.",
    ListDownloads(args::Empty) "list_downloads" "List the files the browser has downloaded in the cell.",
    FetchDownload(args::FetchDownload) "fetch_download" "Copy a finished download to the downloads folder inside the shared files folder.",
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Part {
    Text(String),
    Image { data: String, mime: &'static str },
}

#[derive(Default)]
struct State {
    selected: Option<String>,
    /// Never reused, so an old uid cannot name an element of a newer snapshot.
    next_uid: u64,
    uids: HashMap<String, HashMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Download {
    pub name: String,
    pub size: u64,
    pub finished: bool,
}

pub struct Tools {
    browser: Browser,
    cell: Cell,
    files: PathBuf,
    state: Mutex<State>,
}

impl Tools {
    pub fn new(browser: Browser, cell: Cell, files: PathBuf) -> Tools {
        Tools { browser, cell, files, state: Mutex::new(State::default()) }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        // Nothing panics while the lock is held, so a poisoned one still holds consistent data.
        self.state.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    pub async fn run(&self, tool: Tool) -> Result<Vec<Part>> {
        let text = |s: String| vec![Part::Text(s)];
        Ok(match tool {
            Tool::ListPages(_) => text(self.list_pages().await?),
            Tool::NewPage(a) => text(self.new_page(a.url.as_deref()).await?),
            Tool::SelectPage(a) => text(self.select_page(&a.id).await?),
            Tool::ClosePage(a) => text(self.close_page(&a.id).await?),
            Tool::NavigatePage(a) => text(self.navigate_page(&a.url).await?),
            Tool::NavigateHistory(a) => text(self.navigate_history(a.direction).await?),
            Tool::TakeSnapshot(_) => text(self.take_snapshot().await?),
            Tool::ClickByUid(a) => text(self.click_by_uid(&a.uid).await?),
            Tool::HoverByUid(a) => text(self.hover_by_uid(&a.uid).await?),
            Tool::FillByUid(a) => text(self.fill_by_uid(&a.uid, &a.value).await?),
            Tool::TypeText(a) => text(self.type_text(&a.text).await?),
            Tool::PressKey(a) => text(self.press_key(&a.key).await?),
            Tool::GetPageText(_) => text(self.get_page_text().await?),
            Tool::ScreenshotPage(_) => vec![Part::Image { data: self.screenshot_page().await?, mime: "image/png" }],
            Tool::UploadFileByUid(a) => text(self.upload_file_by_uid(&a.uid, &a.file_path).await?),
            Tool::ListDownloads(_) => text(self.list_downloads().await?),
            Tool::FetchDownload(a) => text(self.fetch_download(&a.name).await?),
        })
    }

    /// Bounds a command in time and turns what an agent can act on into refusals.
    async fn browser<T>(&self, command: impl std::future::Future<Output = Result<T>>) -> Result<T> {
        let answer = tokio::time::timeout(COMMAND_LIMIT, command).await.map_err(|_| Error::Bidi(BidiError::Timeout(COMMAND_LIMIT)))?;
        answer.map_err(|e| match &e {
            Error::Bidi(BidiError::Remote { code, .. }) if code == "no such frame" => {
                self.state().selected = None;
                Error::refused(RefusalCode::PageGone, "The selected page is gone.", "Call list_pages, then select_page.")
            }
            Error::Bidi(BidiError::Remote { code, .. }) if matches!(code.as_str(), "no such node" | "no such element" | "stale element reference") => {
                Error::refused(RefusalCode::StaleElement, "The element is no longer on the page.", "Call take_snapshot again and use a new uid.")
            }
            _ => e,
        })
    }

    async fn page(&self) -> Result<String> {
        if let Some(selected) = self.state().selected.clone() {
            return Ok(selected);
        }
        let first = self.browser(self.browser.contexts()).await?.into_iter().next();
        let first = first.ok_or_else(|| Error::refused(RefusalCode::NoPage, "There is no page open.", "Call new_page to open one."))?.context;
        self.state().selected = Some(first.clone());
        Ok(first)
    }

    fn element(&self, uid: &str) -> Result<(String, String)> {
        let state = self.state();
        let found = state.selected.clone().and_then(|page| state.uids.get(&page).and_then(|m| m.get(uid)).map(|id| (page, id.clone())));
        found.ok_or_else(|| {
            Error::refused(RefusalCode::UnknownUid, format!("There is no element {uid} in the last snapshot of this page."), "Call take_snapshot and use a uid from it.")
        })
    }

    fn no_such_page(id: &str) -> Error {
        Error::refused(RefusalCode::NoSuchPage, format!("There is no page {id}."), "Call list_pages to see the open pages.")
    }

    pub async fn list_pages(&self) -> Result<String> {
        let contexts = self.browser(self.browser.contexts()).await?;
        if contexts.is_empty() {
            return Ok("No pages are open.".to_owned());
        }
        let selected = self.state().selected.clone();
        let mut out = String::new();
        for c in contexts {
            // A page still loading may not answer for its title yet.
            let title = self.browser(self.browser.title(&c.context)).await.unwrap_or_default();
            let mark = if selected.as_deref() == Some(c.context.as_str()) { " (selected)" } else { "" };
            let _ = writeln!(out, "{}{mark} {} {title:?}", c.context, c.url);
        }
        Ok(out)
    }

    pub async fn new_page(&self, url: Option<&str>) -> Result<String> {
        if let Some(url) = url {
            check_url(url)?;
        }
        let id = self.browser(self.browser.new_tab()).await?;
        self.state().selected = Some(id.clone());
        match url {
            Some(url) => Ok(format!("Opened page {id} at {}. It is selected.", self.navigate(&id, url).await?)),
            None => Ok(format!("Opened page {id}. It is selected.")),
        }
    }

    pub async fn select_page(&self, id: &str) -> Result<String> {
        if !self.browser(self.browser.contexts()).await?.iter().any(|c| c.context == id) {
            return Err(Self::no_such_page(id));
        }
        self.state().selected = Some(id.to_owned());
        // Brings the tab to the front for a person watching; driving it does not need that.
        let _ = self.browser(self.browser.activate(id)).await;
        Ok(format!("Page {id} is selected."))
    }

    pub async fn close_page(&self, id: &str) -> Result<String> {
        self.browser(self.browser.close(id)).await.map_err(|e| match e {
            Error::Refused(r) if r.code == RefusalCode::PageGone => Self::no_such_page(id),
            other => other,
        })?;
        let mut state = self.state();
        state.uids.remove(id);
        if state.selected.as_deref() == Some(id) {
            state.selected = None;
        }
        Ok(format!("Closed page {id}."))
    }

    async fn navigate(&self, page: &str, url: &str) -> Result<String> {
        let at = self.browser(self.browser.navigate(page, url)).await?;
        self.state().uids.remove(page);
        Ok(at)
    }

    pub async fn navigate_page(&self, url: &str) -> Result<String> {
        check_url(url)?;
        let page = self.page().await?;
        Ok(format!("Loaded {}. Call take_snapshot to see it.", self.navigate(&page, url).await?))
    }

    pub async fn navigate_history(&self, direction: args::Direction) -> Result<String> {
        let page = self.page().await?;
        let delta = if direction == args::Direction::Back { -1 } else { 1 };
        self.browser(self.browser.traverse_history(&page, delta)).await?;
        self.state().uids.remove(&page);
        Ok("Done. Call take_snapshot to see the page.".to_owned())
    }

    pub async fn take_snapshot(&self) -> Result<String> {
        let page = self.page().await?;
        let remote = self.browser(self.browser.call_function(&page, SNAPSHOT_JS, Vec::new())).await?;
        let first = self.state().next_uid.max(1);
        let (snap, next) = snapshot::parse(&remote, first)?;
        let mut state = self.state();
        state.next_uid = next;
        state.uids.insert(page, snap.uids);
        Ok(snap.text)
    }

    pub async fn get_page_text(&self) -> Result<String> {
        let page = self.page().await?;
        let remote = self.browser(self.browser.call_function(&page, PAGE_TEXT_JS, Vec::new())).await?;
        let text = remote.get("value").and_then(Value::as_str).unwrap_or_default();
        if text.chars().count() > MAX_TEXT {
            let cut: String = text.chars().take(MAX_TEXT).collect();
            return Ok(format!("{cut}\n(the text is longer; the rest is left out.)"));
        }
        Ok(text.to_owned())
    }

    pub async fn screenshot_page(&self) -> Result<String> {
        let page = self.page().await?;
        self.browser(self.browser.screenshot(&page)).await
    }

    pub async fn click_by_uid(&self, uid: &str) -> Result<String> {
        let (page, id) = self.element(uid)?;
        self.browser(self.browser.point_at(&page, &id, true)).await?;
        Ok(format!("Clicked {uid}."))
    }

    pub async fn hover_by_uid(&self, uid: &str) -> Result<String> {
        let (page, id) = self.element(uid)?;
        self.browser(self.browser.point_at(&page, &id, false)).await?;
        Ok(format!("Pointer is over {uid}."))
    }

    pub async fn fill_by_uid(&self, uid: &str, value: &str) -> Result<String> {
        let (page, id) = self.element(uid)?;
        let phase = |p: &str| vec![json!({ "sharedId": id }), json!({ "type": "string", "value": value }), json!({ "type": "string", "value": p })];
        let probe = self.browser(self.browser.call_function(&page, PREPARE_FILL_JS, phase("probe"))).await?;
        match probe.get("value").and_then(Value::as_str) {
            Some("selected") => return Ok(format!("Chose {value:?} in {uid}.")),
            Some("no-option") => return Err(bad_arguments(format!("{uid} has no option {value:?}. Call take_snapshot or get_page_text to see the options."))),
            _ => {}
        }
        self.browser(self.browser.point_at(&page, &id, true)).await?;
        self.browser(self.browser.call_function(&page, PREPARE_FILL_JS, phase("focus"))).await?;
        self.browser(self.browser.type_text(&page, value)).await?;
        Ok(format!("Filled {uid}."))
    }

    pub async fn type_text(&self, text: &str) -> Result<String> {
        let page = self.page().await?;
        self.browser(self.browser.type_text(&page, text)).await?;
        Ok("Typed.".to_owned())
    }

    pub async fn press_key(&self, key: &str) -> Result<String> {
        let actions = keys::press_actions(key)?;
        let page = self.page().await?;
        self.browser(self.browser.perform_actions(&page, actions)).await?;
        Ok(format!("Pressed {key}."))
    }

    pub async fn upload_file_by_uid(&self, uid: &str, file_path: &str) -> Result<String> {
        let source = files::upload_source(&self.files, file_path)?;
        let name = files::upload_name(&source)?;
        let (page, id) = self.element(uid)?;
        let guest = format!("{UPLOADS_PATH}/{name}");
        self.cell.copy_in(&source, &guest, Some(&firefox::user()?)).await?;
        self.browser(self.browser.set_files(&page, &id, &[&guest])).await?;
        Ok(format!("Chose {name} in {uid}."))
    }

    pub async fn downloads(&self) -> Result<Vec<Download>> {
        let out = self.cell.run(&["find", DOWNLOADS_PATH, "-maxdepth", "1", "-type", "f", "-printf", "%f\\0%s\\0"]).await?;
        crate::cell::succeeded("listing the downloads folder", &out)?;
        let text = out.stdout_lossy();
        let fields: Vec<&str> = text.split('\0').collect();
        let mut found: Vec<Download> = fields
            .as_chunks::<2>()
            .0
            .iter()
            .filter(|[name, _]| !name.is_empty())
            .map(|[name, size]| Download {
                name: (*name).to_owned(),
                size: size.parse().unwrap_or(0),
                finished: !Path::new(name).extension().is_some_and(|e| e.eq_ignore_ascii_case("part")),
            })
            .collect();
        found.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(found)
    }

    pub async fn list_downloads(&self) -> Result<String> {
        let found = self.downloads().await?;
        if found.is_empty() {
            return Ok("There are no downloads.".to_owned());
        }
        let mut out = String::new();
        for d in found {
            let _ = writeln!(out, "{} {} bytes{}", d.name, d.size, if d.finished { "" } else { " (still downloading)" });
        }
        Ok(out)
    }

    pub async fn fetch_download(&self, name: &str) -> Result<String> {
        files::check_name(name)?;
        let found = self.downloads().await?;
        let download = found
            .iter()
            .find(|d| d.name == name)
            .ok_or_else(|| Error::refused(RefusalCode::NoSuchDownload, format!("There is no finished download named {name}."), "Call list_downloads to see what is there."))?;
        if !download.finished {
            return Err(Error::refused(RefusalCode::StillDownloading, format!("{name} is still downloading."), "Wait and call list_downloads again."));
        }
        let target = files::free_path(&files::downloads_dir(&self.files)?, name);
        self.cell.copy_out(&format!("{DOWNLOADS_PATH}/{name}"), &target).await?;
        let saved = target.file_name().and_then(|n| n.to_str()).unwrap_or(name);
        Ok(format!("Saved {} bytes as {}/{saved} in the files folder.", download.size, files::DOWNLOADS_DIR))
    }
}

/// Only web addresses and the blank page may be opened: `file:` would read
/// the browser's own profile, `about:` pages such as logins show saved
/// passwords, and `javascript:` and `data:` run script.
pub fn check_url(url: &str) -> Result<()> {
    let lower = url.trim().to_ascii_lowercase();
    if lower.starts_with("http://") || lower.starts_with("https://") || lower == "about:blank" {
        return Ok(());
    }
    Err(Error::refused(RefusalCode::UnsafeUrl, format!("{url:?} cannot be opened."), "Only http and https addresses and about:blank are allowed."))
}
