//! The stock GDS: every default plugin, nothing else.

fn main() -> std::process::ExitCode {
    fgds::Gds::builder().with_defaults().run()
}
