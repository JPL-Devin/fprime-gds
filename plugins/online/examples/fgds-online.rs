//! The stock GDS with the online subcommands, for the checks of `tests/cli` until
//! `OnlinePlugin` is part of the default plugins.

fn main() -> std::process::ExitCode {
    fgds::Gds::builder().with_defaults().with(online::OnlinePlugin).run()
}
