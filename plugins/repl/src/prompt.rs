//! `fgds repl`: the prompt on a terminal, and the stream alone without one. Everything `fgds tui`
//! does not use is here: the line editor (rustyline), what it may take, and printing above it.

use crate::api::Api;
use crate::{
    loaded, reload, restore_on_signal, show, shown, stream, url_arg, view, Dictionary, Follower, Item, Session, PROMPT,
};
use fgds_core::clap::{ArgMatches, Command};
use fgds_core::{CliEnv, Error, Result, Subcommand};
use rustyline::error::ReadlineError;
use rustyline::history::DefaultHistory;
use rustyline::{Cmd, ColorMode, CompletionType, Config, Editor, KeyEvent};
use std::io::{IsTerminal, Write};
use std::process::ExitCode;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The stream as lines, through the filters, for `print`.
async fn stream_lines(api: Api, session: Follower, colour: bool, mut print: impl FnMut(String)) {
    let own = api.client().to_string();
    let on = |item: Item| match item {
        Item::Record(record) => view(&session.shared).line(record, &own, colour).into_iter().for_each(&mut print),
        Item::Note(note) => print(show::visible(&note)),
        Item::Dictionary(read) => print(reload(&session.dictionary, read)),
    };
    let read = loaded(&session.dictionary).value.clone();
    stream(api, read, on).await;
}

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

/// Keeps out of the line what would not be shown as it is (control characters, marks that change
/// the direction of text or have no width), typed or pasted, and the quoted insert that takes any.
///
/// A paste comes between two marks of the terminal (bracketed paste) and is read here key by key,
/// not by the editor, which would take it whole and unseen. In a paste no key is a command: a
/// line break or a tab is a space, so that nothing pasted runs before Enter is pressed.
#[derive(Default)]
struct Typed {
    /// When the last key of a paste was read; a paste whose end mark never comes ends after `PASTE`.
    pasting: Mutex<Option<Instant>>,
}

const PASTE: Duration = Duration::from_millis(500);

impl rustyline::ConditionalEventHandler for Typed {
    fn handle(
        &self,
        event: &rustyline::Event,
        _: rustyline::RepeatCount,
        _: bool,
        _: &rustyline::EventContext,
    ) -> Option<Cmd> {
        use rustyline::{KeyCode, Modifiers};
        let key = event.get(0)?;
        let mut pasting = self.pasting.lock().unwrap_or_else(std::sync::PoisonError::into_inner);
        if pasting.is_some_and(|last| last.elapsed() > PASTE) {
            *pasting = None;
        }
        match key {
            KeyEvent(KeyCode::BracketedPasteStart, _) => {
                *pasting = Some(Instant::now());
                return Some(Cmd::Noop);
            }
            KeyEvent(KeyCode::BracketedPasteEnd, _) => {
                *pasting = None;
                return Some(Cmd::Noop);
            }
            _ if pasting.is_some() => {
                *pasting = Some(Instant::now());
                return match key {
                    KeyEvent(KeyCode::Char(c), Modifiers::NONE | Modifiers::SHIFT) if show::typed(*c) => None,
                    KeyEvent(KeyCode::Enter | KeyCode::Tab, _)
                    | KeyEvent(KeyCode::Char('J' | 'M' | 'I'), Modifiers::CTRL) => {
                        Some(Cmd::Insert(1, " ".to_string()))
                    }
                    _ => Some(Cmd::Noop),
                };
            }
            _ => {}
        }
        match key {
            KeyEvent(KeyCode::Char('V' | 'Q'), Modifiers::CTRL) => Some(Cmd::Noop),
            KeyEvent(KeyCode::Char(c), Modifiers::NONE | Modifiers::SHIFT) if !show::typed(*c) => Some(Cmd::Noop),
            _ => None,
        }
    }
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

fn other(error: impl std::fmt::Display) -> Error {
    Error::Other(error.to_string())
}

/// The prompt, on a terminal. The stream runs on a thread of its own and prints above the line.
fn prompt(session: Session, colour: bool) -> Result<ExitCode> {
    // The colour mode is forced so that the editor asks its highlighter (see `Helper`).
    let config = Config::builder()
        .completion_type(CompletionType::List)
        .color_mode(ColorMode::Forced)
        // The terminal marks a paste; `Typed` reads it.
        .bracketed_paste(true)
        .build();
    let mut editor: Editor<Helper, DefaultHistory> = Editor::with_config(config).map_err(other)?;
    let shown = Shown::default();
    editor.bind_sequence(rustyline::Event::Any, rustyline::EventHandler::Conditional(Box::new(Typed::default())));
    editor.set_helper(Some(Helper { dictionary: session.dictionary.clone(), shown: shown.clone() }));
    let history = std::env::var_os("HOME").map(|home| std::path::Path::new(&home).join(".fgds_history"));
    if let Some(history) = &history {
        let _ = editor.load_history(history);
        // A file written by something else: only lines that are shown as they are can be recalled.
        let kept: Vec<String> = editor.history().iter().filter(|line| line.chars().all(show::typed)).cloned().collect();
        let _ = editor.clear_history();
        kept.iter().for_each(|line| drop(editor.add_history_entry(line.as_str())));
    }
    println!("fgds repl on {}: `help` lists what can be typed, `quit` leaves.", show::visible(session.api.base()));
    // Ctrl-C is a key of the editor here, not a signal: SIGINT comes from another process.
    if let Ok(modes) = nix::sys::termios::tcgetattr(std::io::stdin()) {
        restore_on_signal(move || {
            let _ = nix::sys::termios::tcsetattr(std::io::stdin(), nix::sys::termios::SetArg::TCSANOW, &modes);
            // Bracketed paste off, as the editor leaves it.
            let _ = std::io::stdout().write_all(b"\x1b[?2004l\r\n");
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
            // `Typed` lets none of these in; should the editor find another way, the line stops here.
            Ok(line) if !line.chars().all(show::typed) => {
                print_above(&shown, "line dropped: it holds a character that is not shown as it is");
            }
            Ok(line) => {
                let _ = editor.add_history_entry(line.as_str());
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
            Err(ReadlineError::Io(error)) if error.kind() != std::io::ErrorKind::InvalidData => {
                return Err(Error::Io(error))
            }
            // Bytes that are not text (a paste): the line is dropped, the prompt goes on.
            Err(error) => print_above(&shown, &show::visible(&format!("line dropped: {error}"))),
        }
    }
    Ok(ExitCode::SUCCESS)
}

/// `fgds repl`.
pub struct Repl;

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
        repl(args, env).map_err(shown)
    }
}

fn repl(args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
    {
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
