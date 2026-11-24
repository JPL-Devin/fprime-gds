//! `fgds tui`: the prompt of `fgds repl` on a screen of three panes: events and commands with
//! scrollback, the latest value of each channel, and the command line. Same client, parser,
//! filters and completion as the prompt; only the screen and the line editor are here.

use crate::complete::Names;
use crate::{loaded, reload, show, url_arg, view, Item, Session, PROMPT};
use fgds_core::clap::{ArgMatches, Command};
use fgds_core::serde_json::Value;
use fgds_core::{CliEnv, Error, Result, Subcommand};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Line;
use ratatui::widgets::{Block, Clear, Paragraph};
use ratatui::Frame;
use std::collections::{BTreeMap, VecDeque};
use std::io::IsTerminal;
use std::process::ExitCode;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

/// Lines kept in the events pane.
const SCROLLBACK: usize = 5000;
/// Completions listed at once.
const OFFERS: usize = 8;

#[derive(Clone, Copy, Debug, PartialEq)]
enum Tone {
    Plain,
    Typed,
    Good,
    Notice,
    Warning,
    Bad,
}

impl Tone {
    fn of_event(record: &Value) -> Tone {
        match record["body"]["severity"].as_str() {
            Some("FATAL" | "WARNING_HI") => Tone::Bad,
            Some("WARNING_LO") => Tone::Warning,
            Some("COMMAND") => Tone::Good,
            Some("ACTIVITY_HI") => Tone::Notice,
            _ => Tone::Plain,
        }
    }

    fn style(self) -> Style {
        match self {
            Tone::Plain => Style::new(),
            Tone::Typed => Style::new().add_modifier(Modifier::BOLD),
            Tone::Good => Style::new().fg(Color::Green),
            Tone::Notice => Style::new().fg(Color::Cyan),
            Tone::Warning => Style::new().fg(Color::Yellow),
            Tone::Bad => Style::new().fg(Color::Red),
        }
    }
}

/// What the two upper panes show. The stream fills it from its thread.
#[derive(Default)]
struct Panes {
    log: VecDeque<(String, Tone)>,
    /// Lines of `log` below the last one shown: 0 follows the stream.
    back: usize,
    /// Channel name to its latest value and the time of it.
    channels: BTreeMap<String, (String, String)>,
    /// Rows of the channel table above the first one shown.
    down: usize,
}

impl Panes {
    fn add(&mut self, line: String, tone: Tone) {
        self.log.push_back((line, tone));
        if self.log.len() > SCROLLBACK {
            self.log.pop_front();
        } else if self.back > 0 {
            // Scrolled back: the lines shown stay where they are.
            self.back += 1;
        }
    }

    fn scroll(&mut self, lines: isize) {
        self.back = self.back.saturating_add_signed(lines).min(self.log.len().saturating_sub(1));
    }

    fn sample(&mut self, record: &Value) {
        let body = &record["body"];
        let time = show::flight_time(record);
        let time = time.split_once('T').map_or(time.as_str(), |(_, time)| time).chars().take(12).collect();
        let value = show::visible(body["display_text"].as_str().unwrap_or(""));
        self.channels.insert(show::visible(body["name"].as_str().unwrap_or("?")), (value, time));
    }

    /// The `height` rows of the log that end `back` lines before its end, `width` wide, long
    /// lines on several rows. Near the start of the log, the rows that follow fill the pane.
    fn log_rows(&self, width: usize, height: usize) -> Vec<(String, Tone)> {
        let rows =
            |(line, tone): &(String, Tone)| fold(line, width).into_iter().map(|row| (row, *tone)).collect::<Vec<_>>();
        let end = self.log.len() - self.back.min(self.log.len());
        let mut shown = Vec::new();
        for line in self.log.range(..end).rev() {
            shown.splice(0..0, rows(line));
            if shown.len() >= height {
                break;
            }
        }
        let mut shown = shown.split_off(shown.len().saturating_sub(height));
        for line in self.log.range(end..) {
            if shown.len() >= height {
                break;
            }
            shown.extend(rows(line));
        }
        shown.truncate(height);
        shown
    }

    /// The rows of the channel table for the names `shown` allows, `width` wide. In a narrow
    /// pane the time goes first, then the start of the names: the values stay.
    fn channel_rows(&self, width: usize, shown: impl Fn(&str) -> bool) -> Vec<String> {
        let rows: Vec<_> = self.channels.iter().filter(|(name, _)| shown(name)).collect();
        let widest = |column: usize| {
            let of = |(n, (v, t)): &(&String, &(String, String))| [n.as_str(), v, t][column].width();
            rows.iter().map(of).max().unwrap_or(0)
        };
        let (name, value, time) = (widest(0), widest(1), widest(2));
        let timed = name + value + time + 2 <= width;
        // A long value (a string, an array) is cut by the pane rather than taking the names.
        let name = name.min(width.saturating_sub(value.min(12) + 1)).max(1);
        let pad = |text: &str, width: usize| format!("{text}{}", " ".repeat(width.saturating_sub(text.width())));
        let row = |(n, (v, t)): &(&String, &(String, String))| {
            let n = if n.width() > name {
                let (end, _) = columns(n, n.width() + 1 - name, name);
                format!("…{end}")
            } else {
                n.to_string()
            };
            if timed {
                format!("{} {} {t}", pad(&n, name), pad(v, value))
            } else {
                format!("{} {v}", pad(&n, name))
            }
        };
        rows.iter().map(row).collect()
    }
}

fn wide(c: char) -> usize {
    c.width().unwrap_or(0)
}

/// `line` cut into rows of `width` columns: a wide character (CJK, emoji) takes two.
fn fold(line: &str, width: usize) -> Vec<String> {
    let (mut rows, mut used) = (vec![String::new()], 0);
    for c in line.chars() {
        if used > 0 && used + wide(c) > width {
            rows.push(String::new());
            used = 0;
        }
        rows.last_mut().expect("never empty").push(c);
        used += wide(c);
    }
    rows
}

/// What `text` has from the column `skip` on, `width` columns at most, and the columns left
/// out before it: more than `skip` when a wide character lies across that column.
fn columns(text: &str, skip: usize, width: usize) -> (String, usize) {
    let (mut shown, mut skipped, mut used) = (String::new(), 0, 0);
    for c in text.chars() {
        if skipped < skip {
            skipped += wide(c);
        } else if used + wide(c) <= width {
            shown.push(c);
            used += wide(c);
        } else {
            break;
        }
    }
    (shown, skipped)
}

/// The one line the GDS is being asked. A second one is refused: none is kept to be sent later.
#[derive(Default)]
struct Flight {
    asked: Mutex<Option<String>>,
    left: AtomicBool,
}

impl Flight {
    fn asked(&self) -> std::sync::MutexGuard<'_, Option<String>> {
        self.asked.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Takes `line` as the one being asked; the line still without its answer otherwise.
    fn begin(&self, line: &str) -> std::result::Result<(), String> {
        let mut asked = self.asked();
        match asked.as_ref() {
            Some(waiting) => Err(waiting.clone()),
            None => {
                *asked = Some(line.to_string());
                Ok(())
            }
        }
    }

    fn end(&self) {
        *self.asked() = None;
    }

    /// The user leaves: nothing is posted from now on. The line still without its answer.
    fn leave(&self) -> Option<String> {
        self.left.store(true, Ordering::SeqCst);
        self.asked().clone()
    }

    fn may_post(&self) -> bool {
        !self.left.load(Ordering::SeqCst)
    }
}

/// The command line: the text, the cursor, the lines typed before, and what Tab offered.
#[derive(Default)]
struct Editor {
    line: String,
    /// The cursor, as a byte of `line`.
    pos: usize,
    history: Vec<String>,
    /// The line of `history` shown; `history.len()` is the line being typed.
    at: usize,
    offers: Vec<String>,
}

enum Asked {
    Nothing,
    Run(String),
    Quit,
    /// Lines to scroll the events pane (back is positive), in halves of its height.
    Events(isize),
    Channels(isize),
}

impl Editor {
    fn set(&mut self, line: String) {
        self.pos = line.len();
        self.line = line;
    }

    fn step(&self, forward: bool) -> usize {
        let next = |c: char| c.len_utf8();
        if forward {
            self.pos + self.line[self.pos..].chars().next().map_or(0, next)
        } else {
            self.pos - self.line[..self.pos].chars().next_back().map_or(0, next)
        }
    }

    /// Tab: the only completion, or as much as all of them share, the rest listed.
    fn complete(&mut self, names: &Names) {
        let (start, mut found) = names.complete(&self.line, self.pos);
        let Some(first) = found.first().cloned() else { return };
        let mut shared = found.iter().fold(first.len(), |len, name| {
            len.min(first.bytes().zip(name.bytes()).take_while(|(a, b)| a == b).count())
        });
        while !first.is_char_boundary(shared) {
            shared -= 1;
        }
        let typed = &self.line[start..self.pos];
        let takes = if found.len() == 1 { format!("{first} ") } else { first[..shared].to_string() };
        // A name is also offered for its last part (`CMD_NO`): what the offers share is taken
        // when it still holds what was typed, there or at its start.
        let holds =
            |text: &str| text.starts_with(typed) || text.rsplit('.').next().is_some_and(|last| last.starts_with(typed));
        if found.len() == 1 || (takes != typed && holds(&takes)) {
            self.line.replace_range(start..self.pos, &takes);
            self.pos = start + takes.len();
        }
        if found.len() == 1 {
            found.clear();
        }
        self.offers = found;
    }

    fn key(&mut self, key: KeyEvent, names: &Names) -> Asked {
        let control = key.modifiers.contains(KeyModifiers::CONTROL);
        let alt = key.modifiers.contains(KeyModifiers::ALT);
        if key.code != KeyCode::Tab {
            self.offers.clear();
        }
        match key.code {
            KeyCode::Char('c') if control => self.set(String::new()),
            KeyCode::Char('d') if control && self.line.is_empty() => return Asked::Quit,
            KeyCode::Char('a') if control => self.pos = 0,
            KeyCode::Char('e') if control => self.pos = self.line.len(),
            KeyCode::Char('u') if control => {
                self.line.drain(..self.pos);
                self.pos = 0;
            }
            KeyCode::Char('k') if control => self.line.truncate(self.pos),
            // What `show::visible` would not leave as it is has no place on the line; pasted text has some.
            KeyCode::Char(c)
                if !control && !alt && (c == '\\' || crate::complete::plain(c.encode_utf8(&mut [0; 4]))) =>
            {
                self.line.insert(self.pos, c);
                self.pos += c.len_utf8();
            }
            KeyCode::Backspace if self.pos > 0 => {
                let from = self.step(false);
                self.line.drain(from..self.pos);
                self.pos = from;
            }
            KeyCode::Delete if self.pos < self.line.len() => {
                self.line.drain(self.pos..self.step(true));
            }
            KeyCode::Left => self.pos = self.step(false),
            KeyCode::Right => self.pos = self.step(true),
            KeyCode::Home => self.pos = 0,
            KeyCode::End => self.pos = self.line.len(),
            KeyCode::Up if alt => return Asked::Channels(-1),
            KeyCode::Down if alt => return Asked::Channels(1),
            KeyCode::Up if self.at > 0 => {
                self.at -= 1;
                self.set(self.history[self.at].clone());
            }
            KeyCode::Down if self.at < self.history.len() => {
                self.at += 1;
                self.set(self.history.get(self.at).cloned().unwrap_or_default());
            }
            KeyCode::PageUp => return Asked::Events(1),
            KeyCode::PageDown => return Asked::Events(-1),
            KeyCode::Tab => self.complete(names),
            KeyCode::Enter => {
                let line = std::mem::take(&mut self.line);
                self.pos = 0;
                if !line.trim().is_empty() && self.history.last() != Some(&line) {
                    self.history.push(line.clone());
                }
                self.at = self.history.len();
                return Asked::Run(line);
            }
            _ => {}
        }
        Asked::Nothing
    }
}

fn inner(area: Rect) -> (usize, usize) {
    (usize::from(area.width.saturating_sub(2)), usize::from(area.height.saturating_sub(2)))
}

/// Draws the screen and returns the heights of the events pane and of the channel table.
fn draw(
    frame: &mut Frame,
    panes: &mut Panes,
    editor: &Editor,
    waiting: Option<&str>,
    shown: impl Fn(&str) -> bool,
    colour: bool,
) -> (usize, usize) {
    let [upper, bottom] = Layout::vertical([Constraint::Min(3), Constraint::Length(3)]).areas(frame.area());
    let [left, right] = Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)]).areas(upper);

    let (width, height) = inner(left);
    let tone = |tone: Tone| if colour { tone.style() } else { Style::new() };
    let rows: Vec<Line> =
        panes.log_rows(width, height).into_iter().map(|(row, t)| Line::styled(row, tone(t))).collect();
    let title = match panes.back {
        0 => " Events and commands ".to_string(),
        back => format!(" Events and commands: {back} lines back, PgDn to follow "),
    };
    frame.render_widget(Paragraph::new(rows).block(Block::bordered().title(title)), left);

    let (width, table) = inner(right);
    let rows = panes.channel_rows(width, shown);
    panes.down = panes.down.min(rows.len().saturating_sub(table));
    let title = format!(" Channels: {} of {} ", rows.len(), panes.channels.len());
    let rows: Vec<Line> = rows.into_iter().skip(panes.down).take(table).map(Line::raw).collect();
    frame.render_widget(Paragraph::new(rows).block(Block::bordered().title(title)), right);

    // The end of a line longer than the pane is shown, with the cursor in it.
    let (width, _) = inner(bottom);
    let before = PROMPT.width() + editor.line[..editor.pos].width();
    let (text, skipped) = columns(&format!("{PROMPT}{}", editor.line), (before + 1).saturating_sub(width), width);
    let title = match waiting {
        Some(line) => format!(" waiting for the GDS to answer `{line}`; nothing else is sent meanwhile "),
        None => " Tab completes, PgUp/PgDn events, Alt-Up/Down channels, `help`, `quit` ".to_string(),
    };
    frame.render_widget(Paragraph::new(text).block(Block::bordered().title(title)), bottom);
    let column = u16::try_from(before.saturating_sub(skipped)).unwrap_or(0);
    frame.set_cursor_position((bottom.x.saturating_add(1).saturating_add(column), bottom.y.saturating_add(1)));

    if !editor.offers.is_empty() {
        let mut offers: Vec<String> = editor.offers.iter().take(OFFERS).cloned().collect();
        if editor.offers.len() > OFFERS {
            offers.push(format!("... and {} more", editor.offers.len() - OFFERS));
        }
        let wide = offers.iter().map(|offer| offer.width()).max().unwrap_or(0) + 2;
        let wide = u16::try_from(wide).unwrap_or(u16::MAX).min(upper.width);
        let high = u16::try_from(offers.len() + 2).unwrap_or(u16::MAX).min(upper.height);
        let area = Rect::new(bottom.x, bottom.y.saturating_sub(high), wide, high);
        frame.render_widget(Clear, area);
        let offers: Vec<Line> = offers.into_iter().map(Line::raw).collect();
        frame.render_widget(Paragraph::new(offers).block(Block::bordered().title(" Tab ")), area);
    }
    (height, table)
}

fn panes(shared: &Mutex<Panes>) -> std::sync::MutexGuard<'_, Panes> {
    shared.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Draws the screen and reads the keys until the user leaves. Returns the line the GDS had not
/// answered by then.
fn screen(session: &Arc<Session>, terminal: &mut ratatui::DefaultTerminal) -> std::io::Result<Option<String>> {
    let colour = std::env::var_os("NO_COLOR").is_none();
    let shared = Arc::new(Mutex::new(Panes::default()));
    panes(&shared).add(format!("fgds tui on {}", show::visible(session.api.base())), Tone::Plain);

    let ((api, follower), panes_of_stream) = (session.follower(), shared.clone());
    std::thread::spawn(move || {
        let Ok(runtime) = tokio::runtime::Builder::new_current_thread().enable_all().build() else { return };
        let own = api.client().to_string();
        let on = |item: Item| match item {
            Item::Record(record) if record["kind"] == "fprime.channel" => panes(&panes_of_stream).sample(record),
            Item::Record(record) => {
                let tone = if record["kind"] == "fprime.event" { Tone::of_event(record) } else { Tone::Plain };
                let line = view(&follower.shared).line(record, &own, false);
                line.into_iter().for_each(|line| panes(&panes_of_stream).add(line, tone));
            }
            Item::Note(note) => panes(&panes_of_stream).add(show::visible(&note), Tone::Warning),
            Item::Dictionary(read) => panes(&panes_of_stream).add(reload(&follower.dictionary, read), Tone::Warning),
        };
        runtime.block_on(crate::stream(api, follower.version.clone(), on));
    });

    let flight = Arc::new(Flight::default());
    let answer = |shared: &Mutex<Panes>, lines: Vec<String>| {
        let mut panes = panes(shared);
        for line in lines {
            let tone =
                if line.starts_with("not sent") || line.starts_with("no answer") { Tone::Bad } else { Tone::Plain };
            panes.add(line, tone);
        }
    };
    let mut editor = Editor::default();
    let (mut events, mut table) = (1, 1);
    loop {
        let waiting = flight.asked().clone();
        terminal.draw(|frame| {
            let filter = &view(&session.shared).filter;
            let shown = |name: &str| filter.allows(false, name);
            (events, table) = draw(frame, &mut panes(&shared), &editor, waiting.as_deref(), shown, colour);
        })?;
        if !event::poll(Duration::from_millis(100))? {
            continue;
        }
        // Every key that is there before the next drawing: pasted text is drawn once.
        while event::poll(Duration::ZERO)? {
            let Event::Key(key) = event::read()? else { continue };
            if key.kind == KeyEventKind::Release {
                continue;
            }
            let half = |pane: usize| isize::try_from(pane / 2).unwrap_or(1).max(1);
            let line = match editor.key(key, &loaded(&session.dictionary).names) {
                Asked::Nothing => continue,
                Asked::Quit => return Ok(flight.leave()),
                Asked::Events(way) => {
                    panes(&shared).scroll(way * half(events));
                    continue;
                }
                Asked::Channels(way) => {
                    let mut panes = panes(&shared);
                    panes.down = panes.down.saturating_add_signed(way * half(table));
                    continue;
                }
                Asked::Run(line) => line,
            };
            {
                let mut panes = panes(&shared);
                panes.back = 0;
                panes.add(format!("{PROMPT}{}", show::visible(&line)), Tone::Typed);
            }
            // What goes to the link is asked on a thread of its own, one line at a time: the
            // screen is drawn while the GDS answers, and no line waits to be sent later.
            if !matches!(crate::line::parse(&line), Ok(crate::line::Line::Cmd { .. } | crate::line::Line::RawUplink(_)))
            {
                match session.handle(&line) {
                    Some(lines) => answer(&shared, lines),
                    None => return Ok(flight.leave()),
                }
            } else if let Err(waiting) = flight.begin(&line) {
                let refusal =
                    format!("not sent: the GDS has not answered `{waiting}`; type the line again after its answer");
                answer(&shared, vec![show::visible(&refusal)]);
            } else {
                let (session, shared, flight) = (session.clone(), shared.clone(), flight.clone());
                std::thread::spawn(move || {
                    if flight.may_post() {
                        answer(&shared, session.handle(&line).unwrap_or_default());
                    }
                    flight.end();
                });
            }
        }
    }
}

/// `fgds tui`.
pub struct Tui;

impl Subcommand for Tui {
    fn command(&self) -> Command {
        Command::new("tui")
            .about("Three panes on a running GDS: events and commands, latest channel values, command line")
            .after_help("The command line takes what `fgds repl` takes: type `help`. It needs a terminal.")
            .arg(url_arg())
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        if !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
            let reason = "fgds tui needs a terminal; `fgds repl` prints the stream without one";
            return Err(Error::Invalid(reason.to_string()));
        }
        let session = Arc::new(Session::open("tui", args, env)?);
        // The lock keeps the thread of the screen from drawing after the terminal is put back.
        crate::restore_on_signal(|| {
            let _drawing = std::io::stdout().lock();
            ratatui::restore();
        });
        // `init` also puts the terminal back when the program panics.
        let mut terminal = ratatui::try_init().map_err(Error::Io)?;
        let ended = screen(&session, &mut terminal);
        ratatui::restore();
        // The process ends here without waiting for that answer.
        if let Some(line) = ended.map_err(Error::Io)? {
            eprintln!(
                "fgds tui: left before the GDS answered `{}`: it was posted and may have been sent",
                show::visible(&line)
            );
        }
        Ok(ExitCode::SUCCESS)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fgds_core::serde_json::json;

    fn names() -> Names {
        Names::of(&json!({"commands": [
            {"name": "A.disp.CMD_NO_OP", "formalParams": []},
            {"name": "A.disp.CMD_NO_OP_STRING", "formalParams": []},
            {"name": "B.other.CMD_GO", "formalParams": []},
        ]}))
    }

    #[test]
    fn a_wide_character_takes_two_columns() {
        assert_eq!(fold("ab漢字漢字c", 5), ["ab漢", "字漢", "字c"]);
        assert_eq!(fold("漢", 1), ["漢"]);
        assert_eq!(fold("", 4), [""]);
        assert_eq!(columns("fgds> 漢字x", 7, 4), ("字x".to_string(), 8));
        let mut panes = Panes::default();
        panes.sample(&json!({"body": {"name": "A.b.漢字", "display_text": "é\u{1b}[2J"}}));
        panes.sample(&json!({"body": {"name": "A.b.Count", "display_text": "7"}}));
        let rows = panes.channel_rows(40, |_| true);
        assert_eq!(rows[0].find('7'), rows[1].find('é').map(|at| at - 2), "{rows:?}");
        assert!(rows[1].contains("é\\x1b[2J"), "{rows:?}");
    }

    #[test]
    fn one_line_is_asked_at_a_time_and_none_after_leaving() {
        let flight = Flight::default();
        assert_eq!(flight.begin("cmd A"), Ok(()));
        assert_eq!(flight.begin("cmd B"), Err("cmd A".to_string()));
        flight.end();
        assert_eq!(flight.begin("cmd C"), Ok(()));
        assert!(flight.may_post());
        assert_eq!(flight.leave(), Some("cmd C".to_string()));
        assert!(!flight.may_post());
    }

    #[test]
    fn every_size_of_terminal_is_drawn() {
        let mut panes = Panes::default();
        panes.add("a line 漢字 longer than a small pane is wide".to_string(), Tone::Plain);
        panes.sample(&json!({"body": {"name": "A.b.Count", "display_text": "7"}}));
        let mut editor = Editor::default();
        typed(&mut editor, "cmd X 漢字漢字🚀é");
        editor.offers = vec!["A.disp.CMD_NO_OP".to_string(); 12];
        for (width, height) in [(1, 1), (2, 2), (3, 3), (10, 4), (40, 10), (180, 45)] {
            let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
            terminal
                .draw(|frame| {
                    let _ = draw(frame, &mut panes, &editor, Some("cmd A"), |_| true, true);
                })
                .unwrap();
        }
        // The cursor is at the column of the insertion point: wide characters count for two.
        editor.pos -= "🚀é".len();
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(60, 14)).unwrap();
        terminal
            .draw(|frame| {
                let _ = draw(frame, &mut panes, &editor, None, |_| true, false);
            })
            .unwrap();
        let column = u16::try_from("fgds> cmd X ".len() + 8 + 1).unwrap();
        terminal.backend_mut().assert_cursor_position((column, 12));
    }

    fn typed(editor: &mut Editor, text: &str) {
        for c in text.chars() {
            editor.key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &names());
        }
    }

    fn press(editor: &mut Editor, code: KeyCode) -> Asked {
        editor.key(KeyEvent::new(code, KeyModifiers::NONE), &names())
    }

    #[test]
    fn long_lines_take_several_rows() {
        assert_eq!(fold("abcdefg", 3), ["abc", "def", "g"]);
        assert_eq!(fold("", 3), [""]);
        assert_eq!(fold("ab", 0), ["a", "b"]);
    }

    #[test]
    fn the_events_pane_follows_the_stream_until_scrolled_back() {
        let mut panes = Panes::default();
        for n in 0..10 {
            panes.add(format!("line {n}"), Tone::Plain);
        }
        let rows = |panes: &Panes| panes.log_rows(20, 3).into_iter().map(|(row, _)| row).collect::<Vec<_>>();
        assert_eq!(rows(&panes), ["line 7", "line 8", "line 9"]);
        panes.scroll(2);
        assert_eq!(rows(&panes), ["line 5", "line 6", "line 7"]);
        // A line that arrives does not move what is shown.
        panes.add("line 10".to_string(), Tone::Plain);
        assert_eq!(rows(&panes), ["line 5", "line 6", "line 7"]);
        // At the start of the log the pane is still full.
        panes.scroll(100);
        assert_eq!(rows(&panes), ["line 0", "line 1", "line 2"]);
        panes.scroll(-100);
        assert_eq!(rows(&panes), ["line 8", "line 9", "line 10"]);
        // The end of a long line is its last row.
        panes.add("abcdefgh".to_string(), Tone::Plain);
        assert_eq!(panes.log_rows(5, 2), [("abcde".to_string(), Tone::Plain), ("fgh".to_string(), Tone::Plain)]);
    }

    #[test]
    fn the_log_is_bounded() {
        let mut panes = Panes::default();
        for n in 0..SCROLLBACK + 5 {
            panes.add(n.to_string(), Tone::Plain);
        }
        assert_eq!(panes.log.len(), SCROLLBACK);
        assert_eq!(panes.log.front().map(|(line, _)| line.as_str()), Some("5"));
    }

    #[test]
    fn the_table_holds_the_latest_value_of_each_channel() {
        let mut panes = Panes::default();
        let sample = |name: &str, value: &str| {
            json!({"kind": "fprime.channel", "flight_time": {"base": 2, "seconds": 1},
                "body": {"name": name, "display_text": value}})
        };
        panes.sample(&sample("B.x.Count", "1"));
        panes.sample(&sample("A.y.State", "IDLE"));
        panes.sample(&sample("B.x.Count", "2"));
        let rows = panes.channel_rows(40, |_| true);
        assert_eq!(
            rows,
            ["A.y.State IDLE 00:00:01.000", "B.x.Count 2    00:00:01.000"].map(String::from),
            "{:?}",
            panes.channels
        );
        // A narrow pane: no time, then the end of the names.
        assert_eq!(panes.channel_rows(26, |_| true), ["A.y.State IDLE", "B.x.Count 2"]);
        assert_eq!(panes.channel_rows(10, |_| true), ["…tate IDLE", "…ount 2"]);
        assert_eq!(panes.channel_rows(40, |name| name.starts_with("A.")).len(), 1);
    }

    #[test]
    fn the_line_is_edited() {
        let mut editor = Editor::default();
        typed(&mut editor, "cmd é");
        press(&mut editor, KeyCode::Left);
        press(&mut editor, KeyCode::Backspace);
        typed(&mut editor, "_");
        assert_eq!((editor.line.as_str(), editor.pos), ("cmd_é", 4));
        press(&mut editor, KeyCode::Delete);
        press(&mut editor, KeyCode::Home);
        typed(&mut editor, ">");
        assert_eq!(editor.line, ">cmd_");
        assert!(matches!(press(&mut editor, KeyCode::Enter), Asked::Run(line) if line == ">cmd_"));
        assert_eq!(editor.line, "");
        // Up brings the line back, Down the empty one.
        press(&mut editor, KeyCode::Up);
        assert_eq!(editor.line, ">cmd_");
        press(&mut editor, KeyCode::Down);
        assert_eq!(editor.line, "");
        typed(&mut editor, "x");
        editor.key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL), &names());
        assert_eq!(editor.line, "");
        assert!(matches!(editor.key(KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL), &names()), Asked::Quit));
        assert!(matches!(press(&mut editor, KeyCode::PageUp), Asked::Events(1)));
        assert!(matches!(editor.key(KeyEvent::new(KeyCode::Down, KeyModifiers::ALT), &names()), Asked::Channels(1)));
    }

    #[test]
    fn tab_takes_what_the_offers_share_and_lists_them() {
        let mut editor = Editor::default();
        typed(&mut editor, "cm");
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.line, "cmd");
        assert_eq!(editor.offers, ["cmd", "cmds"]);
        typed(&mut editor, " CMD_NO");
        assert!(editor.offers.is_empty());
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.line, "cmd A.disp.CMD_NO_OP");
        assert_eq!(editor.offers, ["A.disp.CMD_NO_OP", "A.disp.CMD_NO_OP_STRING"]);
        typed(&mut editor, "_S");
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.line, "cmd A.disp.CMD_NO_OP_STRING ");
        assert!(editor.offers.is_empty());
        // Names that share nothing of what was typed are listed and the line is left.
        editor.set("cmd CMD".to_string());
        press(&mut editor, KeyCode::Tab);
        assert_eq!(editor.line, "cmd CMD");
        assert_eq!(editor.offers.len(), 3);
    }
}
