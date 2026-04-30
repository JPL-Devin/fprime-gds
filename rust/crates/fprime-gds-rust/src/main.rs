//! `fprime-gds-rust` — interactive ground system for F´ deployments.
//!
//! Sub-commands:
//!
//! * `run` (default) — open the TCP port, start framer/deframer, drop into a
//!   REPL for commanding while events and telemetry stream in.
//! * `dict-info` — summarize a dictionary file without connecting.
//! * `frame-test` — round-trip a hex-encoded payload through framer/deframer
//!   (handy for debugging at the wire level).
//! * `dp-decode` — decode a data product (`.fdp`) binary file to JSON,
//!   matching the Python GDS' `data_products` tool.

#![deny(rust_2018_idioms)]

mod repl;

use std::{net::SocketAddr, path::PathBuf};

use clap::{Parser, Subcommand, ValueEnum};
use fprime_ccsds::{
    space_data_link::{FALLBACK_FRAME_SIZE, FALLBACK_SCID},
    ChainedDeframer, ChainedFramer, SpacePacketDeframer, SpacePacketFramer, TcFramer, TmDeframer,
};
use fprime_comm::{shared_deframer, shared_framer, SharedDeframer, SharedFramer};
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
    /// Decode a data product (.fdp) binary file to JSON.
    DpDecode(DpDecodeArgs),
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

    /// Wire protocol.
    #[arg(long, value_enum, default_value_t = Protocol::Fprime, global = true)]
    protocol: Protocol,

    /// CCSDS spacecraft id (10-bit).  Only used when --protocol selects a CCSDS variant.
    #[arg(long, global = true, value_parser = parse_u16_auto)]
    scid: Option<u16>,

    /// CCSDS virtual channel id (6-bit, only the low 3 bits matter for TM framing).
    #[arg(long, default_value_t = 1, global = true)]
    vcid: u8,

    /// Fixed CCSDS TM frame size in bytes.
    #[arg(long, global = true)]
    frame_size: Option<usize>,
}

/// Wire protocol selection for the comm layer.
#[derive(Copy, Clone, Debug, PartialEq, Eq, ValueEnum)]
#[value(rename_all = "kebab-case")]
enum Protocol {
    /// F´ DEADBEEF + length + CRC32 (default; matches `FpFramerDeframer`).
    Fprime,
    /// CCSDS Space Packet only (no transfer-frame layer).  Matches the
    /// Python `raw-space-packet` plugin.
    CcsdsSpacePacket,
    /// CCSDS Space Data Link (TC uplink + TM downlink) only.  Matches
    /// `raw-space-data-link`.
    CcsdsSpaceDataLink,
    /// CCSDS Space Packet inside CCSDS Space Data Link (TC/TM).  Matches
    /// `space-packet-space-data-link` — the chained framer/deframer.
    Ccsds,
}

fn parse_u16_auto(s: &str) -> Result<u16, String> {
    let (radix, body) = if let Some(rest) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        (16, rest)
    } else if let Some(rest) = s.strip_prefix("0o").or_else(|| s.strip_prefix("0O")) {
        (8, rest)
    } else if let Some(rest) = s.strip_prefix("0b").or_else(|| s.strip_prefix("0B")) {
        (2, rest)
    } else {
        (10, s)
    };
    u16::from_str_radix(body, radix).map_err(|e| e.to_string())
}

fn build_pair(args: &RunArgs) -> anyhow::Result<(SharedFramer, SharedDeframer)> {
    let scid = args.scid.unwrap_or(FALLBACK_SCID);
    let vcid = args.vcid;
    let frame_size = args.frame_size.unwrap_or(FALLBACK_FRAME_SIZE);
    Ok(match args.protocol {
        Protocol::Fprime => fprime_comm::fprime_pair(),
        Protocol::CcsdsSpacePacket => (
            shared_framer(SpacePacketFramer::new()),
            shared_deframer(SpacePacketDeframer::new()),
        ),
        Protocol::CcsdsSpaceDataLink => (
            shared_framer(TcFramer::new(scid, vcid)?),
            shared_deframer(TmDeframer::new(scid, vcid, frame_size)?),
        ),
        Protocol::Ccsds => {
            let inner_framer: Box<dyn fprime_frame::Framer> = Box::new(SpacePacketFramer::new());
            let outer_framer: Box<dyn fprime_frame::Framer> = Box::new(TcFramer::new(scid, vcid)?);
            let outer_deframer: Box<dyn fprime_frame::Deframer> =
                Box::new(TmDeframer::new(scid, vcid, frame_size)?);
            let inner_deframer: Box<dyn fprime_frame::Deframer> =
                Box::new(SpacePacketDeframer::new());
            (
                shared_framer(ChainedFramer::new(inner_framer, outer_framer)),
                shared_deframer(ChainedDeframer::new(outer_deframer, inner_deframer)),
            )
        }
    })
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

#[derive(Parser, Debug, Clone)]
struct DpDecodeArgs {
    /// Path to the data-product binary file (`.fdp`).
    file: PathBuf,
    /// Path to the F´ JSON topology dictionary.
    #[arg(long, short = 'd')]
    dictionary: PathBuf,
    /// Output JSON path (defaults to `<file>.json`).  Use `-` to write to stdout.
    #[arg(long, short = 'o')]
    output: Option<PathBuf>,
    /// Pretty-print the JSON output.
    #[arg(long, default_value_t = true)]
    pretty: bool,
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
        Cmd::DpDecode(args) => dp_decode(args),
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
    let (framer, deframer) = build_pair(&args)?;
    tracing::info!(protocol = ?args.protocol, "comm protocol selected");
    let comm = fprime_comm::spawn(addr, mode, framer, deframer);

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

fn dp_decode(args: DpDecodeArgs) -> anyhow::Result<()> {
    let dict = Dictionary::from_path(&args.dictionary)?;
    let dp = fprime_dp::decode_path(&args.file, &dict)?;
    let json = fprime_dp::to_json(&dp);
    let text = if args.pretty {
        serde_json::to_string_pretty(&json)?
    } else {
        serde_json::to_string(&json)?
    };
    let output = args
        .output
        .unwrap_or_else(|| args.file.with_extension("json"));
    if output.as_os_str() == "-" {
        println!("{text}");
    } else {
        std::fs::write(&output, text)?;
        eprintln!(
            "decoded {} record(s) from {} -> {}",
            dp.records.len(),
            args.file.display(),
            output.display(),
        );
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
