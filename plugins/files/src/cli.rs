//! `fgds files`: the file routes of a running GDS from the command line.

use fgds_core::clap::{Arg, ArgAction, ArgMatches, Command};
use fgds_core::serde_json::Value;
use fgds_core::{CliEnv, Error, Result, Subcommand};
use std::process::ExitCode;
use std::time::Duration;

pub(crate) struct Files;

impl Subcommand for Files {
    fn command(&self) -> Command {
        Command::new("files")
            .about("File uplink and downlink of a running GDS")
            .subcommand_required(true)
            .arg(
                Arg::new("url")
                    .long("url")
                    .global(true)
                    .value_name("URL")
                    .help("Address of the running GDS [default: the configured --bind]"),
            )
            .subcommand(
                Command::new("upload")
                    .about("Uplink a file and wait for the flight software to confirm it, as `fprime-cli file-uplink`")
                    .arg(Arg::new("file").required(true).help("File on this machine"))
                    .arg(Arg::new("destination").short('d').long("destination").help("Path on the spacecraft"))
                    .args(wait_args()),
            )
            .subcommand(Command::new("queue").about("Show the uplink queue"))
            .subcommand(Command::new("pause").about("Pause the uplink queue; the file being sent is canceled"))
            .subcommand(Command::new("resume").about("Resume the uplink queue"))
            .subcommand(
                Command::new("cancel")
                    .about("Cancel the file being sent or remove a queued file")
                    .arg(Arg::new("id").required(true).help("Identifier shown by `queue`")),
            )
            .subcommand(Command::new("list").about("List the downlinks of this run and the downlinked files"))
            .subcommand(
                Command::new("download")
                    .about("Copy a downlinked file to this machine")
                    .arg(Arg::new("name").required(true).help("Name shown by `list`"))
                    .arg(Arg::new("output").short('o').long("output").help("Where to write it (default: the name)")),
            )
    }

    fn honours(&self) -> &'static [&'static str] {
        &["bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let base = base(args, env)?;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)?;
        runtime.block_on(run(args, &base))
    }
}

/// The waiting options of `fprime-cli file-uplink`.
fn wait_args() -> [Arg; 2] {
    [
        Arg::new("timeout")
            .short('t')
            .long("timeout")
            .value_name("SECONDS")
            .value_parser(fgds_core::clap::value_parser!(f64))
            .default_value("60")
            .help("Maximum time to wait for the uplink to complete"),
        Arg::new("no-wait")
            .long("no-wait")
            .action(ArgAction::SetTrue)
            .help("Queue the file for uplink and return at once"),
    ]
}

/// `fgds file-uplink`: `files upload` with the destination as second argument.
pub(crate) struct FileUplink;

impl Subcommand for FileUplink {
    fn command(&self) -> Command {
        Command::new("file-uplink")
            .about("Uplink a file through a running GDS and wait for the flight software to confirm it")
            .arg(
                Arg::new("url")
                    .long("url")
                    .value_name("URL")
                    .help("Address of the running GDS [default: the configured --bind]"),
            )
            .arg(Arg::new("file").value_name("file-path").required(true).help("Path to the local file to uplink"))
            .arg(Arg::new("destination").help("Destination path on the spacecraft"))
            .args(wait_args())
            .arg(
                Arg::new("file-uplink-chunk-size")
                    .long("file-uplink-chunk-size")
                    .value_name("BYTES")
                    .value_parser(fgds_core::clap::value_parser!(u16).range(1..))
                    .help("Bytes of file data per packet of this uplink [default: the one of the running GDS]"),
            )
            .arg(
                Arg::new("file-uplink-cooldown")
                    .long("file-uplink-cooldown")
                    .value_name("SECONDS")
                    .value_parser(fgds_core::clap::value_parser!(f64))
                    .help("Seconds between two packets of this uplink [default: the one of the running GDS]"),
            )
    }

    fn honours(&self) -> &'static [&'static str] {
        &["bind"]
    }

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
        let base = base(args, env)?;
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)?;
        runtime.block_on(upload(args, &base))
    }
}

/// The address of the file routes: `--url` when given, the configured one otherwise.
fn base(args: &ArgMatches, env: &CliEnv) -> Result<String> {
    let base = match args.get_one::<String>("url") {
        Some(url) => {
            let url = url.trim_end_matches('/');
            let url = if url.contains("://") { url.to_string() } else { format!("http://{url}") };
            if url.ends_with("/api/v1") {
                url
            } else {
                format!("{url}/api/v1")
            }
        }
        None => (env.client)()?.base_url,
    };
    Ok(format!("{base}/files"))
}

/// Flight events that end the wait of `fprime-cli file-uplink` with a failure.
const FLIGHT_FAILURES: [&str; 7] = [
    "FileOpenError",
    "FileWriteError",
    "PacketOutOfBounds",
    "PacketOutOfOrder",
    "UplinkCanceled",
    "DecodeError",
    "InvalidReceiveMode",
];

/// What `fprime-cli file-uplink` does and prints: queue the file, wait until every packet is
/// sent, then for the event of the flight software that confirms or refuses the file.
async fn upload(args: &ArgMatches, base: &str) -> Result<ExitCode> {
    let http = reqwest::Client::new();
    let failed = |e: reqwest::Error| Error::other(format!("the GDS at {base} did not answer: {e}"));
    let get = |url: String| {
        let http = &http;
        async move {
            let response = http.get(url).send().await.map_err(failed)?;
            response.json::<Value>().await.map_err(failed)
        }
    };
    let file = std::path::Path::new(text(args, "file"));
    if !file.is_file() {
        fgds_core::out!("Error: File not found at {}", file.display());
        return Ok(ExitCode::FAILURE);
    }
    let content = std::fs::read(file).map_err(|e| Error::other(format!("{}: {e}", file.display())))?;
    let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let records = base.strip_suffix("/files").unwrap_or(base);
    // Read before the file is queued: the events of this uplink come after it.
    let cursor = get(format!("{records}/cursor")).await?;
    let mut after = cursor["seq"].as_str().unwrap_or("0").to_string();
    let seconds = args.get_one::<f64>("timeout").copied().unwrap_or(60.0);
    let deadline = tokio::time::Instant::now() + Duration::try_from_secs_f64(seconds).unwrap_or(Duration::ZERO);

    // Only the name goes to the GDS: the source path of the start packet is that name in its uplink directory.
    let mut url = format!("{base}/uplink?name={}", encode(&name));
    // Refused here as the GDS refuses it, before the file is sent to it.
    let Some(destination) = args.get_one::<String>("destination") else {
        fgds_core::out!("Error: no destination on the spacecraft is given for {name}");
        return Ok(ExitCode::FAILURE);
    };
    url += &format!("&destination={}", encode(destination));
    if let Some(chunk) = args.try_get_one::<u16>("file-uplink-chunk-size").ok().flatten() {
        url += &format!("&chunk={chunk}");
    }
    if let Some(cooldown) = args.try_get_one::<f64>("file-uplink-cooldown").ok().flatten() {
        if !(cooldown.is_finite() && *cooldown >= 0.0) {
            fgds_core::out!("Error starting file uplink: --file-uplink-cooldown: a number of seconds, not negative");
            return Ok(ExitCode::FAILURE);
        }
        url += &format!("&cooldown={cooldown}");
    }
    let response = http.post(url).body(content).send().await.map_err(failed)?;
    let ok = response.status().is_success();
    let json: Value = response.json().await.unwrap_or(Value::Null);
    if !ok {
        fgds_core::out!(
            "Error starting file uplink: {}",
            json["error"]["message"].as_str().unwrap_or("the request failed")
        );
        return Ok(ExitCode::FAILURE);
    }
    // The path that is sent.
    let shown = json["destination"].as_str().unwrap_or_default().to_string();
    fgds_core::out!("Queued '{}' for uplink to '{shown}'", file.display());
    if let Some(notice) = json["notice"].as_str() {
        fgds_core::out!("Note: {notice}");
    }
    if args.get_flag("no-wait") {
        return Ok(ExitCode::SUCCESS);
    }

    let id = json["id"].as_str().unwrap_or_default().to_string();
    let mut last = None;
    loop {
        if tokio::time::Instant::now() >= deadline {
            fgds_core::out!("Error: uplink of '{name}' did not complete within {seconds:?} seconds");
            return Ok(ExitCode::FAILURE);
        }
        let queue = get(format!("{base}/uplink")).await?;
        let Some(entry) = queue["files"].as_array().into_iter().flatten().find(|t| t["id"] == id.as_str()) else {
            fgds_core::out!("Error: '{name}' is no longer tracked by the uplinker");
            return Ok(ExitCode::FAILURE);
        };
        let percent = entry["done"].as_u64().unwrap_or(0) * 100 / entry["size"].as_u64().unwrap_or(0).max(1);
        match entry["state"].as_str().unwrap_or_default() {
            "transferring" if last != Some(percent) => {
                fgds_core::out!("Uplinking '{name}': {percent}%");
                last = Some(percent);
            }
            "finished" => {
                fgds_core::out!(
                    "All uplink packets sent for '{name}' to '{}'",
                    entry["destination"].as_str().unwrap_or("-")
                );
                break;
            }
            // Every packet is sent and the flight software said nothing about the file.
            "sent" => {
                fgds_core::out!("Error: no 'FileReceived' event received for '{name}'; uplink unconfirmed");
                return Ok(ExitCode::FAILURE);
            }
            state @ ("failed" | "canceled") => {
                match entry["reason"].as_str() {
                    Some(reason) => fgds_core::out!("Uplink of '{name}' {state}: {}", fgds_core::visible(reason)),
                    None => fgds_core::out!("Uplink of '{name}' {state}"),
                }
                return Ok(ExitCode::FAILURE);
            }
            _ => {}
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    let ends = |record: &Value, names: &[&str]| {
        let name = record["body"]["name"].as_str().unwrap_or_default();
        names.iter().any(|n| name.ends_with(&format!(".{n}")))
    };
    // At least one second is given to the event, as `fprime-cli` does.
    let deadline = deadline.max(tokio::time::Instant::now() + Duration::from_secs(1));
    loop {
        let page = get(format!("{records}/records?kind=fprime.event&after_seq={after}")).await?;
        for record in page["records"].as_array().into_iter().flatten() {
            let line = || fgds_core::visible(&fgds_core::python_event_record_line(record)).into_owned();
            if ends(record, &["FileReceived"]) {
                fgds_core::out!("Flight software confirmed receipt: {}", line());
                return Ok(ExitCode::SUCCESS);
            }
            if ends(record, &FLIGHT_FAILURES) {
                fgds_core::out!("Uplink failed on the flight side: {}", line());
                return Ok(ExitCode::FAILURE);
            }
            after = record["seq"].as_str().unwrap_or(&after).to_string();
        }
        match page["next"].as_str() {
            Some(next) => after = next.to_string(),
            None if tokio::time::Instant::now() >= deadline => break,
            None => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
    fgds_core::out!("Error: no 'FileReceived' event received before the timeout; uplink unconfirmed");
    Ok(ExitCode::FAILURE)
}

fn text<'a>(args: &'a ArgMatches, name: &str) -> &'a str {
    args.get_one::<String>(name).map(String::as_str).unwrap_or_default()
}

/// Percent-encode a path segment or a query value.
fn encode(value: &str) -> String {
    value
        .bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || b"._-~".contains(&b) {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}

async fn run(args: &ArgMatches, base: &str) -> Result<ExitCode> {
    let http = reqwest::Client::new();
    let (name, sub) = args.subcommand().expect("a subcommand is required");
    if name == "upload" {
        return upload(sub, base).await;
    }
    let request = match name {
        "queue" => http.get(format!("{base}/uplink")),
        "pause" | "resume" => http.post(format!("{base}/uplink/{name}")),
        "cancel" => http.post(format!("{base}/uplink/{}/cancel", encode(text(sub, "id")))),
        "list" => http.get(format!("{base}/downlink")),
        "download" => http.get(format!("{base}/downlink/{}", encode(text(sub, "name")))),
        _ => unreachable!("clap accepts the subcommands above only"),
    };
    let failed = |e: reqwest::Error| Error::other(format!("the GDS at {base} did not answer: {e}"));
    let response = request.send().await.map_err(failed)?;
    let ok = response.status().is_success();
    let body = response.bytes().await.map_err(failed)?;
    if ok && name == "download" {
        // The output is a path of this machine chosen by the operator; the default is a base name.
        let default = crate::base_name(text(sub, "name")).map_err(Error::other)?;
        let output = sub.get_one::<String>("output").map_or(default, String::as_str);
        std::fs::write(output, &body).map_err(|e| Error::other(format!("{output}: {e}")))?;
        fgds_core::out!("{output}: {} bytes", body.len());
        return Ok(ExitCode::SUCCESS);
    }
    let json: Value = fgds_core::serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !ok {
        fgds_core::err!("error: {}", json["error"]["message"].as_str().unwrap_or("the request failed"));
        return Ok(ExitCode::FAILURE);
    }
    let rows = |list: &Value| {
        for t in list.as_array().into_iter().flatten() {
            let reason = t["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
            fgds_core::out!(
                "{}  {}  {}/{} bytes  {} -> {}  {}{reason}",
                t["id"].as_str().unwrap_or("-"),
                t["state"].as_str().unwrap_or("-"),
                t["done"],
                t["size"],
                t["source"].as_str().unwrap_or("-"),
                t["destination"].as_str().unwrap_or("-"),
                t["name"].as_str().unwrap_or("-"),
            );
        }
    };
    match name {
        "list" => {
            rows(&json["transfers"]);
            for file in json["files"].as_array().into_iter().flatten() {
                fgds_core::out!("{}  {} bytes", file["name"].as_str().unwrap_or("-"), file["size"]);
            }
        }
        _ => {
            let paused = if json["paused"] == true { "paused" } else { "running" };
            fgds_core::out!("queue: {paused}, link: {}", json["link"].as_str().unwrap_or("-"));
            rows(&json["files"]);
        }
    }
    Ok(ExitCode::SUCCESS)
}
