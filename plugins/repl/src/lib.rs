//! `fgds repl`: a prompt on a running GDS. Events, channel samples and the commands of other
//! clients are printed above the prompt while a line is typed. Everything goes through the
//! public HTTP and WebSocket API: a command is checked, encoded and recorded by the GDS, as
//! with `fgds command-send`. An extra of fgds; the Python GDS has no counterpart.

mod api;
mod complete;
mod filter;
mod line;
mod prompt;
mod show;
mod tui;

use api::Api;
use base64::Engine;
use complete::Names;
use fgds_core::clap::{Arg, ArgMatches};
use fgds_core::serde_json::{json, Value};
use fgds_core::{CliEnv, Error, Manifest, Plugin, Registry, Result};
use filter::Filter;
use line::Line;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub struct ReplPlugin;

impl Plugin for ReplPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "repl", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_subcommand(Arc::new(prompt::Repl))?;
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
/// and wait longer each time. `dictionary` is the dictionary as last read.
async fn stream(api: Api, mut dictionary: Value, mut on: impl FnMut(Item)) {
    let (mut after, mut wait, mut lost) = (None, WAITS.0, false);
    let mut without = Vec::new();
    loop {
        // A GDS started again may have another dictionary under the same version: the content is compared.
        if let Ok(read) = api.get("/dictionary", &[]).await {
            if read != dictionary {
                dictionary.clone_from(&read);
                on(Item::Dictionary(read));
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
}

/// On a signal that ends the process (SIGTERM, SIGHUP, SIGQUIT, SIGINT, SIGALRM, SIGUSR1, SIGUSR2), calls `restore` to put
/// the terminal back, then ends with 128 + the signal number. SIGKILL cannot be answered.
fn restore_on_signal(restore: impl FnOnce() + Send + 'static) {
    use tokio::signal::unix::{signal, SignalKind};
    let (ready, waits) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        runtime.block_on(async {
            let (got, mut signals) = tokio::sync::mpsc::channel(1);
            let kinds = [
                SignalKind::terminate(),
                SignalKind::hangup(),
                SignalKind::quit(),
                SignalKind::interrupt(),
                SignalKind::alarm(),
                SignalKind::user_defined1(),
                SignalKind::user_defined2(),
            ];
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

struct Session {
    api: Api,
    dictionary: Dictionary,
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
        let dictionary = Arc::new(Mutex::new(load(dictionary)));
        Ok(Session { api, dictionary, shared: Shared::default(), runtime })
    }

    /// For the thread of the stream: a client (a connection is not shared between two runtimes)
    /// and what the stream reads and changes.
    fn follower(&self) -> (Api, Follower) {
        let api = Api::new(self.api.base().to_string(), self.api.client().to_string());
        (api, Follower { shared: self.shared.clone(), dictionary: self.dictionary.clone() })
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
            // The record itself: the first after the one before it, or the first of all.
            let after = seq.checked_sub(1).map(|before| before.to_string());
            let mut query = vec![("kind", "fprime.command"), ("limit", "1")];
            query.extend(after.as_deref().map(|after| ("after_seq", after)));
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

/// The line for a request that failed: one that found no GDS is known not to be sent, an error
/// answer of the GDS (`link down`) is its refusal, and of the others nothing is known.
fn unsent(error: &Error) -> String {
    match error {
        Error::Io(_) | Error::Invalid(_) => format!("not sent: {error}"),
        error if api::answered(error) => format!("refused: {error}"),
        _ => format!("no answer, it may have been sent: {error}"),
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
        assert_eq!(
            unsent(&Error::Other("503 Service Unavailable: link down".into())),
            "refused: 503 Service Unavailable: link down"
        );
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
