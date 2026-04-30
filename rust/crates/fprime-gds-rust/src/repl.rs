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
use fprime_pipeline::{decode_packet, encode_command, parse_arg, Decoded};
use rustyline::error::ReadlineError;
use tokio::sync::mpsc;

#[derive(Debug, Default, Clone)]
struct Filter {
    mute_events: bool,
    mute_channels: bool,
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
    let printer: Box<dyn LinePrinter> = match start_repl(input_tx.clone(), shutdown.clone()) {
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
    input_tx: mpsc::UnboundedSender<Line>,
    shutdown: Arc<AtomicBool>,
) -> anyhow::Result<Box<dyn LinePrinter>> {
    let mut editor: rustyline::Editor<(), rustyline::history::DefaultHistory> =
        rustyline::Editor::new()?;
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
    mut editor: rustyline::Editor<(), rustyline::history::DefaultHistory>,
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
    let now = Local::now().format("%H:%M:%S%.3f");
    match decode_packet(packet, dict) {
        Ok(Decoded::Event(e)) => {
            if filter.read().mute_events {
                return;
            }
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
            if filter.read().mute_channels {
                return;
            }
            printer.print(format!(
                "{now}  TLM {name} = {value}",
                name = c.channel.name,
                value = c.value
            ));
        }
        Ok(Decoded::Handshake(_)) => {
            // Handshake packets are mostly noise — keep them silent.
        }
        Ok(Decoded::PacketizedTelem(body)) => {
            printer.print(format!(
                "{now}  PKTLM {} bytes (decoding not yet supported)",
                body.len()
            ));
        }
        Ok(Decoded::Unknown { descriptor, body }) => {
            printer.print(format!(
                "{now}  UNK desc={descriptor:#06x} {} bytes",
                body.len()
            ));
        }
        Err(e) => {
            printer.print(format!("{now}  decode error: {e}"));
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
        "help" => print_help(),
        "dict" => dict_subcmd(&parts[1..], dict)?,
        "mute" => toggle_filter(&parts[1..], filter, true)?,
        "unmute" => toggle_filter(&parts[1..], filter, false)?,
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

fn print_help() {
    println!(
        "Commands:\n  help                                show this message\n  \
        dict commands [pattern]              list commands (optionally substring filtered)\n  \
        dict events [pattern]                list events\n  \
        dict channels [pattern]              list channels\n  \
        cmd <qualified.name> [args...]       encode + uplink a command\n  \
        raw-uplink <hex-bytes>               uplink an unframed payload\n  \
        mute events|channels                 silence one downlink stream\n  \
        unmute events|channels               unsilence\n  \
        quit                                 exit"
    );
}

fn dict_subcmd(parts: &[String], dict: &Dictionary) -> anyhow::Result<()> {
    let kind = parts.first().map(String::as_str).unwrap_or("commands");
    let pattern = parts.get(1).map(String::as_str).unwrap_or("");
    match kind {
        "commands" => {
            let mut v: Vec<_> = dict
                .commands_by_opcode
                .values()
                .filter(|c| pattern.is_empty() || c.name.contains(pattern))
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
                .filter(|e| pattern.is_empty() || e.name.contains(pattern))
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
                .filter(|c| pattern.is_empty() || c.name.contains(pattern))
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

fn toggle_filter(
    parts: &[String],
    filter: &parking_lot_filter::RwLock<Filter>,
    mute: bool,
) -> anyhow::Result<()> {
    let target = parts
        .first()
        .ok_or_else(|| anyhow::anyhow!("usage: mute|unmute events|channels"))?;
    let mut f = filter.write();
    match target.as_str() {
        "events" => f.mute_events = mute,
        "channels" => f.mute_channels = mute,
        other => anyhow::bail!("unknown stream: {other}"),
    }
    Ok(())
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
    if raw_args.len() != cmd.params.len() {
        anyhow::bail!(
            "command {name} expects {} args, got {}",
            cmd.params.len(),
            raw_args.len()
        );
    }
    let mut values = Vec::with_capacity(cmd.params.len());
    for (param, raw) in cmd.params.iter().zip(raw_args.iter()) {
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
