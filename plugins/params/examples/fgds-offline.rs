//! The stock `fgds` with the sequence and parameter plugins, for the checks of `tests/seq`
//! and as the example of adding them to a project binary.

fn main() -> std::process::ExitCode {
    let mut gds = fgds::Gds::builder().with_defaults();
    let has = |name: &str| fgds::default_plugins().iter().any(|p| p.manifest().name == name);
    if !has("sequences") {
        gds = gds.with(sequences::SequencesPlugin);
    }
    if !has("params") {
        gds = gds.with(params::ParamsPlugin);
    }
    gds.run()
}
