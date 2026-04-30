//! Interactive REPL for `fprime-gds-rust`.
//!
//! When stdin is a TTY we use rustyline so the user can type commands while
//! events and telemetry stream above the prompt.  When stdin is *not* a TTY
//! we transparently fall back to a streaming-only mode (downlink to stderr,
//! no commanding) — useful for CI runs or `tee`-style logging.
//!
//! The rustyline editor must run on a blocking thread because it owns a
//! synchronous read loop on the terminal.  We bridge it to the tokio runtime
//! through `mpsc` channels.

use std::{
    fmt::Write as _,
    io::Write as _,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use chrono::Local;
use fprime_comm::{Comm, Status};
use fprime_dict::Dictionary;
use fprime_pipeline::{decode_packets, encode_command, parse_arg, Decoded};
use rustyline::completion::{Completer, Pair};
use rustyline::error::ReadlineError;
use rustyline::highlight::Highlighter;
use rustyline::hint::Hinter;
use rustyline::validate::Validator;
use rustyline::{Context, Helper};
use tokio::sync::mpsc;

const TOP_LEVEL: &[&str] = &[
    "help",
    "cmd",
    "cmds",
    "events",
    "channels",
    "dict",
    "mute",
    "unmute",
    "subscribe",
    "unsubscribe",
    "filters",
    "clear-filters",
    "raw-uplink",
    "quit",
    "exit",
];

struct DictHelper {
    cmd_names: Vec<String>,
    event_names: Vec<String>,
    channel_names: Vec<String>,
}

impl DictHelper {
    fn from_dict(dict: &Dictionary) -> Self {
        let mut cmd_names: Vec<String> = dict
            .commands_by_opcode
            .values()
            .map(|c| c.name.clone())
            .collect();
        cmd_names.sort();
        let mut event_names: Vec<String> =
            dict.events_by_id.values().map(|e| e.name.clone()).collect();
        event_names.sort();
        let mut channel_names: Vec<String> = dict
            .channels_by_id
            .values()
            .map(|c| c.name.clone())
            .collect();
        channel_names.sort();
        Self {
            cmd_names,
            event_names,
            channel_names,
        }
    }
}

impl Helper for DictHelper {}
impl Hinter for DictHelper {
    type Hint = String;
}
impl Highlighter for DictHelper {}
impl Validator for DictHelper {}

impl Completer for DictHelper {
    type Candidate = Pair;

    fn complete(
        &self,
        line: &str,
        pos: usize,
        _ctx: &Context<'_>,
    ) -> rustyline::Result<(usize, Vec<Pair>)> {
        let head = &line[..pos];
        // Find the start of the current word
        let word_start = head.rfind(' ').map(|i| i + 1).unwrap_or(0);
        let prefix = &head[word_start..];

        // What kind of token is this position?  Look at the first whitespace
        // token of the line to decide.
        let first_token: &str = head.split_whitespace().next().unwrap_or("");
        let token_idx = head[..word_start].split_whitespace().count();

        let pool: &[String] = if token_idx == 0 {
            // top-level command
            return Ok((
                word_start,
                TOP_LEVEL
                    .iter()
                    .filter(|c| c.starts_with(prefix))
                    .map(|c| Pair {
                        display: (*c).into(),
                        replacement: format!("{c} "),
                    })
                    .collect(),
            ));
        } else if matches!(first_token, "cmd" | "help") && token_idx == 1 {
            &self.cmd_names
        } else if matches!(first_token, "cmds" | "dict") {
            // "dict <kind> <pattern>" — at idx==1 suggest kinds, idx==2 suggest names
            if first_token == "dict" && token_idx == 1 {
                return Ok((
                    word_start,
                    ["commands", "events", "channels"]
                        .iter()
                        .filter(|c| c.starts_with(prefix))
                        .map(|c| Pair {
                            display: (*c).into(),
                            replacement: format!("{c} "),
                        })
                        .collect(),
                ));
            }
            &self.cmd_names
        } else if first_token == "events" {
            &self.event_names
        } else if first_token == "channels" {
            &self.channel_names
        } else if matches!(first_token, "mute" | "unmute" | "subscribe" | "unsubscribe") {
            // Allow muting/subscribing to either events or channels by name.
            // For a single-token suggestion list we merge them; the user can
            // also type "events"/"channels" as special tokens.
            let mut out: Vec<Pair> = ["events", "channels"]
                .iter()
                .filter(|c| c.starts_with(prefix))
                .map(|c| Pair {
                    display: (*c).into(),
                    replacement: (*c).into(),
                })
                .collect();
            for n in self.channel_names.iter().chain(self.event_names.iter()) {
                if n.starts_with(prefix) {
                    out.push(Pair {
                        display: n.clone(),
                        replacement: n.clone(),
                    });
                }
            }
            return Ok((word_start, out));
        } else {
            // No completions for arg positions of `cmd <name> ...`
            return Ok((word_start, vec![]));
        };

        let pairs = pool
            .iter()
            .filter(|n| n.starts_with(prefix))
            .map(|n| Pair {
                display: n.clone(),
                replacement: n.clone(),
            })
            .collect();
        Ok((word_start, pairs))
    }
}

#[derive(Debug, Default, Clone)]
struct Filter {
    /// Mute the entire event stream.
    mute_events: bool,
    /// Mute the entire channel stream.
    mute_channels: bool,
    /// Glob patterns; items whose qualified name matches any pattern are
    /// suppressed (`mute Ref.systemResources.CPU_*`).
    mute_patterns: Vec<String>,
    /// Glob patterns; when non-empty, only items matching at least one pattern
    /// are shown (`subscribe CdhCore.*`).
    subscribe_patterns: Vec<String>,
    /// Decode errors we've already printed once.  Avoids flooding the REPL
    /// when a periodic channel/event has an arg type we can't decode.
    seen_decode_errors: std::collections::HashSet<String>,
}

impl Filter {
    /// Returns true if a downlink record with the given qualified name should
    /// be shown (i.e. not muted and within active subscriptions).
    fn allows(&self, name: &str) -> bool {
        if !self.subscribe_patterns.is_empty()
            && !self.subscribe_patterns.iter().any(|p| glob_match(p, name))
        {
            return false;
        }
        !self.mute_patterns.iter().any(|p| glob_match(p, name))
    }
}

/// Minimal `?`/`*` glob matcher.  `*` matches any (possibly empty) substring,
/// `?` matches exactly one byte.  All other characters match literally.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p = pattern.as_bytes();
    let t = text.as_bytes();
    fn rec(p: &[u8], t: &[u8]) -> bool {
        match (p.first(), t.first()) {
            (None, None) => true,
            (None, _) => false,
            (Some(b'*'), _) => {
                // either consume zero chars and advance pattern, or eat one and stay
                if rec(&p[1..], t) {
                    return true;
                }
                if t.is_empty() {
                    return false;
                }
                rec(p, &t[1..])
            }
            (Some(b'?'), Some(_)) => rec(&p[1..], &t[1..]),
            (Some(a), Some(b)) if a == b => rec(&p[1..], &t[1..]),
            _ => false,
        }
    }
    rec(p, t)
}

/// Abstracts over rustyline's external printer (writes above the prompt) and a
/// plain stderr writer (no prompt at all).
trait LinePrinter: Send {
    fn print(&mut self, line: String);
}

struct StderrPrinter;
impl LinePrinter for StderrPrinter {
    fn print(&mut self, line: String) {
        let mut out = std::io::stderr().lock();
        let _ = writeln!(out, "{line}");
    }
}

struct RustylinePrinter<P: rustyline::ExternalPrinter + Send>(P);
impl<P: rustyline::ExternalPrinter + Send> LinePrinter for RustylinePrinter<P> {
    fn print(&mut self, line: String) {
        let _ = self.0.print(line);
    }
}

pub async fn run(dict: Dictionary, comm: Comm) -> anyhow::Result<()> {
    let Comm {
        mut downlink,
        uplink,
        mut status,
    } = comm;
    let dict = Arc::new(dict);
    let filter = Arc::new(parking_lot_filter::RwLock::new(Filter::default()));

    let (input_tx, mut input_rx) = mpsc::unbounded_channel::<Line>();
    let shutdown = Arc::new(AtomicBool::new(false));

    // Try to start an interactive REPL.  If we can't (no TTY, redirected
    // stdin, etc.) fall back to logging-only mode.
    let printer: Box<dyn LinePrinter> = match start_repl(
        DictHelper::from_dict(&dict),
        input_tx.clone(),
        shutdown.clone(),
    ) {
        Ok(printer) => printer,
        Err(e) => {
            tracing::warn!("interactive REPL disabled: {e}; running in log-only mode");
            // Closing input_tx would race the receiver; we keep it alive so
            // the recv loop below blocks on Ctrl-C / process termination.
            std::mem::forget(input_tx.clone());
            Box::new(StderrPrinter)
        }
    };

    let dict_for_dl = dict.clone();
    let filter_for_dl = filter.clone();
    let mut printer_for_dl = printer;
    let downlink_task = tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(packet) = downlink.recv() => {
                    handle_downlink(&packet, &dict_for_dl, &filter_for_dl, printer_for_dl.as_mut());
                }
                Some(s) = status.recv() => {
                    let line = match s {
                        Status::Listening(addr) => format!("[comm] listening on {addr}"),
                        Status::Connected(addr) => format!("[comm] connected to {addr}"),
                        Status::Down(reason) => format!("[comm] down: {reason}"),
                    };
                    printer_for_dl.print(line);
                }
                else => break,
            }
        }
    });

    while let Some(line) = input_rx.recv().await {
        match line {
            Line::Quit => {
                shutdown.store(true, Ordering::SeqCst);
                break;
            }
            Line::Cmd(parts) => {
                if let Err(e) = handle_user_command(&parts, &dict, &filter, &uplink).await {
                    eprintln!("error: {e}");
                }
            }
        }
    }

    downlink_task.abort();
    let _ = downlink_task.await;
    Ok(())
}

fn start_repl(
    helper: DictHelper,
    input_tx: mpsc::UnboundedSender<Line>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<Box<dyn LinePrinter>> {
    let mut editor: rustyline::Editor<DictHelper, rustyline::history::DefaultHistory> =
        rustyline::Editor::new()?;
    editor.set_helper(Some(helper));
    let printer = editor.create_external_printer()?;
    let history_path = history_path();
    if let Some(p) = &history_path {
        let _ = editor.load_history(p);
    }

    std::thread::spawn(move || {
        repl_loop(editor, input_tx, shutdown, history_path);
    });

    Ok(Box::new(RustylinePrinter(printer)))
}

#[derive(Debug)]
enum Line {
    Cmd(Vec<String>),
    Quit,
}

fn repl_loop(
    mut editor: rustyline::Editor<DictHelper, rustyline::history::DefaultHistory>,
    input_tx: mpsc::UnboundedSender<Line>,
    shutdown: Arc<AtomicBool>,
    history_path: Option<std::path::PathBuf>,
) {
    let mut stdout = std::io::stdout();
    let _ = writeln!(
        stdout,
        "fprime-gds-rust — type 'help' for commands, 'quit' to exit."
    );
    while !shutdown.load(Ordering::SeqCst) {
        let line = editor.readline("fprime> ");
        match line {
            Ok(line) => {
                let trimmed = line.trim();
                if trimmed.is_empty() {
                    continue;
                }
                let _ = editor.add_history_entry(trimmed);
                let parts = match shlex::split(trimmed) {
                    Some(parts) => parts,
                    None => {
                        eprintln!("error: unable to parse line (mismatched quotes?)");
                        continue;
                    }
                };
                if parts.is_empty() {
                    continue;
                }
                if matches!(parts[0].as_str(), "quit" | "exit") {
                    let _ = input_tx.send(Line::Quit);
                    break;
                }
                if input_tx.send(Line::Cmd(parts)).is_err() {
                    break;
                }
            }
            Err(ReadlineError::Interrupted) => {
                println!("(interrupt — type 'quit' to exit)");
                continue;
            }
            Err(ReadlineError::Eof) => {
                let _ = input_tx.send(Line::Quit);
                break;
            }
            Err(e) => {
                eprintln!("readline error: {e}");
                let _ = input_tx.send(Line::Quit);
                break;
            }
        }
    }
    if let Some(p) = history_path {
        let _ = editor.save_history(&p);
    }
}

fn history_path() -> Option<std::path::PathBuf> {
    let home = std::env::var_os("HOME")?;
    Some(std::path::PathBuf::from(home).join(".fprime-gds-rust-history"))
}

fn handle_downlink(
    packet: &[u8],
    dict: &Dictionary,
    filter: &parking_lot_filter::RwLock<Filter>,
    printer: &mut dyn LinePrinter,
) {
    let now = Local::now().format("%H:%M:%S%.3f").to_string();
    for result in decode_packets(packet, dict) {
        handle_record(&now, result, filter, printer);
    }
}

fn handle_record(
    now: &str,
    result: Result<Decoded, fprime_pipeline::PipelineError>,
    filter: &parking_lot_filter::RwLock<Filter>,
    printer: &mut dyn LinePrinter,
) {
    match result {
        Ok(Decoded::Event(e)) => {
            let f = filter.read();
            if f.mute_events || !f.allows(&e.event.name) {
                return;
            }
            drop(f);
            let mut line = format!(
                "{now}  EVT {sev:<10} {name}",
                sev = e.event.severity,
                name = e.event.name
            );
            if !e.args.is_empty() {
                line.push_str("  args=[");
                for (i, a) in e.args.iter().enumerate() {
                    if i > 0 {
                        line.push_str(", ");
                    }
                    write!(&mut line, "{a}").ok();
                }
                line.push(']');
            }
            printer.print(line);
        }
        Ok(Decoded::Channel(c)) => {
            let f = filter.read();
            if f.mute_channels || !f.allows(&c.channel.name) {
                return;
            }
            drop(f);
            printer.print(format!(
                "{now}  TLM {name} = {value}",
                name = c.channel.name,
                value = c.value
            ));
        }
        Ok(Decoded::Handshake(_)) => {
            // Handshake packets are mostly noise — keep them silent.
        }
        Ok(Decoded::PacketizedTelem(pkt)) => {
            let f = filter.read();
            if f.mute_channels {
                return;
            }
            let allowed: Vec<_> = pkt
                .channels
                .iter()
                .filter(|ch| f.allows(&ch.channel.name))
                .collect();
            drop(f);
            for ch in allowed {
                printer.print(format!(
                    "{now}  TLM {name} = {value}",
                    name = ch.channel.name,
                    value = ch.value
                ));
            }
        }
        Ok(Decoded::Unknown { descriptor, body }) => {
            printer.print(format!(
                "{now}  UNK desc={descriptor:#06x} {} bytes",
                body.len()
            ));
        }
        Err(e) => {
            // Dedup decode errors so a periodic channel/event whose args
            // contain an as-yet-unsupported type doesn't flood the REPL.
            let key = format!("{e}");
            let mut g = filter.write();
            if g.seen_decode_errors.insert(key.clone()) {
                printer.print(format!(
                    "{now}  decode error: {e} (further occurrences suppressed)"
                ));
            }
        }
    }
}

async fn handle_user_command(
    parts: &[String],
    dict: &Dictionary,
    filter: &parking_lot_filter::RwLock<Filter>,
    uplink: &mpsc::UnboundedSender<Vec<u8>>,
) -> anyhow::Result<()> {
    let head = parts[0].as_str();
    match head {
        "help" => help_subcmd(&parts[1..], dict),
        "dict" => dict_subcmd(&parts[1..], dict)?,
        "cmds" => dict_subcmd(
            &[
                String::from("commands"),
                parts.get(1).cloned().unwrap_or_default(),
            ],
            dict,
        )?,
        "events" => dict_subcmd(
            &[
                String::from("events"),
                parts.get(1).cloned().unwrap_or_default(),
            ],
            dict,
        )?,
        "channels" => dict_subcmd(
            &[
                String::from("channels"),
                parts.get(1).cloned().unwrap_or_default(),
            ],
            dict,
        )?,
        "mute" => mute_subcmd(&parts[1..], filter, true)?,
        "unmute" => mute_subcmd(&parts[1..], filter, false)?,
        "subscribe" => subscribe_subcmd(&parts[1..], filter)?,
        "unsubscribe" => unsubscribe_subcmd(&parts[1..], filter)?,
        "filters" => list_filters(filter),
        "clear-filters" => clear_filters(filter),
        "cmd" => {
            let name = parts
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: cmd <qualified.name> [args...]"))?;
            send_command(name, &parts[2..], dict, uplink)?;
        }
        "raw-uplink" => {
            let hex_str = parts
                .get(1)
                .ok_or_else(|| anyhow::anyhow!("usage: raw-uplink <hex-bytes>"))?;
            let bytes = hex::decode(hex_str.replace(' ', ""))?;
            uplink
                .send(bytes)
                .map_err(|_| anyhow::anyhow!("uplink channel closed"))?;
            println!("ok");
        }
        other => anyhow::bail!("unknown command: {other} (try 'help')"),
    }
    Ok(())
}

fn help_subcmd(parts: &[String], dict: &Dictionary) {
    if let Some(target) = parts.first() {
        if let Some(cmd) = dict.command_by_name(target) {
            let params: Vec<String> = cmd
                .params
                .iter()
                .map(|p| {
                    format!(
                        "{}: {}",
                        p.name,
                        p.ty.primitive_name().unwrap_or(&p.ty.name)
                    )
                })
                .collect();
            println!("  {} (opcode {})", cmd.name, cmd.opcode);
            if params.is_empty() {
                println!("  args: (none)");
            } else {
                println!("  args: {}", params.join(", "));
            }
            if let Some(a) = &cmd.annotation {
                println!("  {a}");
            }
            println!(
                "  invoke positional: cmd {} {}",
                cmd.name,
                cmd.params
                    .iter()
                    .map(|p| format!("<{}>", p.name))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
            if !cmd.params.is_empty() {
                println!(
                    "  invoke named:      cmd {} {}",
                    cmd.name,
                    cmd.params
                        .iter()
                        .map(|p| format!("{}=<value>", p.name))
                        .collect::<Vec<_>>()
                        .join(" ")
                );
            }
        } else if let Some(ev) = dict.events_by_id.values().find(|e| e.name == *target) {
            println!(
                "  EVENT {} (id {}, severity {})",
                ev.name, ev.id, ev.severity
            );
            println!("  format: {}", ev.format);
            for p in &ev.params {
                println!(
                    "    {}: {}",
                    p.name,
                    p.ty.primitive_name().unwrap_or(&p.ty.name)
                );
            }
        } else if let Some(ch) = dict.channels_by_id.values().find(|c| c.name == *target) {
            println!(
                "  CHANNEL {} (id {}, type {})",
                ch.name,
                ch.id,
                ch.ty.primitive_name().unwrap_or(&ch.ty.name)
            );
        } else {
            println!("no command/event/channel named {target:?} (try 'cmds <pattern>')");
        }
        return;
    }
    print_help();
}

fn print_help() {
    println!(
        "Commands:\n  \
        help                                 show this message\n  \
        help <name>                          show usage for a command/event/channel\n  \
        cmds [pattern]                       list commands (glob, e.g. CdhCore.*)\n  \
        events [pattern]                     list events\n  \
        channels [pattern]                   list channels\n  \
        dict commands|events|channels [pat]  same as the three above\n  \
        cmd <name> [arg ...]                 send a command (positional args)\n  \
        cmd <name> name=val ...              send a command (named args)\n  \
        raw-uplink <hex-bytes>               uplink an unframed payload\n  \
        mute <pattern>                       suppress downlink items matching glob\n  \
        mute events|channels                 suppress an entire stream\n  \
        unmute <pattern>|events|channels     remove a mute\n  \
        subscribe <pattern>                  show ONLY items matching pattern\n  \
        unsubscribe <pattern>                remove a subscription\n  \
        filters                              list active mutes/subscriptions\n  \
        clear-filters                        reset all filters\n  \
        quit                                 exit\n\
        \n\
        Glob patterns: * matches any chars, ? one char.  Examples:\n  \
        mute Ref.systemResources.CPU_*\n  \
        subscribe CdhCore.*"
    );
}

fn dict_subcmd(parts: &[String], dict: &Dictionary) -> anyhow::Result<()> {
    let kind = parts.first().map(String::as_str).unwrap_or("commands");
    let pattern = parts.get(1).map(String::as_str).unwrap_or("");
    let matches = |name: &str| {
        if pattern.is_empty() {
            true
        } else if pattern.contains('*') || pattern.contains('?') {
            glob_match(pattern, name)
        } else {
            name.contains(pattern)
        }
    };
    match kind {
        "commands" => {
            let mut v: Vec<_> = dict
                .commands_by_opcode
                .values()
                .filter(|c| matches(&c.name))
                .collect();
            v.sort_by_key(|c| c.opcode);
            for c in v {
                let params: Vec<String> = c
                    .params
                    .iter()
                    .map(|p| {
                        format!(
                            "{}: {}",
                            p.name,
                            p.ty.primitive_name().unwrap_or(&p.ty.name)
                        )
                    })
                    .collect();
                println!("  {:>5}  {} ({})", c.opcode, c.name, params.join(", "));
            }
        }
        "events" => {
            let mut v: Vec<_> = dict
                .events_by_id
                .values()
                .filter(|e| matches(&e.name))
                .collect();
            v.sort_by_key(|e| e.id);
            for e in v {
                println!(
                    "  {:>5}  {:<11} {}  ({})",
                    e.id, e.severity, e.name, e.format
                );
            }
        }
        "channels" => {
            let mut v: Vec<_> = dict
                .channels_by_id
                .values()
                .filter(|c| matches(&c.name))
                .collect();
            v.sort_by_key(|c| c.id);
            for c in v {
                println!(
                    "  {:>5}  {} : {}",
                    c.id,
                    c.name,
                    c.ty.primitive_name().unwrap_or(&c.ty.name)
                );
            }
        }
        other => anyhow::bail!("unknown dict kind: {other}"),
    }
    Ok(())
}

fn mute_subcmd(
    parts: &[String],
    filter: &parking_lot_filter::RwLock<Filter>,
    mute: bool,
) -> anyhow::Result<()> {
    let target = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: mute|unmute <pattern>|events|channels"))?;
    let mut f = filter.write();
    match target.as_str() {
        "events" => f.mute_events = mute,
        "channels" => f.mute_channels = mute,
        pat => {
            if mute {
                if !f.mute_patterns.iter().any(|p| p == pat) {
                    f.mute_patterns.push(pat.to_string());
                }
            } else {
                f.mute_patterns.retain(|p| p != pat);
            }
        }
    }
    Ok(())
}

fn subscribe_subcmd(
    parts: &[String],
    filter: &parking_lot_filter::RwLock<Filter>,
) -> anyhow::Result<()> {
    let pat = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: subscribe <pattern>"))?;
    let mut f = filter.write();
    if !f.subscribe_patterns.iter().any(|p| p == pat) {
        f.subscribe_patterns.push(pat.to_string());
    }
    Ok(())
}

fn unsubscribe_subcmd(
    parts: &[String],
    filter: &parking_lot_filter::RwLock<Filter>,
) -> anyhow::Result<()> {
    let pat = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: unsubscribe <pattern>"))?;
    let mut f = filter.write();
    f.subscribe_patterns.retain(|p| p != pat);
    Ok(())
}

fn list_filters(filter: &parking_lot_filter::RwLock<Filter>) {
    let f = filter.read();
    if f.mute_events {
        println!("  mute: events (whole stream)");
    }
    if f.mute_channels {
        println!("  mute: channels (whole stream)");
    }
    for p in &f.mute_patterns {
        println!("  mute: {p}");
    }
    for p in &f.subscribe_patterns {
        println!("  subscribe: {p}");
    }
    if !f.mute_events
        && !f.mute_channels
        && f.mute_patterns.is_empty()
        && f.subscribe_patterns.is_empty()
    {
        println!("  (no filters active)");
    }
}

fn clear_filters(filter: &parking_lot_filter::RwLock<Filter>) {
    let mut f = filter.write();
    f.mute_events = false;
    f.mute_channels = false;
    f.mute_patterns.clear();
    f.subscribe_patterns.clear();
}

/// Recognise `key=value` syntax: returns `(key, value)` if the first `=` is
/// preceded by a valid identifier (alphanumeric + underscore, starting with a
/// non-digit).  Returns `None` for plain values like `42` or `=foo` so they
/// are treated as positional.
fn arg_split_named(arg: &str) -> Option<(&str, &str)> {
    let eq = arg.find('=')?;
    if eq == 0 {
        return None;
    }
    let key = &arg[..eq];
    let first = key.chars().next()?;
    if !(first.is_alphabetic() || first == '_') {
        return None;
    }
    if !key.chars().all(|c| c.is_alphanumeric() || c == '_') {
        return None;
    }
    Some((key, &arg[eq + 1..]))
}

fn send_command(
    name: &str,
    raw_args: &[String],
    dict: &Dictionary,
    uplink: &mpsc::UnboundedSender<Vec<u8>>,
) -> anyhow::Result<()> {
    let cmd = dict
        .command_by_name(name)
        .ok_or_else(|| anyhow::anyhow!("unknown command: {name}"))?;

    // Two arg-passing styles: positional ("cmd Foo 42 bar") and named
    // ("cmd Foo x=42 y=bar").  Disallow mixing the two; named tokens are
    // detected by the presence of '=' before the first quote-stripped char.
    let any_named = raw_args.iter().any(|a| arg_split_named(a).is_some());
    let all_named = raw_args.iter().all(|a| arg_split_named(a).is_some());
    if any_named && !all_named {
        anyhow::bail!("mixing positional and named args is not supported");
    }

    let ordered: Vec<&str> = if all_named && !raw_args.is_empty() {
        let mut by_name: std::collections::HashMap<&str, &str> = Default::default();
        for raw in raw_args {
            let (k, v) = arg_split_named(raw).expect("checked above");
            if by_name.insert(k, v).is_some() {
                anyhow::bail!("duplicate arg: {k}");
            }
        }
        let mut out = Vec::with_capacity(cmd.params.len());
        for p in &cmd.params {
            let v = by_name.remove(p.name.as_str()).ok_or_else(|| {
                anyhow::anyhow!("missing arg: {} (use `help {}` for usage)", p.name, name)
            })?;
            out.push(v);
        }
        if let Some((extra, _)) = by_name.iter().next() {
            anyhow::bail!("unknown arg: {extra}");
        }
        out
    } else {
        if raw_args.len() != cmd.params.len() {
            anyhow::bail!(
                "command {name} expects {} args, got {} (try `help {name}`)",
                cmd.params.len(),
                raw_args.len()
            );
        }
        raw_args.iter().map(String::as_str).collect()
    };

    let mut values = Vec::with_capacity(cmd.params.len());
    for (param, raw) in cmd.params.iter().zip(ordered.iter()) {
        let prim = param
            .ty
            .primitive_name()
            .ok_or_else(|| anyhow::anyhow!("unsupported arg type: {}", param.ty.name))?;
        let v = parse_arg(prim, raw)?;
        values.push(v);
    }
    let body = encode_command(cmd, &values)?;
    uplink
        .send(body)
        .map_err(|_| anyhow::anyhow!("uplink channel closed"))?;
    println!("ok: queued {name} (opcode {})", cmd.opcode);
    Ok(())
}

mod parking_lot_filter {
    use std::sync::{RwLock as StdRwLock, RwLockReadGuard, RwLockWriteGuard};

    pub struct RwLock<T>(StdRwLock<T>);

    impl<T> RwLock<T> {
        pub fn new(t: T) -> Self {
            Self(StdRwLock::new(t))
        }
        pub fn read(&self) -> RwLockReadGuard<'_, T> {
            self.0.read().expect("poisoned")
        }
        pub fn write(&self) -> RwLockWriteGuard<'_, T> {
            self.0.write().expect("poisoned")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn glob_matches_star() {
        assert!(glob_match("Ref.*", "Ref.systemResources.CPU_00"));
        assert!(glob_match(
            "Ref.systemResources.CPU_*",
            "Ref.systemResources.CPU_00"
        ));
        assert!(glob_match("*.CPU_??", "Ref.systemResources.CPU_00"));
        assert!(!glob_match("Ref.*.CPU_*", "CdhCore.cmdDisp.NoOpReceived"));
    }

    #[test]
    fn glob_question_matches_one() {
        assert!(glob_match("CPU_??", "CPU_00"));
        assert!(!glob_match("CPU_?", "CPU_00"));
    }

    #[test]
    fn arg_split_named_recognises_kv() {
        assert_eq!(arg_split_named("x=42"), Some(("x", "42")));
        assert_eq!(
            arg_split_named("foo_bar=baz=qux"),
            Some(("foo_bar", "baz=qux"))
        );
        assert_eq!(arg_split_named("42"), None);
        assert_eq!(arg_split_named("=foo"), None);
        assert_eq!(arg_split_named("9x=1"), None);
    }

    #[test]
    fn filter_allows_with_subscriptions() {
        let mut f = Filter::default();
        f.subscribe_patterns.push("Ref.*".into());
        assert!(f.allows("Ref.systemResources.CPU_00"));
        assert!(!f.allows("CdhCore.cmdDisp.NoOpReceived"));
    }

    #[test]
    fn filter_allows_with_mute() {
        let mut f = Filter::default();
        f.mute_patterns.push("Ref.systemResources.CPU_*".into());
        assert!(!f.allows("Ref.systemResources.CPU_00"));
        assert!(f.allows("Ref.systemResources.MEMORY_TOTAL"));
    }
}
