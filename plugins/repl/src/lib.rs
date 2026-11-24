//! `fgds repl`: a prompt on a running GDS. Events, channel samples and the commands of other
//! clients are printed above the prompt while a line is typed. Everything goes through the
//! public HTTP and WebSocket API: a command is checked, encoded and recorded by the GDS, as
//! with `fgds command-send`. An extra of fgds; the Python GDS has no counterpart.

mod api;
mod complete;
mod filter;
mod line;
mod show;
mod tui;

use api::Api;
use base64::Engine;
use complete::Names;
use fgds_core::clap::{Arg, ArgMatches, Command};
use fgds_core::serde_json::{json, Value};
use fgds_core::{CliEnv, Error, Manifest, Plugin, Registry, Result, Subcommand};
use filter::Filter;
use line::Line;
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{ColorMode, CompletionType, Config, Editor};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ReplPlugin;

impl Plugin for ReplPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "repl", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(Repl))?;
        reg.add_subcommand(Arc::new(tui::Tui))
    }
}

const KINDS: &str = "fprime.event,fprime.channel,fprime.command";

/// How long the events that name the opcode of a command sent here are printed whatever the filters.
const RESULT: Duration = Duration::from_secs(10);

/// What the prompt and the stream share.
#[derive(Default)]
struct View {
    filter: Filter,
    /// Opcodes of the commands sent from this prompt, with the time of sending.
    sent: Vec<(u64, Instant)>,
}

impl View {
    /// Whether `record` is an event about a command this prompt has just sent.
    fn result_of_ours(&mut self, record: &Value) -> bool {
        self.sent.retain(|(_, at)| at.elapsed() < RESULT);
        record["body"]["severity"] == "COMMAND"
            && record["body"]["args"].as_object().is_some_and(|args| {
                args.values().any(|v| self.sent.iter().any(|(opcode, _)| v.as_u64() == Some(*opcode)))
            })
    }

    /// The line of `record`, as a terminal may be given it; `None` when the filters leave it out.
    fn line(&mut self, record: &Value, own: &str, colour: bool) -> Option<String> {
        let name = record["body"]["name"].as_str().unwrap_or("?");
        match record["kind"].as_str()? {
            "fprime.event" => {
                (self.result_of_ours(record) || self.filter.allows(true, name)).then(|| show::event(record, colour))
            }
            "fprime.channel" => self.filter.allows(false, name).then(|| show::visible(&show::channel(record))),
            // The prompt prints the outcome of its own commands itself.
            "fprime.command" => (record["body"]["client"] != own).then(|| show::visible(&show::command(record))),
            _ => None,
        }
    }
}

type Shared = Arc<Mutex<View>>;

fn view(shared: &Shared) -> std::sync::MutexGuard<'_, View> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the stream gives: a record, a line about the connection, or the dictionary of the GDS
/// when it is another one than the last read.
enum Item<'a> {
    Record(&'a Value),
    Note(String),
    Dictionary(Value),
}

/// The waits between two attempts to open the stream: the first, and the longest.
const WAITS: (Duration, Duration) = (Duration::from_secs(1), Duration::from_secs(30));
/// A stream open for this long was a working one: the next wait is the first again.
const STEADY: Duration = Duration::from_secs(10);

/// The kinds of `KINDS` the GDS records, and those it does not (a GDS built without their plugin).
async fn kinds(api: &Api) -> (Vec<&'static str>, Vec<&'static str>) {
    let all = || KINDS.split(',');
    match api.get("/kinds", &[]).await {
        Ok(Value::Array(known)) => all().partition(|kind| known.iter().any(|k| k["id"] == *kind)),
        // No answer: the stream says what is wrong.
        _ => (all().collect(), Vec::new()),
    }
}

/// Gives the stream to `on` until the process ends, connecting again when it stops. One note
/// when the stream is lost and one when it is open again; the attempts in between say nothing
/// and wait longer each time. `dictionary` is the dictionary version of `/version` last read.
async fn stream(api: Api, mut dictionary: Value, mut on: impl FnMut(Item)) {
    let (mut after, mut wait, mut lost) = (None, WAITS.0, false);
    let mut without = Vec::new();
    loop {
        // A GDS started again may have another dictionary.
        if let Ok(version) = api.check_version().await {
            if version["dictionary"] != dictionary {
                if let Ok(read) = api.get("/dictionary", &[]).await {
                    dictionary = version["dictionary"].clone();
                    on(Item::Dictionary(read));
                }
            }
        }
        let (kinds, missing) = kinds(&api).await;
        if missing != without {
            if !missing.is_empty() {
                on(Item::Note(format!("the GDS has no records of kind {}: none are shown", missing.join(", "))));
            }
            without = missing;
        }
        let mut opened = None;
        let ended = if kinds.is_empty() {
            Err(Error::Other("nothing to follow".to_string()))
        } else {
            let on = &mut |record: Option<&Value>| match record {
                Some(record) => on(Item::Record(record)),
                None => {
                    opened = Some(Instant::now());
                    if std::mem::take(&mut lost) {
                        on(Item::Note("the GDS answers again".to_string()));
                    }
                }
            };
            api.follow(&kinds.join(","), &mut after, on).await
        };
        let Err(error) = ended else { continue };
        if !lost && !kinds.is_empty() {
            on(Item::Note(format!("{error}; trying again until the GDS answers")));
            lost = true;
        }
        // What the GDS recorded meanwhile is not printed: it may be another run.
        after = None;
        if opened.is_some_and(|at: Instant| at.elapsed() >= STEADY) {
            wait = WAITS.0;
        }
        tokio::time::sleep(wait).await;
        wait = (wait * 2).min(WAITS.1);
    }
}

/// The stream as lines, through the filters, for `print`.
async fn stream_lines(api: Api, session: Follower, colour: bool, mut print: impl FnMut(String)) {
    let own = api.client().to_string();
    let on = |item: Item| match item {
        Item::Record(record) => view(&session.shared).line(record, &own, colour).into_iter().for_each(&mut print),
        Item::Note(note) => print(show::visible(&note)),
        Item::Dictionary(read) => print(reload(&session.dictionary, read)),
    };
    stream(api, session.version, on).await;
}

/// The dictionary of the GDS as last read, with the names Tab completes.
struct Loaded {
    value: Value,
    names: Names,
}

type Dictionary = Arc<Mutex<Arc<Loaded>>>;

fn loaded(dictionary: &Dictionary) -> Arc<Loaded> {
    dictionary.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone()
}

fn load(value: Value) -> Arc<Loaded> {
    Arc::new(Loaded { names: Names::of(&value), value })
}

/// Takes the dictionary the stream has read from a GDS that has another one; the line to print.
fn reload(dictionary: &Dictionary, read: Value) -> String {
    *dictionary.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = load(read);
    "the GDS has another dictionary: names, completion and usage are from it now".to_string()
}

/// What the thread of the stream takes from a session.
struct Follower {
    shared: Shared,
    dictionary: Dictionary,
    version: Value,
}

/// On a signal that ends the process (SIGTERM, SIGHUP, SIGQUIT, SIGINT), calls `restore` to put
/// the terminal back, then ends with 128 + the signal number. SIGKILL cannot be answered.
fn restore_on_signal(restore: impl FnOnce() + Send + 'static) {
    use tokio::signal::unix::{signal, SignalKind};
    let (ready, waits) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(async {
            let (got, mut signals) = tokio::sync::mpsc::channel(1);
            let kinds = [SignalKind::terminate(), SignalKind::hangup(), SignalKind::quit(), SignalKind::interrupt()];
            for kind in kinds {
                let (Ok(mut arrives), got) = (signal(kind), got.clone()) else { continue };
                tokio::spawn(async move {
                    arrives.recv().await;
                    let _ = got.send(kind.as_raw_value()).await;
                });
            }
            let _ = ready.send(());
            if let Some(number) = signals.recv().await {
                restore();
                std::process::exit(128 + number);
            }
        });
    });
    // The handlers are in place before the terminal is changed.
    let _ = waits.recv();
}

const PROMPT: &str = "fgds> ";

/// What the terminal shows on its last row, for the stream to print above it.
#[derive(Default)]
struct Screen {
    state: Row,
    line: String,
    /// The cursor, as a byte of `line`.
    pos: usize,
    /// What the stream had to print while the row was `Row::Entered`.
    held: Vec<String>,
}

#[derive(Default, PartialEq)]
enum Row {
    /// Nothing is being typed: the cursor is at the start of an empty row.
    #[default]
    Free,
    /// The row holds the prompt and `line`.
    Typed,
    /// Enter was pressed and the editor is leaving the row.
    Entered,
}

type Shown = Arc<Mutex<Screen>>;

fn screen(shown: &Shown) -> std::sync::MutexGuard<'_, Screen> {
    shown.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Prints `text` above the line being typed and draws that line again under it.
///
/// rustyline has an `ExternalPrinter` for this, but with one in use it leaves keys that arrive
/// together (pasted text) unread until the next key (versions 14 to 19). So the editor tells
/// through its helper what the row holds, and the row is drawn again here. A typed line longer
/// than the terminal is wide leaves its first rows above `text`.
fn print_above(shown: &Shown, text: &str) {
    let mut screen = screen(shown);
    let mut out = std::io::stdout().lock();
    let _ = match screen.state {
        Row::Free => writeln!(out, "{text}"),
        Row::Typed => {
            let back = unicode_width::UnicodeWidthStr::width(&screen.line[screen.pos..]);
            let back = if back > 0 { format!("\x1b[{back}D") } else { String::new() };
            write!(out, "\r\x1b[J{text}\r\n{PROMPT}{}{back}", screen.line)
        }
        Row::Entered => {
            screen.held.push(text.to_string());
            Ok(())
        }
    };
    let _ = out.flush();
}

struct Helper {
    dictionary: Dictionary,
    shown: Shown,
}

impl Helper {
    fn note(&self, line: &str, pos: usize) {
        let mut screen = screen(&self.shown);
        screen.line.clear();
        screen.line.push_str(line);
        screen.pos = pos;
    }
}

impl rustyline::Helper for Helper {}
impl rustyline::validate::Validator for Helper {
    /// Asked when Enter is pressed.
    fn validate(
        &self,
        _: &mut rustyline::validate::ValidationContext,
    ) -> rustyline::Result<rustyline::validate::ValidationResult> {
        screen(&self.shown).state = Row::Entered;
        Ok(rustyline::validate::ValidationResult::Valid(None))
    }
}
// The editor asks for a hint after every change of the line, and whether to highlight after
// every move of the cursor: the two places where it tells what the row holds.
impl rustyline::hint::Hinter for Helper {
    type Hint = String;

    fn hint(&self, line: &str, pos: usize, _: &rustyline::Context<'_>) -> Option<String> {
        self.note(line, pos);
        None
    }
}
impl rustyline::highlight::Highlighter for Helper {
    fn highlight_char(&self, line: &str, pos: usize, _: rustyline::highlight::CmdKind) -> bool {
        self.note(line, pos);
        false
    }
}
impl rustyline::completion::Completer for Helper {
    type Candidate = String;

    fn complete(&self, line: &str, pos: usize, _: &rustyline::Context<'_>) -> rustyline::Result<(usize, Vec<String>)> {
        Ok(loaded(&self.dictionary).names.complete(line, pos))
    }
}

struct Session {
    api: Api,
    dictionary: Dictionary,
    /// The dictionary version of `/version` when `dictionary` was read.
    version: Value,
    shared: Shared,
    runtime: tokio::runtime::Runtime,
}

fn url_arg() -> Arg {
    Arg::new("url").long("url").value_name("URL").help("Address of the running GDS [default: the configured --bind]")
}

impl Session {
    /// Reads the dictionary of the GDS at `--url`; `name` is the subcommand, for the client name.
    fn open(name: &str, args: &ArgMatches, env: &CliEnv) -> Result<Session> {
        let base = match args.get_one::<String>("url") {
            Some(url) => api::base_url(url),
            None => (env.client)()?.base_url,
        };
        let api = Api::new(base, format!("fgds-{name}-{}", std::process::id()));
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)?;
        let (version, dictionary) = runtime.block_on(async {
            let version = api.check_version().await?;
            Ok::<_, Error>((version["dictionary"].clone(), api.get("/dictionary", &[]).await?))
        })?;
        let dictionary = Arc::new(Mutex::new(load(dictionary)));
        Ok(Session { api, dictionary, version, shared: Shared::default(), runtime })
    }

    /// For the thread of the stream: a client (a connection is not shared between two runtimes)
    /// and what the stream reads and changes.
    fn follower(&self) -> (Api, Follower) {
        let api = Api::new(self.api.base().to_string(), self.api.client().to_string());
        let (shared, dictionary, version) = (self.shared.clone(), self.dictionary.clone(), self.version.clone());
        (api, Follower { shared, dictionary, version })
    }

    /// Does what `line` asks and returns what to print, as a terminal may be given it; `None` to leave.
    fn handle(&self, line: &str) -> Option<Vec<String>> {
        Some(self.answer(line)?.iter().map(|line| show::visible(line)).collect())
    }

    fn answer(&self, line: &str) -> Option<Vec<String>> {
        let dictionary = &loaded(&self.dictionary).value;
        let parsed = match line::parse(line) {
            Ok(parsed) => parsed,
            Err(error) => return Some(vec![error]),
        };
        let changed = |changed: bool, done: String| vec![if changed { done } else { "nothing to change".to_string() }];
        Some(match parsed {
            Line::Empty => Vec::new(),
            Line::Quit => return None,
            Line::Help(None) => show::HELP.lines().map(str::to_string).collect(),
            Line::Help(Some(name)) => show::help(dictionary, &name),
            Line::Dict(which, pattern) => {
                let mut lines = show::definitions(dictionary, which, pattern.as_deref());
                if lines.is_empty() {
                    lines.push("none".to_string());
                }
                lines
            }
            Line::Mute(on, target) => changed(
                view(&self.shared).filter.mute(on, &target),
                format!("{}: {target}", if on { "muted" } else { "unmuted" }),
            ),
            Line::Subscribe(on, pattern) => changed(
                view(&self.shared).filter.subscribe(on, &pattern),
                format!("{}: {pattern}", if on { "subscribed" } else { "unsubscribed" }),
            ),
            Line::Filters => view(&self.shared).filter.lines(),
            Line::ClearFilters => {
                view(&self.shared).filter = Filter::default();
                vec!["every event and channel sample is printed".to_string()]
            }
            Line::Cmd { name, args } => self.command(&name, &args),
            Line::RawUplink(bytes) => {
                let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                match self.runtime.block_on(self.api.post("/raw", &json!({"data": data}))) {
                    Ok(_) => vec![format!("sent {} bytes as one packet: {}", bytes.len(), show::hex(&bytes))],
                    Err(error) => vec![unsent(&error)],
                }
            }
        })
    }

    /// Sends a command through the GDS, which checks and encodes it; prints its record. A name
    /// the dictionary read here does not have is sent as well: the GDS is the judge.
    fn command(&self, name: &str, args: &[String]) -> Vec<String> {
        let dictionary = &loaded(&self.dictionary).value;
        let known = show::command_parameters(dictionary, name);
        let request = json!({"name": name, "args": line::arguments(args, known.as_deref().unwrap_or(&[]))});
        self.runtime.block_on(async {
            let sent = match self.api.post("/commands", &request).await {
                Ok(sent) => sent,
                Err(Error::Invalid(reason)) => {
                    let mut lines = vec![format!("not sent: {reason}")];
                    lines.extend(known.map(|_| show::usage(dictionary, name)));
                    return lines;
                }
                Err(error) => return vec![unsent(&error)],
            };
            let Some(seq) = api::seq(&sent["seq"]) else { return vec!["sent".to_string()] };
            let after = seq.saturating_sub(1).to_string();
            let query = [("kind", "fprime.command"), ("after_seq", after.as_str()), ("limit", "1")];
            match self.api.get("/records", &query).await {
                Ok(page) => {
                    let record = &page["records"][0];
                    if let Some(opcode) = record["body"]["opcode"].as_u64() {
                        view(&self.shared).sent.push((opcode, Instant::now()));
                    }
                    vec![show::command(record)]
                }
                Err(error) => vec![format!("sent; its record could not be read: {error}")],
            }
        })
    }
}

/// The line for a request the GDS did not answer: only one that found no GDS is known not to be sent.
fn unsent(error: &Error) -> String {
    match error {
        Error::Io(_) | Error::Invalid(_) => format!("not sent: {error}"),
        _ => format!("no answer, it may have been sent: {error}"),
    }
}

fn other(error: impl std::fmt::Display) -> Error {
    Error::Other(error.to_string())
}

/// The prompt, on a terminal. The stream runs on a thread of its own and prints above the line.
fn prompt(session: Session, colour: bool) -> Result<ExitCode> {
    // The colour mode is forced so that the editor asks its highlighter (see `Helper`).
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .auto_add_history(true)
        .color_mode(ColorMode::Forced)
        .build();
    let mut editor: Editor<Helper, DefaultHistory> = Editor::with_config(config).map_err(other)?;
    let shown = Shown::default();
    editor.set_helper(Some(Helper { dictionary: session.dictionary.clone(), shown: shown.clone() }));
    let history = std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".fgds_history"));
    if let Some(history) = &history {
        let _ = editor.load_history(history);
    }
    println!("fgds repl on {}: `help` lists what can be typed, `quit` leaves.", show::visible(session.api.base()));
    // Ctrl-C is a key of the editor here, not a signal: SIGINT comes from another process.
    if let Ok(modes) = nix::sys::termios::tcgetattr(std::io::stdin()) {
        restore_on_signal(move || {
            let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &modes);
            let _ = std::io::stdout().write_all(b"\r\n");
        });
    }
    let (api, follower) = session.follower();
    let above = shown.clone();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(stream_lines(api, follower, colour, |line| print_above(&above, &line)));
    });
    loop {
        *screen(&shown) = Screen { state: Row::Typed, ..Default::default() };
        let read = editor.readline(PROMPT);
        let held = std::mem::take(&mut *screen(&shown)).held;
        held.iter().for_each(|line| print_above(&shown, line));
        match read {
            Ok(line) => {
                // After each line: a prompt ended by a signal keeps its history.
                if let Some(history) = &history {
                    let _ = editor.append_history(history);
                }
                match session.handle(&line) {
                    Some(lines) => lines.iter().for_each(|line| print_above(&shown, line)),
                    None => break,
                }
            }
            // Ctrl-C drops the line being typed.
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(other(error)),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `fgds repl`.
struct Repl;

impl Subcommand for Repl {
    fn command(&self) -> Command {
        Command::new("repl")
            .about("Interactive prompt on a running GDS: send commands, watch events and telemetry")
            .after_help(
                "Type `help` at the prompt. When the standard input is not a terminal, nothing is read: \
                 events, channel samples and commands are printed until interrupted.",
            )
            .arg(url_arg())
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let session = Session::open("repl", args, env)?;
        let colour = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        if std::io::stdin().is_terminal() {
            return prompt(session, colour);
        }
        let (api, follower) = session.follower();
        let print = |line: String| {
            let mut out = std::io::stdout().lock();
            // A closed pipe (`fgds repl | head`) ends the command, it is not an error.
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                std::process::exit(0);
            }
        };
        session.runtime.block_on(async {
            tokio::select! {
                () = stream_lines(api, follower, colour, print) => {}
                () = fgds_core::interrupt() => {}
            }
        });
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn event(name: &str, severity: &str, args: Value) -> Value {
        json!({"kind": "fprime.event", "flight_time": {"base": 2, "seconds": 1},
            "body": {"name": name, "severity": severity, "args": args, "display_text": "text"}})
    }

    #[test]
    fn only_a_request_that_found_no_gds_is_called_not_sent() {
        let refused = Error::Io(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "no GDS answers"));
        assert!(unsent(&refused).starts_with("not sent: "));
        assert!(unsent(&Error::Invalid("unknown command X".into())).starts_with("not sent: "));
        let late = unsent(&Error::Other("the GDS did not answer: timed out".into()));
        assert!(late.starts_with("no answer, it may have been sent: "), "{late}");
    }

    #[test]
    fn the_stream_is_filtered() {
        let mut view = View::default();
        let channel = json!({"kind": "fprime.channel", "body": {"name": "A.b.Count", "display_text": "1"}});
        assert!(view.line(&channel, "me", false).is_some());
        assert!(view.line(&event("A.b.Done", "ACTIVITY_HI", json!({})), "me", false).is_some());
        view.filter.mute(true, "channels");
        view.filter.mute(true, "A.b.D*");
        assert!(view.line(&channel, "me", false).is_none());
        assert!(view.line(&event("A.b.Done", "ACTIVITY_HI", json!({})), "me", false).is_none());
        assert!(view.line(&json!({"kind": "gds.status", "body": {}}), "me", false).is_none());
    }

    #[test]
    fn the_events_of_a_command_sent_here_pass_the_filters() {
        let mut view = View::default();
        view.filter.mute(true, "events");
        let completed = event("A.disp.OpCodeCompleted", "COMMAND", json!({"Opcode": 16}));
        assert!(view.line(&completed, "me", false).is_none());
        view.sent.push((16, Instant::now()));
        assert!(view.line(&completed, "me", false).is_some());
        assert!(view.line(&event("A.disp.OpCodeCompleted", "COMMAND", json!({"Opcode": 17})), "me", false).is_none());
        assert!(view.line(&event("A.b.Done", "ACTIVITY_HI", json!({"count": 16})), "me", false).is_none());
        view.sent[0].1 = Instant::now().checked_sub(RESULT * 2).unwrap();
        assert!(view.line(&completed, "me", false).is_none());
    }

    #[test]
    fn commands_of_other_clients_are_printed() {
        let mut view = View::default();
        let record = |client: &str| {
            json!({"kind": "fprime.command", "ground_time": "T", "raw": "",
            "body": {"name": "A.b.NO_OP", "opcode": 1, "args": {}, "client": client, "outcome": "sent"}})
        };
        assert!(view.line(&record("gui"), "me", false).is_some());
        assert!(view.line(&record("me"), "me", false).is_none());
    }
}
