//! The `eidolon` command. Every command takes `--json`. Exit 0 is done, 1 is
//! a refusal or a failure, 2 is a mistake in how it was typed. `receive` and
//! `mcp` own standard output, so their results and errors go to standard
//! error.

pub mod args;

use args::{Cmd, Settle, Usage};
use eidolon::cell::{command_stream, Cells};
use eidolon::home::{Home, FILE_STORE_WARNING};
use eidolon::model::{EidolonId, Holder, SignedSeat};
use eidolon::moving::MoveStats;
use eidolon::Error;
use serde_json::{json, Value};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::process::ExitCode;
use std::time::Duration;

enum Failure {
    Usage(Usage),
    Failed(Error),
}

impl From<Error> for Failure {
    fn from(e: Error) -> Failure {
        Failure::Failed(e)
    }
}

impl Failure {
    fn code(&self) -> String {
        match self {
            Failure::Usage(_) => "usage".to_owned(),
            Failure::Failed(e) => e.code(),
        }
    }

    fn message_and_fix(&self) -> (String, String) {
        match self {
            Failure::Usage(Usage(text)) => (text.clone(), "Run `eidolon help` for the commands.".to_owned()),
            Failure::Failed(Error::Refused(r)) => (r.message.clone(), r.fix.clone()),
            Failure::Failed(Error::RefusedByPeer(r)) => (format!("The other machine refused: {}", r.message), r.fix.clone()),
            Failure::Failed(e) => (e.to_string(), String::new()),
        }
    }

    fn exit(&self) -> u8 {
        if matches!(self, Failure::Usage(_)) {
            2
        } else {
            1
        }
    }
}

type Result<T> = std::result::Result<T, Failure>;

struct Report {
    json: Value,
    human: String,
}

fn report(json: Value, human: impl Into<String>) -> Report {
    Report { json, human: human.into() }
}

pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> ExitCode {
    let parsed = args::parse(args);
    let json = parsed.as_ref().map_or_else(|_| args.iter().any(|a| a == "--json"), |p| p.json);
    let owns_stdout = matches!(parsed, Ok(args::Parsed { cmd: Cmd::Receive | Cmd::Mcp { .. }, .. }));
    let done = match parsed {
        Err(e) => Err(Failure::Usage(e)),
        Ok(parsed) => match tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all().build() {
            Ok(runtime) => runtime.block_on(dispatch(parsed.cmd, json, &mut *out, &mut *err)),
            Err(e) => Err(Failure::Failed(Error::Io { what: "start the runtime", source: e })),
        },
    };
    let sink: &mut dyn Write = if owns_stdout { err } else { out };
    match done {
        Ok(None) => ExitCode::SUCCESS,
        Ok(Some(report)) => {
            let _ = writeln!(sink, "{}", if json { report.json.to_string() } else { report.human });
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let (message, fix) = failure.message_and_fix();
            if json {
                let _ = writeln!(sink, "{}", json!({ "ok": false, "code": failure.code(), "message": message, "fix": fix }));
            } else {
                let _ = writeln!(err, "eidolon: {}", format!("{message} {fix}").trim_end());
            }
            ExitCode::from(failure.exit())
        }
    }
}

fn stdin_bytes() -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    std::io::stdin().read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn read_input(from: &str) -> Result<Vec<u8>> {
    let read = if from == "-" { stdin_bytes() } else { std::fs::read(from) };
    read.map_err(|source| Failure::Failed(Error::Io { what: "read the input file", source }))
}

fn parse_json<T: serde::de::DeserializeOwned>(what: &'static str, bytes: &[u8]) -> Result<T> {
    serde_json::from_slice(bytes).map_err(|e| Failure::Failed(Error::Damaged { what, why: e.to_string() }))
}

fn secs(d: Duration) -> f64 {
    (d.as_secs_f64() * 1000.0).round() / 1000.0
}

fn moved(stats: &MoveStats) -> Value {
    json!({ "chunks_offered": stats.chunks_offered, "bytes_sent": stats.bytes_sent, "bytes_received": stats.bytes_received, "seconds": secs(stats.elapsed) })
}

fn merge(mut a: Value, b: Value) -> Value {
    if let (Some(a), Value::Object(b)) = (a.as_object_mut(), b) {
        a.extend(b);
    }
    a
}

async fn dispatch(cmd: Cmd, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> Result<Option<Report>> {
    if cmd == Cmd::Help {
        return Ok(Some(report(json!({ "ok": true, "usage": args::USAGE }), args::USAGE)));
    }
    let home = Home::from_env()?;
    match cmd {
        Cmd::Help => Ok(None),
        Cmd::MachineInit { name } => {
            let machine = home.init(&name)?;
            Ok(Some(report(
                json!({ "ok": true, "holder": machine.holder, "key_store": "file", "warning": FILE_STORE_WARNING }),
                format!("{name} is set up. Its holder entry is `eidolon machine show`.\nwarning: {FILE_STORE_WARNING}"),
            )))
        }
        Cmd::MachineShow => {
            let holder = serde_json::to_value(home.machine()?.holder).unwrap_or_default();
            Ok(Some(report(holder.clone(), holder.to_string())))
        }
        Cmd::New { wall, holders, facets } => {
            home.machine()?;
            let wall = wall.parse().map_err(|_| Failure::Failed(Error::BadValue { what: "wall", rule: "`wall_` and 1 to 64 of a-z, 0-9, _ or -" }))?;
            let holders = holders.iter().map(|h| parse_json::<Holder>("holder entry", &read_input(h)?)).collect::<Result<Vec<_>>>()?;
            let id = home.create(wall, holders, facets)?;
            Ok(Some(report(json!({ "ok": true, "eidolon": id, "generation": 1 }), id.to_string())))
        }
        Cmd::Seal { eidolon, cell } => {
            let s = home.seal(&Cells::system()?, &eidolon, cell.as_ref()).await?;
            let human = format!("Sealed {eidolon} from {}: {} files, {} chunks, {}s.", s.cell, s.files, s.chunks, secs(s.elapsed));
            Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "cell": s.cell, "files": s.files, "chunks": s.chunks, "seconds": secs(s.elapsed) }), human)))
        }
        Cmd::Open { eidolon, cell } => {
            let o = home.open(&Cells::system()?, &eidolon, &cell).await?;
            let mut human = format!("Opened {eidolon} in {cell}.");
            if let Some(earlier) = &o.cleared {
                let _ = write!(human, "\nAn earlier open had begun in {earlier} and not finished; the files it placed there are cleared.");
            }
            Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "cell": cell, "provisioned": o.provisioned, "cleared": o.cleared, "seconds": secs(o.elapsed) }), human)))
        }
        Cmd::Move { eidolon, argv } => move_to(&home, &eidolon, &argv).await,
        Cmd::Receive => {
            let mut stream = tokio::io::join(tokio::io::stdin(), tokio::io::stdout());
            let stats = home.receive(&mut stream).await?;
            let human = format!("Received {} chunks of {} offered.", stats.chunks_sent, stats.chunks_offered);
            Ok(Some(report(merge(json!({ "ok": true, "chunks_received": stats.chunks_sent }), moved(&stats)), human)))
        }
        Cmd::Settle { eidolon, how } => match how {
            Settle::Abandon => {
                home.abandon_move(&eidolon)?;
                Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "settled": "abandoned" }), format!("{eidolon} is abandoned."))))
            }
            Settle::Confirm(from) => {
                let taken: SignedSeat = parse_json("seat", &read_input(&from)?)?;
                home.confirm_move(&eidolon, &taken)?;
                Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "settled": "confirmed", "generation": taken.record.generation }), format!("{eidolon} is confirmed."))))
            }
        },
        Cmd::Verify { eidolon } => {
            let chunks = home.verify(&eidolon)?;
            Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "chunks": chunks }), format!("{eidolon} is whole: {chunks} chunks open."))))
        }
        Cmd::Facets { eidolon } => {
            let facets = home.holding(&eidolon)?.spec.facets;
            let rows: Vec<String> = facets.iter().map(|f| format!("{} {:?} {}", f.name, f.kind, f.path)).collect();
            Ok(Some(report(json!({ "ok": true, "eidolon": eidolon, "facets": facets }), rows.join("\n"))))
        }
        Cmd::Ls => {
            let held = home.list()?;
            let rows: Vec<Value> = held
                .iter()
                .map(|h| json!({ "eidolon": h.eidolon, "wall": h.wall, "state": h.state, "generation": h.generation, "seat_here": h.seat_here, "pending_move": h.pending_move, "facets": h.facets }))
                .collect();
            let human: Vec<String> = held
                .iter()
                .map(|h| format!("{} {} {} generation {}{}", h.eidolon, h.wall, json!(h.state), h.generation, if h.pending_move { " (move pending)" } else { "" }))
                .collect();
            Ok(Some(report(json!({ "ok": true, "eidolons": rows }), human.join("\n"))))
        }
        Cmd::View { eidolon } => view(&home, &eidolon, json, out, err).await,
        Cmd::Mcp { eidolon } => {
            let cells = Cells::system()?;
            let (cell, _guard) = home.browser_cell(&cells, &eidolon).await?;
            let files = home.files_dir(&eidolon)?;
            let _ = writeln!(err, "eidolon: serving the browser of {eidolon} in {} on standard input and output. Files go through {}.", cell.name(), files.display());
            let browser = eidolon::browser::Browser::new(cell.clone(), eidolon::browser::ConnectOptions::default());
            eidolon::mcp::serve_stdio(eidolon::mcp::Tools::new(browser, cell, files)).await?;
            Ok(None)
        }
    }
}

/// The receiving side's command runs as an argument list, with an empty
/// environment but for what a remote shell or a second home needs.
async fn move_to(home: &Home, eidolon: &EidolonId, argv: &[String]) -> Result<Option<Report>> {
    let pass = ["PATH", "HOME", "EIDOLON_HOME", "SSH_AUTH_SOCK", "ADYTUM_HOME"];
    let mut duplex = command_stream(argv, &pass, Duration::from_secs(24 * 60 * 60)).await?;
    let sent = home.send(eidolon, &mut duplex).await;
    let finished = duplex.finish().await;
    let (stats, generation) = match sent {
        Ok(done) => done,
        Err(e @ (Error::Cut | Error::Protocol(_) | Error::Io { .. })) => {
            let said = finished.map(|o| o.stderr_lossy()).unwrap_or_default();
            let failure = Failure::Failed(e);
            if said.trim().is_empty() {
                return Err(failure);
            }
            let (message, _) = failure.message_and_fix();
            return Err(Failure::Failed(Error::Protocol(format!("{message} The receiving side said: {}", said.trim()))));
        }
        Err(e) => return Err(e.into()),
    };
    let human = format!("Moved {eidolon}: {} of {} chunks sent, {} bytes, {}s.", stats.chunks_sent, stats.chunks_offered, stats.bytes_sent, secs(stats.elapsed));
    let receiver_status = finished.ok().and_then(|o| o.status);
    Ok(Some(report(
        merge(json!({ "ok": true, "eidolon": eidolon, "generation": generation, "chunks_sent": stats.chunks_sent, "receiver_status": receiver_status }), moved(&stats)),
        human,
    )))
}

async fn view(home: &Home, eidolon: &EidolonId, json: bool, out: &mut dyn Write, err: &mut dyn Write) -> Result<Option<Report>> {
    let cells = Cells::system()?;
    let (cell, guard) = home.browser_cell(&cells, eidolon).await?;
    let name = cell.name().clone();
    let mut window = eidolon::view::View::open(cell, guard).await?;
    let secret = window.secret().unwrap_or_default();
    if json {
        let _ = writeln!(
            out,
            "{}",
            json!({ "ok": true, "event": "open", "eidolon": eidolon, "cell": name, "address": window.address().to_string(), "url": window.url(), "secret": secret })
        );
        let _ = out.flush();
    } else {
        let _ = writeln!(
            err,
            "The sign-in window for {eidolon} is open.\n  Address: {}\n  Secret:  {secret}\nOpen the address in Screen Sharing and sign in with the secret. The secret works for this session only.\nWhen you are done, press Enter here (or Ctrl-C). Until then the browser is not driven.",
            window.url()
        );
    }
    wait_for_the_person().await;
    window.close().await?;
    Ok(Some(report(json!({ "ok": true, "event": "closed", "eidolon": eidolon }), format!("Closed the window. The browser of {eidolon} is driven again."))))
}

/// Waits for Enter, Ctrl-C or the terminate signal. A closed standard input
/// is not an answer.
async fn wait_for_the_person() {
    use tokio::io::AsyncBufReadExt as _;
    let mut line = String::new();
    let mut input = tokio::io::BufReader::new(tokio::io::stdin());
    let enter = async {
        if matches!(input.read_line(&mut line).await, Ok(n) if n > 0) {
            return;
        }
        std::future::pending::<()>().await;
    };
    #[cfg(unix)]
    let term = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut s) => drop(s.recv().await),
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let term = std::future::pending::<()>();
    tokio::select! {
        () = enter => {}
        _ = tokio::signal::ctrl_c() => {}
        () = term => {}
    }
}
