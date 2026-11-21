//! The stock binary's commands that end by themselves; `run` is covered by `scripts/check.sh`.

use std::process::{Command, Output};

const DICTIONARY: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/data/dictionary.json");

fn fgds(args: &[&str], dir: &std::path::Path) -> (Output, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_fgds")).args(args).current_dir(dir).output().unwrap();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    (output.clone(), text(&output.stdout), text(&output.stderr))
}

fn scratch(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("fgds-cli-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn check_prints_what_would_start_and_command_line_wins_over_the_file() {
    let dir = scratch("check");
    let config = format!(
        "dictionary = \"{DICTIONARY}\"\ncomm = \"tcp-client\"\nbind = \"127.0.0.1:1\"\n\
         [plugins.comm-tcp]\ntcp-address = \"10.0.0.1:7\"\n"
    );
    std::fs::write(dir.join("fgds.toml"), config).unwrap();
    let (output, stdout, stderr) = fgds(&["check", "--bind", "127.0.0.1:2"], &dir);
    assert!(output.status.success(), "{stderr}");
    assert!(stderr.contains("configuration: fgds.toml"));
    for line in [
        "2 commands, 2 events, 3 channels",
        "adapter: tcp-client",
        "framing: fprime",
        "server: http://127.0.0.1:2",
        "--tcp-address = 10.0.0.1:7",
        "record kinds: raw.discarded, gds.decode_error, gds.unknown_packet, gds.plugin_fault, gds.storage_gap, gds.storage_recovered",
    ] {
        assert!(stdout.contains(line), "missing \"{line}\" in:\n{stdout}");
    }
}

#[test]
fn a_plugin_can_be_disabled_in_the_configuration() {
    let dir = scratch("disabled");
    std::fs::write(dir.join("off.toml"), "disabled_plugins = [\"events\"]\n").unwrap();
    let (output, stdout, _) = fgds(&["check", "--config", "off.toml", "--dictionary", DICTIONARY], &dir);
    assert!(output.status.success());
    assert!(!stdout.contains("events 0.1.0") && stdout.contains("channels 0.1.0"));
}

#[test]
fn mistakes_stop_startup_with_a_message() {
    let dir = scratch("mistakes");
    std::fs::write(dir.join("bad.toml"), "colour = 1\n").unwrap();
    std::fs::write(dir.join("bad.json"), "{").unwrap();
    for (args, message) in [
        (&["check"][..], "no dictionary"),
        (&["check", "--dictionary", "nope.json"], "nope.json"),
        (&["check", "--dictionary", "bad.json"], "bad.json"),
        (
            &["check", "--dictionary", DICTIONARY, "--comm", "pigeon"],
            "unknown adapter \"pigeon\"; available: tcp-client, tcp-server",
        ),
        (&["check", "--dictionary", DICTIONARY, "--framing", "x"], "unknown framing profile \"x\""),
        (&["check", "--config", "bad.toml"], "colour"),
        (&["check", "--config", "missing.toml"], "missing.toml"),
    ] {
        let (output, _, stderr) = fgds(args, &dir);
        assert!(!output.status.success(), "{args:?}");
        assert!(stderr.contains(message), "{args:?}: {stderr}");
    }
}
