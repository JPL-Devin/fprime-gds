//! The stock GDS with the CCSDS framing plugin: what `fgds` is once the plugin is in
//! its default list. Used by `tests/ccsds/run.sh`.

fn main() -> std::process::ExitCode {
    fgds::Gds::builder().with_defaults().with(framing_ccsds::CcsdsFramingPlugin).run()
}
