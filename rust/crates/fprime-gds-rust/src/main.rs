//! `fprime-gds-rust` — interactive ground system for F´ deployments.
//!
//! Sub-commands:
//!
//! * `run` (default) — open the TCP port, start framer/deframer, drop into a
//!   REPL for commanding while events and telemetry stream in.
//! * `dict-info` — summarize a dictionary file without connecting.
//! * `frame-test` — round-trip a hex-encoded payload through framer/deframer
//!   (handy for debugging at the wire level).

#![deny(rust_2018_idioms)]

mod repl;

use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand};
use fprime_dict::Dictionary;
use tracing_subscriber::EnvFilter;

#[derive(Parser, Debug)]
#[command(
    name = "fprime-gds-rust",
    version,
    about = "F´ ground data system (Rust)"
)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,

    /// Shortcut: with no subcommand, behave like `run`.  These flags are mirrored
    /// onto `RunArgs`.
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Connect to FSW and open the interactive REPL (default).
    Run(RunArgs),
    /// Summarize a dictionary file.
    DictInfo(DictArgs),
    /// Round-trip hex bytes through the framer.
    FrameTest(FrameArgs),
}

#[derive(Parser, Debug, Clone)]
struct RunArgs {
    /// Path to the F´ JSON topology dictionary.
    #[arg(long, short = 'd', global = true)]
    dictionary: Option<PathBuf>,

    /// TCP host (interface to bind in server mode, address to dial in client mode).
    #[arg(long, default_value = "0.0.0.0", global = true)]
    host: String,

    /// TCP port.
    #[arg(long, default_value_t = 50000, global = true)]
    port: u16,

    /// Connect to FSW instead of listening for it.
    #[arg(long, global = true)]
    connect: bool,
}

#[derive(Parser, Debug, Clone)]
struct DictArgs {
    /// Path to the F´ JSON topology dictionary.
    dictionary: PathBuf,
}

#[derive(Parser, Debug, Clone)]
struct FrameArgs {
    /// Hex-encoded payload (no spaces, no `0x`).
    hex: String,
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    init_tracing();
    let cli = Cli::parse();

    let cmd = cli.cmd.unwrap_or(Cmd::Run(cli.run));
    match cmd {
        Cmd::Run(args) => run(args).await,
        Cmd::DictInfo(args) => dict_info(args),
        Cmd::FrameTest(args) => frame_test(args),
    }
}

fn init_tracing() {
    let filter = EnvFilter::try_from_env("FPRIME_GDS_LOG")
        .unwrap_or_else(|_| EnvFilter::new("info,fprime_comm=info,fprime_frame=info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_target(false)
        .with_writer(std::io::stderr)
        .try_init();
}

async fn run(args: RunArgs) -> anyhow::Result<()> {
    let dict = match args.dictionary.as_ref() {
        Some(path) => {
            let d = Dictionary::from_path(path)?;
            tracing::info!(
                commands = d.commands_by_opcode.len(),
                events = d.events_by_id.len(),
                channels = d.channels_by_id.len(),
                "loaded dictionary {}",
                path.display(),
            );
            d
        }
        None => {
            tracing::warn!(
                "no --dictionary supplied; downlink will be displayed as raw descriptors and \
                 commands cannot be encoded by name"
            );
            Dictionary::default()
        }
    };

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    let mode = if args.connect {
        fprime_comm::Mode::Client
    } else {
        fprime_comm::Mode::Server
    };
    let comm = fprime_comm::spawn(addr, mode);

    repl::run(dict, comm).await
}

fn dict_info(args: DictArgs) -> anyhow::Result<()> {
    let dict = Dictionary::from_path(&args.dictionary)?;
    println!("Dictionary: {}", args.dictionary.display());
    println!("  commands: {}", dict.commands_by_opcode.len());
    println!("  events:   {}", dict.events_by_id.len());
    println!("  channels: {}", dict.channels_by_id.len());

    let mut cmds: Vec<_> = dict.commands_by_opcode.values().collect();
    cmds.sort_by_key(|c| c.opcode);
    println!("\nCommands (first 20):");
    for c in cmds.iter().take(20) {
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
    Ok(())
}

fn frame_test(args: FrameArgs) -> anyhow::Result<()> {
    let payload = hex::decode(args.hex.replace(' ', "")).map_err(anyhow::Error::from)?;
    let framed = fprime_frame::frame(&payload)?;
    println!("framed ({} bytes):", framed.len());
    println!("  {}", hex::encode(&framed));
    let r = fprime_frame::deframe(&framed);
    println!(
        "deframe: consumed={} discarded={} payload={:?}",
        r.consumed,
        r.discarded.len(),
        r.frame.as_ref().map(hex::encode)
    );
    Ok(())
}
