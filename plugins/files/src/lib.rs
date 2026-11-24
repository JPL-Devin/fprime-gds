//! F Prime file uplink and downlink (FIL-1..5, section 4.5 of the design).
//!
//! Downlink: the decoder turns each file packet into a transient `fprime.file.packet`
//! record; the downlink handler writes the data to a file under `<directory>/downlink`
//! and publishes `fprime.file.downlink` progress records and `fprime.file.problem`
//! records. Uplink: a handler owns the queue, sends file packets with
//! `Uplink::send_packet` and publishes `fprime.file.uplink` progress records. File
//! content is in files only, never in a stored record.

mod cli;
mod downlink;
pub mod packet;
mod routes;
mod uplink;

use fgds_core::bytes::Bytes;
use fgds_core::serde_json::{json, Value};
use fgds_core::{
    Context, DecodeError, Decoder, Dictionary, Error, FieldSpec, FieldType, KindSpec, Manifest, NewRecord, OptionSpec,
    PacketMatch, PacketMeta, PacketType, Plugin, PluginConfig, Registry, Result, StaticFiles, UiBundle,
};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;
use tokio::sync::Notify;

/// One file packet from the flight software; `raw` is the packet. Transient.
pub const KIND_PACKET: &str = "fprime.file.packet";
/// State and progress of one uplink; the entity is the uplink.
pub const KIND_UPLINK: &str = "fprime.file.uplink";
/// State and progress of one downlink; the entity is the downlink.
pub const KIND_DOWNLINK: &str = "fprime.file.downlink";
/// A file packet that does not fit the transfer in progress.
pub const KIND_PROBLEM: &str = "fprime.file.problem";

const NAME: &str = "files";

/// `directory`: where the files are kept when the option `directory` is not given;
/// `None` is the places of the Python GDS: `<user>/fprime-uplink` and `<user>/fprime-downlink` in the
/// temporary directory of the machine.
#[derive(Debug, Clone, Default)]
pub struct FilesPlugin {
    pub directory: Option<PathBuf>,
}

/// The options of the plugin, read where they are used.
#[derive(Debug, Clone)]
pub(crate) struct Options {
    pub directory: Option<PathBuf>,
    pub chunk: usize,
    pub cooldown: Duration,
    pub timeout: Duration,
    pub max_upload: usize,
}

impl Options {
    fn read(config: &PluginConfig, default_directory: &Option<PathBuf>) -> Result<Options> {
        let number = |name: &str, default: f64| match config.get(name) {
            None => Ok(default),
            Some(text) => match text.parse::<f64>() {
                Ok(n) if n.is_finite() && n >= 0.0 => Ok(n),
                _ => Err(Error::Config(format!("[plugins.files] {name}: \"{text}\" is not a positive number"))),
            },
        };
        let directory = config.get("directory").map(PathBuf::from).or_else(|| default_directory.clone());
        let chunk = number("chunk-size", 256.0)? as usize;
        if chunk == 0 || chunk > usize::from(u16::MAX) {
            return Err(Error::Config("[plugins.files] chunk-size: between 1 and 65535".into()));
        }
        Ok(Options {
            directory,
            chunk,
            cooldown: Duration::from_secs_f64(number("cooldown", 0.01)?),
            timeout: Duration::from_secs_f64(number("timeout", 20.0)?),
            max_upload: number("max-upload-bytes", 64.0 * 1024.0 * 1024.0)? as usize,
        })
    }

    pub fn uplink_dir(&self) -> PathBuf {
        self.place("uplink")
    }

    pub fn downlink_dir(&self) -> PathBuf {
        self.place("downlink")
    }

    /// Without the option `directory`, where the Python GDS keeps the files: the path of the staged copy
    /// is the source path of the start packet, so it is the same bytes for the same user.
    fn place(&self, kind: &str) -> PathBuf {
        match &self.directory {
            Some(directory) => directory.join(kind),
            None => {
                let user = std::env::var("USER").or_else(|_| std::env::var("LOGNAME")).unwrap_or_default();
                std::env::temp_dir().join(user).join(format!("fprime-{kind}"))
            }
        }
    }
}

/// The base name of a name received from the flight software or an API client (FIL-3):
/// what follows the last `/` or `\`. A name that is empty, `.`, `..` or holds a control
/// character is rejected.
pub fn base_name(name: &str) -> Result<&str, String> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or_default();
    if base.is_empty() || base == "." || base == ".." || base.chars().any(char::is_control) {
        return Err(format!("\"{}\" is not a usable file name", name.escape_default()));
    }
    Ok(base)
}

/// A path sent to the flight software in a start packet (`what` is `destination` or `source`), as the
/// operator gave it: nothing is renamed or cut. Refused: an empty path, one longer than the 255 bytes the
/// one length byte of the packet can state (`Fw/FilePacket/FilePacket.hpp`), a control character (NUL
/// included), and a last part that is empty, `.` or `..`.
pub fn flight_path(what: &str, path: &str) -> Result<(), String> {
    let last = path.rsplit('/').next().unwrap_or_default();
    let problem = if path.is_empty() {
        "is empty".to_string()
    } else if path.len() > 255 {
        format!("is {} bytes long in UTF-8; the file packet carries at most 255", path.len())
    } else if path.chars().any(|c| c < ' ' || c == '\x7f') {
        "holds a control character".to_string()
    } else if matches!(last, "" | "." | "..") {
        format!("names no file (it ends in \"{last}\")")
    } else {
        return Ok(());
    };
    Err(format!("the {what} path {problem}"))
}

/// A notice for a path with characters outside ASCII: it is sent as given, in UTF-8.
pub fn flight_path_notice(path: &str) -> Option<String> {
    (!path.is_ascii()).then(|| {
        format!("\"{path}\" holds characters outside ASCII; it is sent as given, in UTF-8, and the file system of the spacecraft may refuse it")
    })
}

/// The path on the spacecraft of an uplinked file, by the rule of the Python GDS for every entry point:
/// a destination is sent as given; without one it is `os.path.join(directory, name)`, where the directory is
/// `/` unless given (`fprime-cli file-uplink SRC` sends `/SRC`; the page gives the text of its field, so an
/// empty field sends the name alone and `.` sends `./name`).
pub fn uplink_destination(destination: Option<&str>, directory: Option<&str>, name: &str) -> String {
    match (destination.filter(|d| !d.is_empty()), directory.unwrap_or("/")) {
        (Some(destination), _) => destination.to_string(),
        (None, "") => name.to_string(),
        (None, directory) if directory.ends_with('/') => format!("{directory}{name}"),
        (None, directory) => format!("{directory}/{name}"),
    }
}

/// The name on the ground of a file the flight software names `destination` (FIL-3):
/// every `/` and `\` becomes `_`, as the Python GDS does, so that two files of two
/// directories of the spacecraft keep two names and the result is one path component.
pub fn downlink_name(destination: &str) -> Result<String, String> {
    let name = destination.replace(['/', '\\'], "_");
    base_name(&name).map(str::to_string)
}

/// The path of `name` inside `directory`; `name` is reduced to its base name first, and
/// the result is checked again to be directly inside `directory` (FIL-3).
pub fn path_inside(directory: &Path, name: &str) -> Result<PathBuf, String> {
    let path = directory.join(base_name(name)?);
    if path.parent() != Some(directory) || path.file_name().is_none() {
        return Err(format!("\"{}\" is outside the storage directory", name.escape_default()));
    }
    Ok(path)
}

/// One uplink or downlink, as the routes and the records show it.
#[derive(Debug, Clone)]
pub(crate) struct Transfer {
    /// The entity of its records.
    pub id: String,
    /// Base name of the file on the ground.
    pub name: String,
    /// Path on the spacecraft.
    pub source: String,
    pub destination: String,
    pub size: u64,
    /// Bytes sent (uplink) or received (downlink).
    pub done: u64,
    /// `queued`, `transferring`, `finished`, `canceled`, `failed` or `removed`.
    pub state: &'static str,
    pub reason: Option<String>,
    pub problems: u64,
    /// Uplink only: the operator asked to cancel it.
    pub cancel: bool,
    /// Ground time when the first packet was sent or received, and when the transfer ended.
    pub started: Option<String>,
    pub ended: Option<String>,
    /// Uplink only: the data packets to send, numbered from 1 (`Select Packets` of the Python GDS page);
    /// every packet of the file when absent.
    pub packets: Option<Vec<u64>>,
    /// Uplink only: bytes of file data per packet and time between two packets of this file, when the
    /// request gave them (`--file-uplink-chunk-size`, `--file-uplink-cooldown`); the options of the plugin otherwise.
    pub chunk: Option<usize>,
    pub cooldown: Option<Duration>,
}

impl Transfer {
    pub fn to_json(&self) -> Value {
        json!({
            "id": self.id, "name": self.name, "source": self.source, "destination": self.destination,
            "size": self.size, "done": self.done, "state": self.state, "reason": self.reason,
            "problems": self.problems, "started": self.started, "ended": self.ended,
            "packets": self.packets.clone().unwrap_or_default(),
            "notice": flight_path_notice(&self.destination),
        })
    }

    pub fn record(&self, kind: &str) -> NewRecord {
        NewRecord { entity: Some(self.id.clone()), ..NewRecord::new(kind, self.to_json()) }
    }

    /// The ground time of now, as the record API writes one.
    pub fn now() -> Option<String> {
        Some(fgds_core::Timestamp::now().to_iso())
    }

    pub fn ended(&self) -> bool {
        !matches!(self.state, "queued" | "transferring")
    }
}

/// Transfers kept for the lists of the routes; the oldest ended ones are dropped.
const KEPT: usize = 500;

#[derive(Default)]
pub(crate) struct State {
    pub next_id: u64,
    /// FIL-4: true until an operator resumes with the link connected.
    pub paused: bool,
    pub uplinks: Vec<Transfer>,
    pub downlinks: Vec<Transfer>,
}

impl State {
    pub fn trim(list: &mut Vec<Transfer>) {
        while list.len() > KEPT {
            match list.iter().position(Transfer::ended) {
                Some(at) => list.remove(at),
                None => break,
            };
        }
    }
}

/// What the handlers and the routes of one GDS share.
pub(crate) struct Shared {
    state: Mutex<State>,
    /// The uplink handler has something to look at.
    pub wake: Notify,
    default_directory: Option<PathBuf>,
    /// Distinguishes the entities of this process from those of earlier runs.
    started: u128,
}

impl Shared {
    pub fn state(&self) -> MutexGuard<'_, State> {
        // A poisoned lock only means a panic elsewhere; the data is still usable.
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn options(&self, config: &PluginConfig) -> Result<Options> {
        Options::read(config, &self.default_directory)
    }

    pub fn new_id(&self, prefix: &str) -> String {
        let mut state = self.state();
        state.next_id += 1;
        format!("{prefix}-{:x}-{}", self.started, state.next_id)
    }
}

/// Publish a record. A record that cannot be published (the GDS is stopping) is counted.
pub(crate) async fn publish(ctx: &Context, record: NewRecord) {
    if ctx.records.publish(record).await.is_err() {
        ctx.status.count("records_not_published", 1);
    }
}

struct FileDecoder;

impl Decoder for FileDecoder {
    fn matches(&self) -> PacketMatch {
        PacketMatch { packet_type: Some(PacketType::FILE), ..Default::default() }
    }

    fn decode(&self, bytes: &[u8], _: &PacketMeta, dict: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        let file_bytes = bytes.get(packet::descriptor_bytes(dict)..).unwrap_or_default();
        let (seq, file_packet) = packet::decode(file_bytes).map_err(DecodeError)?;
        // The handler decodes `raw` again; the body is for a client that watches the packets.
        let body = json!({"type": file_packet.name(), "seq": seq});
        Ok(vec![NewRecord { raw: Bytes::copy_from_slice(file_bytes), ..NewRecord::new(KIND_PACKET, body) }])
    }
}

static UI: StaticFiles = StaticFiles(&[
    ("uplink.js", include_bytes!("../ui/uplink.js")),
    ("downlink.js", include_bytes!("../ui/downlink.js")),
    ("common.js", include_bytes!("../ui/common.js")),
    ("lit-core.min.js", include_bytes!("../ui/lit-core.min.js")),
]);

impl Plugin for FilesPlugin {
    fn manifest(&self) -> Manifest {
        let option = |name, help, default| OptionSpec { name, help, default, secret: false };
        Manifest {
            name: NAME,
            version: env!("CARGO_PKG_VERSION"),
            options: vec![
                option(
                    "directory",
                    "Directory of uplink and downlink files (default: <user>/fprime-uplink and <user>/fprime-downlink in the temporary directory, as the Python GDS)",
                    None,
                ),
                option("chunk-size", "Bytes of file data per uplink packet", Some("256")),
                option("cooldown", "Seconds between two uplink packets", Some("0.01")),
                option("timeout", "Seconds without progress after which a transfer fails", Some("20")),
                option("max-upload-bytes", "Largest file accepted for uplink", Some("67108864")),
            ],
            ..Default::default()
        }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        const FIELDS: &[FieldSpec] = &[
            FieldSpec { name: "state", pointer: "/state", ty: FieldType::Text },
            FieldSpec { name: "name", pointer: "/name", ty: FieldType::Text },
        ];
        reg.add_kind(KindSpec {
            transient: true,
            ..KindSpec::new(KIND_PACKET, "A file packet from the flight software")
        })?;
        for (kind, description) in [
            (KIND_UPLINK, "State and progress of a file uplink"),
            (KIND_DOWNLINK, "State and progress of a file downlink"),
        ] {
            reg.add_kind(KindSpec {
                fields: FIELDS,
                text_fields: &["/name", "/source", "/destination", "/reason"],
                has_entities: true,
                ..KindSpec::new(kind, description)
            })?;
        }
        reg.add_kind(KindSpec {
            text_fields: &["/reason", "/name"],
            ..KindSpec::new(KIND_PROBLEM, "A file packet that does not fit the transfer in progress")
        })?;
        reg.add_decoder(Arc::new(|_| Ok(Box::new(FileDecoder))))?;

        let started = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
        let shared = Arc::new(Shared {
            state: Mutex::new(State::default()),
            wake: Notify::new(),
            default_directory: self.directory.clone(),
            started,
        });
        let for_handler = shared.clone();
        reg.add_handler(Arc::new(move |config| {
            let options = for_handler.options(config)?;
            Ok(Box::new(uplink::UplinkHandler { shared: for_handler.clone(), options, unconfirmed: Vec::new() }))
        }))?;
        let for_handler = shared.clone();
        reg.add_handler(Arc::new(move |config| {
            let options = for_handler.options(config)?;
            Ok(Box::new(downlink::DownlinkHandler::new(for_handler.clone(), options)))
        }))?;
        reg.add_routes(routes::routes(shared))?;
        reg.add_tab(UiBundle { tab_id: "uplink", title: Some("Uplink"), order: 40, module: "uplink.js", files: &UI })?;
        reg.add_tab(UiBundle {
            tab_id: "downlink",
            title: Some("Downlink"),
            order: 50,
            module: "downlink.js",
            files: &UI,
        })?;
        reg.add_subcommand(Arc::new(cli::FileUplink))?;
        reg.add_subcommand(Arc::new(cli::Files))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_name_is_one_path_component_inside_the_directory() {
        let directory = std::path::Path::new("/store/uplink");
        for (name, base) in [
            ("../../etc/passwd", "passwd"),
            ("..\\..\\boot.ini", "boot.ini"),
            ("/etc/passwd", "passwd"),
            ("C:\\dir\\a.bin", "a.bin"),
            ("a/b\\c", "c"),
            ("name with space.txt", "name with space.txt"),
        ] {
            assert_eq!(super::base_name(name), Ok(base));
            assert_eq!(super::path_inside(directory, name), Ok(directory.join(base)));
        }
        let long = "n".repeat(5000);
        assert_eq!(super::path_inside(directory, &long), Ok(directory.join(&long)));
        for name in ["", ".", "..", "a/..", "a\\..", "dir/", "/", "a\0b", "a/\0", "a\nb", "\u{7f}", "\u{85}"] {
            assert!(super::base_name(name).is_err(), "{name:?}");
            assert!(super::path_inside(directory, name).is_err(), "{name:?}");
        }
    }

    #[test]
    fn a_flight_path_is_sent_as_given_or_refused() {
        let at_limit = format!("{}a", "\u{e9}".repeat(127));
        for path in ["a.bin", "/dir/a.bin", "name with space.txt", "\u{fc}ni-\u{e9}.txt", "../a", "a\\..", &at_limit] {
            assert_eq!(super::flight_path("destination", path), Ok(()), "{path:?}");
            assert_eq!(super::flight_path_notice(path).is_some(), !path.is_ascii());
        }
        for (path, message) in [
            ("", "the destination path is empty"),
            ("a\0", "the destination path holds a control character"),
            ("a\u{1f}b", "the destination path holds a control character"),
            ("\u{7f}", "the destination path holds a control character"),
            (".", "the destination path names no file (it ends in \".\")"),
            ("dir/..", "the destination path names no file (it ends in \"..\")"),
            ("dir/", "the destination path names no file (it ends in \"\")"),
            (
                &"\u{e9}".repeat(128),
                "the destination path is 256 bytes long in UTF-8; the file packet carries at most 255",
            ),
        ] {
            assert_eq!(super::flight_path("destination", path), Err(message.to_string()));
        }
    }

    #[test]
    fn destination_as_python() {
        // (destination, directory, result): `os.path.join` of the Python uplinker.
        for (destination, directory, expected) in [
            (None, None, "/a.bin"),
            (None, Some(""), "a.bin"),
            (None, Some("."), "./a.bin"),
            (None, Some("/"), "/a.bin"),
            (None, Some("dir"), "dir/a.bin"),
            (None, Some("dir/"), "dir/a.bin"),
            (None, Some("dir//"), "dir//a.bin"),
            (Some("x/y.bin"), Some("dir"), "x/y.bin"),
            (Some(""), None, "/a.bin"),
        ] {
            assert_eq!(super::uplink_destination(destination, directory, "a.bin"), expected);
        }
    }
}
