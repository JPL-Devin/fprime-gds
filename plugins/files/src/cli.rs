//! `fgds files`: the file routes of a running GDS from the command line.

use fgds_core::clap::{Arg, ArgMatches, Command};
use fgds_core::serde_json::Value;
use fgds_core::{CliEnv, Error, Result, Subcommand};
use std::process::ExitCode;

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
                    .about("Queue a file for uplink")
                    .arg(Arg::new("file").required(true).help("File on this machine"))
                    .arg(Arg::new("destination").short('d').long("destination").help("Path on the spacecraft")),
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

    fn run(&self, args: &ArgMatches, env: &CliEnv) -> Result<ExitCode> {
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
        let base = format!("{base}/files");
        let runtime = tokio::runtime::Builder::new_current_thread().enable_all().build().map_err(Error::Io)?;
        runtime.block_on(run(args, &base))
    }
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
    let request = match name {
        "upload" => {
            let file = std::path::Path::new(text(sub, "file"));
            if !file.is_file() {
                // The line and exit code of `fprime-cli file-uplink`.
                println!("Error: File not found at {}", file.display());
                return Ok(ExitCode::FAILURE);
            }
            let content = std::fs::read(file).map_err(|e| Error::other(format!("{}: {e}", file.display())))?;
            let name = file.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
            let mut url = format!("{base}/uplink?name={}", encode(&name));
            if let Some(destination) = sub.get_one::<String>("destination") {
                url += &format!("&destination={}", encode(destination));
            }
            http.post(url).body(content)
        }
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
        println!("{output}: {} bytes", body.len());
        return Ok(ExitCode::SUCCESS);
    }
    let json: Value = fgds_core::serde_json::from_slice(&body).unwrap_or(Value::Null);
    if !ok {
        eprintln!("error: {}", json["error"]["message"].as_str().unwrap_or("the request failed"));
        return Ok(ExitCode::FAILURE);
    }
    let rows = |list: &Value| {
        for t in list.as_array().into_iter().flatten() {
            let reason = t["reason"].as_str().map(|r| format!(" ({r})")).unwrap_or_default();
            println!(
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
        "upload" => {
            println!("{} queued as {}", json["name"].as_str().unwrap_or("-"), json["id"].as_str().unwrap_or("-"))
        }
        "list" => {
            rows(&json["transfers"]);
            for file in json["files"].as_array().into_iter().flatten() {
                println!("{}  {} bytes", file["name"].as_str().unwrap_or("-"), file["size"]);
            }
        }
        _ => {
            let paused = if json["paused"] == true { "paused" } else { "running" };
            println!("queue: {paused}, link: {}", json["link"].as_str().unwrap_or("-"));
            rows(&json["files"]);
        }
    }
    Ok(ExitCode::SUCCESS)
}
