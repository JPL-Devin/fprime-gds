//! Conflict checks at registration (CUS-10): the error names both plugins.

use fgds_core::testing::{empty_dictionary, FakeGds};
use fgds_core::*;
use std::sync::Arc;

struct NullDecoder(PacketMatch);

impl Decoder for NullDecoder {
    fn matches(&self) -> PacketMatch {
        self.0.clone()
    }
    fn decode(&self, _: &[u8], _: &PacketMeta, _: &Dictionary) -> Result<Vec<NewRecord>, DecodeError> {
        Ok(vec![NewRecord::new("t.kind", serde_json::json!({"rule": format!("{:?}", self.0)}))])
    }
}

struct Sub(&'static str);

impl Subcommand for Sub {
    fn command(&self) -> clap::Command {
        clap::Command::new(self.0)
    }
    fn run(&self, _: &clap::ArgMatches, _: &CliEnv) -> Result<std::process::ExitCode> {
        Ok(std::process::ExitCode::SUCCESS)
    }
}

static FILES: StaticFiles = StaticFiles(&[]);

type Register = fn(&mut Registry) -> Result<()>;

/// A plugin that registers whatever the test gives it.
struct P(&'static str, Register);

impl Plugin for P {
    fn manifest(&self) -> Manifest {
        let option = OptionSpec { name: "shared-option", help: "", default: None, secret: false };
        let options = if self.0.starts_with("opt") { vec![option] } else { vec![] };
        Manifest { name: self.0, version: "1", options, ..Default::default() }
    }
    fn register(&self, reg: &mut Registry) -> Result<()> {
        (self.1)(reg)
    }
}

fn rule(packet_type: Option<u32>, meta: &[(&'static str, u64)]) -> PacketMatch {
    PacketMatch {
        packet_type: packet_type.map(PacketType),
        meta: meta.iter().map(|(k, v)| (*k, MetaValue::U64(*v))).collect(),
    }
}

fn conflict(first: P, second: P) -> String {
    let mut reg = Registry::new();
    reg.register(&first).unwrap();
    match reg.register(&second) {
        Err(Error::Conflict(message)) => message,
        other => panic!("expected a conflict, got {:?}", other.err()),
    }
}

#[test]
fn duplicate_registrations_fail_naming_both_plugins() {
    let nothing: Register = |_| Ok(());
    let cases: [(&str, Register); 8] = [
        ("record kind \"t.kind\"", |r| r.add_kind(KindSpec::new("t.kind", ""))),
        ("route prefix \"things\"", |r| {
            r.add_routes(Routes { prefix: "things", router: axum::Router::new(), access: vec![] })
        }),
        ("tab \"things\"", |r| {
            r.add_tab(UiBundle { tab_id: "things", title: "", order: 0, module: "m.js", files: &FILES })
        }),
        ("subcommand \"thing\"", |r| r.add_subcommand(Arc::new(Sub("thing")))),
        ("framing layer \"l\"", |r| r.add_framing_layer("l", Arc::new(testing::LengthFraming))),
        ("framing profile \"p\"", |r| r.add_framing_profile("p", &[])),
        ("storage \"s\"", |r| r.add_storage("s", Arc::new(testing::MemoryStorage::default()))),
        ("decoder match rule", |r| r.add_decoder(Arc::new(NullDecoder(rule(Some(2), &[]))))),
    ];
    for (what, register) in cases {
        let message = conflict(P("first", register), P("second", register));
        assert!(message.contains(what), "{message}");
        assert!(message.contains("plugin \"first\" and plugin \"second\""), "{message}");
    }
    // The core's own kinds are protected the same way.
    let core_kind: Register = |r| r.add_kind(KindSpec::new("gds.plugin_fault", ""));
    let message = conflict(P("first", nothing), P("second", core_kind));
    assert!(message.contains("plugin \"core\" and plugin \"second\""), "{message}");
    assert!(conflict(P("same", nothing), P("same", nothing)).starts_with("plugin \"same\""));
    assert!(conflict(P("opt-a", nothing), P("opt-b", nothing)).starts_with("argument \"shared-option\""));
}

#[test]
fn decoder_rules_conflict_only_when_equally_specific_and_overlapping() {
    let add = |rules: &[PacketMatch]| {
        let mut reg = Registry::new();
        rules.iter().try_for_each(|r| reg.add_decoder(Arc::new(NullDecoder(r.clone()))))
    };
    // Different packet types, or different values of one metadata key, never overlap.
    assert!(add(&[rule(Some(1), &[]), rule(Some(2), &[])]).is_ok());
    assert!(add(&[rule(None, &[("apid", 1)]), rule(None, &[("apid", 2)])]).is_ok());
    // A metadata rule takes precedence over a packet type alone.
    assert!(add(&[rule(Some(1), &[]), rule(Some(1), &[("apid", 1)]), rule(None, &[])]).is_ok());
    // Same precedence and one packet can match both.
    assert!(add(&[rule(None, &[("apid", 1)]), rule(None, &[("vcid", 1)])]).is_err());
    assert!(add(&[rule(Some(1), &[("apid", 1)]), rule(Some(1), &[("apid", 1)])]).is_err());
}

#[test]
fn override_decoder_replaces_the_decoder_with_the_same_rule() {
    let mut reg = Registry::new();
    reg.add_decoder(Arc::new(NullDecoder(rule(Some(1), &[])))).unwrap();
    assert!(reg.override_decoder(Arc::new(NullDecoder(rule(Some(1), &[])))).is_ok());
    assert_eq!(reg.decoders.len(), 1);
    assert!(reg.override_decoder(Arc::new(NullDecoder(rule(Some(3), &[])))).is_err());
}

#[tokio::test]
async fn the_most_specific_rule_receives_the_packet() {
    let register: Register = |r| {
        r.add_kind(KindSpec::new("t.kind", ""))?;
        r.add_decoder(Arc::new(NullDecoder(rule(None, &[]))))?;
        r.add_decoder(Arc::new(NullDecoder(rule(Some(1), &[]))))
    };
    let gds = FakeGds::start(&[&P("p", register)], empty_dictionary()).await.unwrap();
    gds.receive(&[0, 1]).await;
    gds.receive(&[0, 2]).await;
    let records = gds.records(&["t.kind"]).await;
    assert!(records[0].body["rule"].as_str().unwrap().contains("Some(PacketType(1))"));
    assert!(records[1].body["rule"].as_str().unwrap().contains("packet_type: None"));
}

#[test]
fn missing_requirements_fail_the_check() {
    struct Needy;
    impl Plugin for Needy {
        fn manifest(&self) -> Manifest {
            Manifest { name: "needy", version: "1", requires_kinds: vec!["x.absent"], ..Default::default() }
        }
        fn register(&self, reg: &mut Registry) -> Result<()> {
            reg.add_framing_profile("broken", &["absent-layer"])
        }
    }
    let mut reg = Registry::new();
    reg.register(&Needy).unwrap();
    assert!(reg.check().unwrap_err().to_string().contains("x.absent"));
}
