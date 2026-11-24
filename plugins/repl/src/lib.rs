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

    /// The line to print for a record of the stream; `None` when it is filtered out.
    fn line(&mut self, record: &Value, own: &str, colour: bool) -> Option<String> {
        let name = record["body"]["name"].as_str().unwrap_or("?");
        match record["kind"].as_str()? {
            "fprime.event" => {
                (self.result_of_ours(record) || self.filter.allows(true, name)).then(|| show::event(record, colour))
            }
            "fprime.channel" => self.filter.allows(false, name).then(|| show::channel(record)),
            // The prompt prints the outcome of its own commands itself.
            "fprime.command" => (record["body"]["client"] != own).then(|| show::command(record)),
            _ => None,
        }
    }
}

type Shared = Arc<Mutex<View>>;

fn view(shared: &Shared) -> std::sync::MutexGuard<'_, View> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// What the stream gives: a record, or a line about the connection.
enum Item<'a> {
    Record(&'a Value),
    Note(String),
}

/// Gives the stream to `on` until the process ends, connecting again when it stops.
async fn stream(api: Api, mut on: impl FnMut(Item)) {
    let (mut after, mut down) = (None, false);
    loop {
        if down {
            tokio::time::sleep(Duration::from_secs(1)).await;
            if api.check_version().await.is_err() {
                continue;
            }
            on(Item::Note("the GDS answers again".to_string()));
            down = false;
        }
        if let Err(error) = api.follow(KINDS, &mut after, &mut |record| on(Item::Record(record))).await {
            on(Item::Note(format!("{error}; trying again every second")));
            // What the GDS recorded meanwhile is not printed: it may be another run.
            (after, down) = (None, true);
        }
    }
}

/// The stream as lines, through the filters, for `print`.
async fn stream_lines(api: Api, shared: Shared, colour: bool, mut print: impl FnMut(String)) {
    let own = api.client().to_string();
    let on = |item: Item| match item {
        Item::Record(record) => view(&shared).line(record, &own, colour).into_iter().for_each(&mut print),
        Item::Note(note) => print(note),
    };
    stream(api, on).await;
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
            let back = screen.line[screen.pos..].chars().count();
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
    names: Names,
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
        Ok(self.names.complete(line, pos))
    }
}

struct Session {
    api: Api,
    dictionary: Value,
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
        let dictionary = runtime.block_on(async {
            api.check_version().await?;
            api.get("/dictionary", &[]).await
        })?;
        Ok(Session { api, dictionary, shared: Shared::default(), runtime })
    }

    /// A client for the thread of the stream: a connection is not shared between two runtimes.
    fn stream_api(&self) -> Api {
        Api::new(self.api.base().to_string(), self.api.client().to_string())
    }

    /// Does what `line` asks and returns what to print; `None` to leave.
    fn handle(&self, line: &str) -> Option<Vec<String>> {
        let parsed = match line::parse(line) {
            Ok(parsed) => parsed,
            Err(error) => return Some(vec![error]),
        };
        let changed = |changed: bool, done: String| vec![if changed { done } else { "nothing to change".to_string() }];
        Some(match parsed {
            Line::Empty => Vec::new(),
            Line::Quit => return None,
            Line::Help(None) => show::HELP.lines().map(str::to_string).collect(),
            Line::Help(Some(name)) => show::help(&self.dictionary, &name),
            Line::Dict(which, pattern) => {
                let mut lines = show::definitions(&self.dictionary, which, pattern.as_deref());
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
                    Err(error) => vec![format!("not sent: {error}")],
                }
            }
        })
    }

    /// Sends a command through the GDS, which checks and encodes it; prints its record.
    fn command(&self, name: &str, args: &[String]) -> Vec<String> {
        let Some(parameters) = show::command_parameters(&self.dictionary, name) else {
            return vec![format!("not sent: {name} is not a command of the dictionary (try `cmds {name}`)")];
        };
        let request = json!({"name": name, "args": line::arguments(args, &parameters)});
        self.runtime.block_on(async {
            let sent = match self.api.post("/commands", &request).await {
                Ok(sent) => sent,
                Err(Error::Invalid(reason)) => {
                    return vec![format!("not sent: {reason}"), show::usage(&self.dictionary, name)];
                }
                Err(error) => return vec![format!("not sent: {error}")],
            };
            let Some(seq) = api::seq(&sent["seq"]) else { return vec!["sent".to_string()] };
            let after = (seq - 1).to_string();
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
    editor.set_helper(Some(Helper { names: Names::of(&session.dictionary), shown: shown.clone() }));
    let history = std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".fgds_history"));
    if let Some(history) = &history {
        let _ = editor.load_history(history);
    }
    println!("fgds repl on {}: `help` lists what can be typed, `quit` leaves.", session.api.base());
    let api = session.stream_api();
    let (shared, above) = (session.shared.clone(), shown.clone());
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(stream_lines(api, shared, colour, |line| print_above(&above, &line)));
    });
    loop {
        *screen(&shown) = Screen { state: Row::Typed, ..Default::default() };
        let read = editor.readline(PROMPT);
        let held = std::mem::take(&mut *screen(&shown)).held;
        held.iter().for_each(|line| print_above(&shown, line));
        match read {
            Ok(line) => match session.handle(&line) {
                Some(lines) => lines.iter().for_each(|line| print_above(&shown, line)),
                None => break,
            },
            // Ctrl-C drops the line being typed.
            Err(ReadlineError::Interrupted) => continue,
            Err(ReadlineError::Eof) => break,
            Err(error) => return Err(other(error)),
        }
    }
    if let Some(history) = &history {
        let _ = editor.save_history(history);
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
        let Session { api, dictionary, shared, runtime } = Session::open("repl", args, env)?;
        let colour = std::io::stdout().is_terminal() && std::env::var_os("NO_COLOR").is_none();
        if std::io::stdin().is_terminal() {
            return prompt(Session { api, dictionary, shared, runtime }, colour);
        }
        let print = |line: String| {
            let mut out = std::io::stdout().lock();
            // A closed pipe (`fgds repl | head`) ends the command, it is not an error.
            if writeln!(out, "{line}").and_then(|()| out.flush()).is_err() {
                std::process::exit(0);
            }
        };
        runtime.block_on(async {
            tokio::select! {
                () = stream_lines(api, shared, colour, print) => {}
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
