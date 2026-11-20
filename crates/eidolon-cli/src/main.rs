//! The `eidolon` command. Every command takes `--json`. Exit 0 is done, 1 is
//! a refusal or a failure, 2 is a mistake in how the command was typed.

use clap::{Args, Parser, Subcommand};
use eidolon::{Code, Dest, Error, HolderKey, Home, KeySource, Limits, MachineKey, Recipient, Settled, Source, Target};
use serde_json::{json, Value};
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::process::{Command, ExitCode, Stdio};

#[derive(Parser)]
#[command(name = "eidolon", version, about = "Seal a folder target as age(zstd(tar)), move it between your machines with one holder at a time, and open it safely.")]
struct Cli {
    /// The machine's Eidolon home.
    #[arg(long, global = true, env = "EIDOLON_HOME", value_name = "DIR")]
    home: Option<PathBuf>,
    /// An OpenSSH key file (mode 600) instead of the operating system's key store.
    #[arg(long, global = true, env = "EIDOLON_KEY_FILE", value_name = "FILE")]
    key_file: Option<PathBuf>,
    /// Print the result as JSON.
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// This machine's key.
    #[command(subcommand)]
    Key(KeyCmd),
    /// The machines that may hold a target.
    #[command(subcommand)]
    Holders(HoldersCmd),
    /// The folders Eidolon seals, and their rules.
    #[command(subcommand)]
    Target(TargetCmd),
    /// Capture a target into a new bundle, encrypted to every holder.
    Seal {
        target: String,
        /// Read this folder instead of the target's path.
        #[arg(long, value_name = "DIR", conflicts_with = "from_tar")]
        from: Option<PathBuf>,
        /// Read a tar stream instead; `-` is standard input.
        #[arg(long, value_name = "-")]
        from_tar: Option<String>,
        /// Also encrypt to this age recipient, for recovery. Repeatable.
        #[arg(short, long = "recipient", value_name = "RECIPIENT")]
        recipients: Vec<String>,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Restore the newest bundle, replacing the folder whole.
    Open {
        target: String,
        /// Restore into this folder instead of the target's path.
        #[arg(long, value_name = "DIR", conflicts_with = "tar")]
        to: Option<PathBuf>,
        /// Write a clean tar to standard output instead.
        #[arg(long)]
        tar: bool,
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Move a target to another holder over a command's stdin and stdout, or as a file.
    Send {
        target: String,
        /// The holder that takes the seat.
        #[arg(long)]
        to: String,
        /// Write the offer to this file instead of running a command.
        #[arg(long, value_name = "FILE", conflicts_with = "command")]
        out: Option<PathBuf>,
        /// After the ack, remove this machine's copy at the target's path.
        #[arg(long, conflicts_with = "out")]
        clear: bool,
        /// The command that carries the offer and brings the ack back, such as `ssh tower eidolon receive`.
        #[arg(last = true, value_name = "COMMAND")]
        command: Vec<OsString>,
    },
    /// Read an offer on standard input and write the ack on standard output.
    Receive {
        #[command(flatten)]
        limits: LimitArgs,
    },
    /// Finish a move that was cut off.
    Settle {
        target: String,
        /// The receiver's ack.
        #[arg(long, value_name = "ACK", conflicts_with = "abandon", required_unless_present = "abandon")]
        confirm: Option<PathBuf>,
        /// Drop the pending move. Only when the receiver surely took nothing.
        #[arg(long)]
        abandon: bool,
        /// After the ack, remove this machine's copy at the target's path.
        #[arg(long)]
        clear: bool,
    },
    /// Every target, who holds it, and any pending move.
    Ls,
    /// Check a target's seats, and its bundle where this machine holds it.
    Verify {
        target: String,
        #[command(flatten)]
        limits: LimitArgs,
    },
}

#[derive(Args, Clone, Copy)]
struct LimitArgs {
    /// The most a bundle may hold, unpacked, such as 16G or 500M.
    #[arg(long, value_name = "SIZE", value_parser = size, default_value = "16G")]
    max_size: u64,
}

impl LimitArgs {
    fn limits(self) -> Limits {
        Limits { max_bytes: self.max_size, ..Limits::default() }
    }
}

fn size(text: &str) -> Result<u64, String> {
    let (digits, shift) = match text.strip_suffix(['K', 'M', 'G', 'T']) {
        Some(d) => (d, [10, 20, 30, 40][text.chars().last().map_or(0, |c| "KMGT".find(c).unwrap_or(0))]),
        None => (text, 0),
    };
    digits.parse::<u64>().ok().and_then(|n| n.checked_mul(1 << shift)).ok_or_else(|| format!("{text:?} is not a size such as 16G"))
}

#[derive(Subcommand)]
enum KeyCmd {
    /// Make this machine's key, unless it has one.
    Init,
    /// Print the public key, or say there is none yet. Writes nothing.
    Show,
}

#[derive(Subcommand)]
enum HoldersCmd {
    /// List a machine by the key `eidolon key show` printed on it.
    Add {
        name: String,
        /// `ssh-ed25519 AAAA...`, in one or two words.
        #[arg(required = true, num_args = 1..)]
        key: Vec<String>,
    },
    Ls,
}

#[derive(Subcommand)]
enum TargetCmd {
    /// Add a target from a preset, a TOML file or just a folder, or set its path on this machine.
    Add {
        name: String,
        /// Take the rules of this preset (`firefox`). The default when the name is a preset.
        #[arg(long, conflicts_with = "file")]
        preset: Option<String>,
        /// Take the rules from this TOML file.
        #[arg(long, value_name = "TOML")]
        file: Option<PathBuf>,
        /// Where the target's folder is on this machine.
        #[arg(long, value_name = "DIR")]
        path: Option<PathBuf>,
    },
    Ls,
    /// Print a target's rules (or a preset's) and its state here.
    Show {
        name: String,
    },
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let json = cli.json;
    match run(cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            if json {
                let refusal = match &e {
                    Error::Refused(r) => json!(r),
                    Error::Io { .. } => json!({ "code": "io", "message": e.to_string(), "fix": "" }),
                };
                let _ = writeln!(io::stdout(), "{}", json!({ "error": refusal }));
            } else {
                eprintln!("eidolon: {e}");
            }
            ExitCode::FAILURE
        }
    }
}

struct Out {
    json: bool,
    to_stderr: bool,
}

impl Out {
    fn show(&self, value: Value, text: &str) -> eidolon::Result<()> {
        let line = if self.json { value.to_string() } else { text.trim_end().to_owned() };
        let failed = |e| Error::Io { doing: "print the result".into(), source: e };
        if self.to_stderr {
            writeln!(io::stderr(), "{line}").map_err(failed)
        } else {
            writeln!(io::stdout(), "{line}").map_err(failed)
        }
    }
}

fn home_dir(cli: &Cli) -> eidolon::Result<PathBuf> {
    if let Some(home) = &cli.home {
        return Ok(home.clone());
    }
    std::env::home_dir()
        .map(|h| h.join(".eidolon"))
        .ok_or_else(|| Error::refused(Code::NoPath, "There is no home folder to keep Eidolon's home in.", "Pass --home or set EIDOLON_HOME."))
}

fn run(cli: Cli) -> eidolon::Result<()> {
    let key = cli.key_file.clone().map_or(KeySource::Keyring, KeySource::File);
    let stdout_is_data = matches!(cli.command, Cmd::Open { tar: true, .. } | Cmd::Receive { .. });
    let out = Out { json: cli.json, to_stderr: stdout_is_data };
    let home = Home::open(home_dir(&cli)?, &key)?;
    match cli.command {
        Cmd::Key(KeyCmd::Init) => {
            let (made, created) = MachineKey::create(&key)?;
            let line = made.public().line();
            let text = if created { format!("Made this machine's key:\n{line}") } else { line.clone() };
            out.show(json!({ "key": line, "created": created }), &text)
        }
        Cmd::Key(KeyCmd::Show) => {
            let line = home.public_key().map(HolderKey::line);
            let text = line.clone().unwrap_or_else(|| "This machine has no key yet. Run `eidolon key init` to make it.".to_owned());
            out.show(json!({ "key": line, "seat_namespace": eidolon::SEAT_NAMESPACE, "ack_namespace": eidolon::ACK_NAMESPACE }), &text)
        }
        Cmd::Holders(HoldersCmd::Add { name, key }) => {
            let added = home.add_holder(&name, key.join(" ").parse()?)?;
            out.show(json!({ "holder": name, "added": added }), &if added { format!("Added {name}.") } else { format!("{name} was already listed.") })
        }
        Cmd::Holders(HoldersCmd::Ls) => {
            let holders = home.holders()?;
            let me = home.public_key();
            let rows: Vec<Value> = holders.iter().map(|h| json!({ "name": h.name, "key": h.key, "this_machine": Some(&h.key) == me })).collect();
            let text = holders.iter().map(|h| format!("{}{} {}\n", h.name, if Some(&h.key) == me { " (this machine)" } else { "" }, h.key)).collect::<Vec<_>>().concat();
            out.show(json!({ "holders": rows }), if text.is_empty() { "No holders yet. Add each machine, this one too, with `eidolon holders add`." } else { &text })
        }
        Cmd::Target(TargetCmd::Add { name, preset, file, path }) => {
            let target = match (preset, file) {
                (Some(p), _) => Some(Target::preset(&p).ok_or_else(|| {
                    Error::refused(Code::UnknownTarget, format!("There is no preset {p:?}."), format!("Presets: {}.", Target::presets().collect::<Vec<_>>().join(", ")))
                })?),
                (None, Some(f)) => Some(Target::parse(&std::fs::read_to_string(&f).map_err(|e| Error::Io { doing: format!("read {}", f.display()), source: e })?)?),
                (None, None) => None,
            };
            let status = home.add_target(&name, target, path)?;
            let text = format!("{name} is at {}.", status.path.as_ref().map_or_else(|| "no path on this machine yet".to_owned(), |p| p.display().to_string()));
            out.show(json!(status), &text)
        }
        Cmd::Target(TargetCmd::Ls) => {
            let rows = home.ls()?;
            let text = rows.iter().map(|s| format!("{}\t{}\n", s.target, s.path.as_ref().map_or_else(|| "-".to_owned(), |p| p.display().to_string()))).collect::<Vec<_>>().concat();
            out.show(json!({ "targets": rows }), &text)
        }
        Cmd::Target(TargetCmd::Show { name }) => match home.target(&name) {
            Ok(target) => show_target(&out, &name, target, Some(home.status(&name)?)),
            Err(e) => match Target::preset(&name) {
                Some(preset) => show_target(&out, &name, preset, None),
                None => Err(e),
            },
        },
        Cmd::Seal { target, from, from_tar, recipients, limits } => {
            let extra = recipients.iter().map(|r| Recipient::parse(r)).collect::<eidolon::Result<Vec<_>>>()?;
            let mut stdin = io::stdin().lock();
            let source = match from_tar.as_deref() {
                Some("-") => Source::Tar(&mut stdin),
                Some(other) => return Err(Error::refused(Code::NoPath, format!("--from-tar takes `-`, not {other:?}."), "Pipe the tar in on standard input.")),
                None => Source::Folder(from),
            };
            let sealed = home.seal(&target, source, extra, limits.limits())?;
            let c = sealed.counts;
            let text = format!("Sealed {target} generation {}: {} files, {} bytes, bundle {} bytes.", sealed.generation, c.files, c.bytes, sealed.size);
            out.show(json!(sealed), &text)
        }
        Cmd::Open { target, to, tar, limits } => {
            let mut stdout = io::stdout().lock();
            let dest = if tar { Dest::Tar(&mut stdout) } else { Dest::Folder(to) };
            let opened = home.open_target(&target, dest, limits.limits())?;
            drop(stdout);
            let place = opened.to.as_ref().map_or_else(|| "standard output".to_owned(), |p| p.display().to_string());
            out.show(json!(opened), &format!("Opened {target} generation {} to {place}: {} files.", opened.generation, opened.counts.files))
        }
        Cmd::Send { target, to, out: Some(file), clear: _, command: _ } => {
            let f = std::fs::File::create(&file).map_err(|e| Error::Io { doing: format!("create {}", file.display()), source: e })?;
            if let Err(e) = home.offer(&target, &to, io::BufWriter::new(f)) {
                let _ = std::fs::remove_file(&file);
                return Err(e);
            }
            let text = format!("Wrote the offer to {}. On {to}: `eidolon receive < offer > ack`; then here: `eidolon settle {target} --confirm ack`.", file.display());
            out.show(json!({ "target": target, "to": to, "offer": file, "pending": true }), &text)
        }
        Cmd::Send { target, to, out: None, clear, command } => {
            let (program, args) = command.split_first().ok_or_else(|| {
                Error::refused(Code::NoPath, "Send needs a command after `--`, or --out.", format!("Run `eidolon send {target} --to {to} -- ssh {to} eidolon receive`."))
            })?;
            let mut child = None;
            let sent = home.send(&target, &to, clear, || {
                let spawned = child.insert(
                    Command::new(program)
                        .args(args)
                        .stdin(Stdio::piped())
                        .stdout(Stdio::piped())
                        .stderr(Stdio::inherit())
                        .spawn()
                        .map_err(|e| Error::Io { doing: format!("run {}", program.to_string_lossy()), source: e })?,
                );
                match (spawned.stdin.take(), spawned.stdout.take()) {
                    (Some(stdin), Some(stdout)) => Ok((stdin, stdout)),
                    _ => Err(Error::refused(Code::BadAck, "The command's stdin and stdout were not piped.", "Run it again.")),
                }
            });
            if let Some(mut child) = child {
                let _ = child.wait();
            }
            let moved = sent?;
            let text = format!("Moved {target} to {} (generation {}){}.", moved.to, moved.generation, if moved.cleared { "; this machine's copy is removed" } else { "" });
            out.show(json!(moved), &text)
        }
        Cmd::Receive { limits } => {
            let received = home.receive(io::stdin().lock(), io::stdout().lock(), limits.limits())?;
            let text = format!("Took {} generation {} from {}{}.", received.target, received.generation, received.from, if received.again { " (again)" } else { "" });
            out.show(json!(received), &text)
        }
        Cmd::Settle { target, confirm, abandon, clear } => {
            let settled = if abandon {
                home.abandon(&target)?
            } else {
                let path = confirm.unwrap_or_default();
                let mut ack = Vec::new();
                std::fs::File::open(&path).and_then(|mut f| f.read_to_end(&mut ack)).map_err(|e| Error::Io { doing: format!("read {}", path.display()), source: e })?;
                home.settle_with_ack(&target, &ack, clear)?
            };
            let text = match &settled {
                Settled::Moved(m) => format!("Moved {target} to {} (generation {}).", m.to, m.generation),
                Settled::Refused(r) => format!("The receiver refused, and the seat stays here: {r}"),
                Settled::Abandoned => format!("Dropped the pending move of {target}; the seat stays here."),
            };
            out.show(json!(settled), &text)
        }
        Cmd::Ls => {
            let rows = home.ls()?;
            let text = rows
                .iter()
                .map(|s| {
                    let holder = match (&s.holder, s.held_here) {
                        (None, _) => "not sealed".to_owned(),
                        (Some(_), true) => "held here".to_owned(),
                        (Some(h), false) => format!("held by {h}"),
                    };
                    let pending = s.pending_to.as_ref().map_or_else(String::new, |t| format!(", move to {t} pending"));
                    format!("{}\tgeneration {}\t{holder}{pending}\n", s.target, s.generation.unwrap_or(0))
                })
                .collect::<Vec<_>>()
                .concat();
            out.show(
                json!({ "targets": rows, "key": home.public_key().map(HolderKey::line) }),
                if text.is_empty() { "No targets yet. Add one with `eidolon target add firefox --path <profile>`." } else { &text },
            )
        }
        Cmd::Verify { target, limits } => {
            let v = home.verify(&target, limits.limits())?;
            let opened = v.opened.map_or_else(|| "the bundle is on the holder".to_owned(), |c| format!("the bundle opens: {} files, {} bytes", c.files, c.bytes));
            out.show(json!(v), &format!("{target}: {} seats check out, {} holds generation {}, {opened}.", v.seats_checked, v.holder, v.generation))
        }
    }
}

fn show_target(out: &Out, name: &str, target: Target, status: Option<eidolon::Status>) -> eidolon::Result<()> {
    let toml = target.to_toml();
    let mut text = format!("# {name}\n{toml}");
    if let Some(s) = &status {
        let path = s.path.as_ref().map_or_else(|| "none".to_owned(), |p| p.display().to_string());
        text = format!("{text}# path here: {path}\n");
    }
    out.show(json!({ "target": name, "rules": target, "toml": toml, "status": status }), &text)
}
