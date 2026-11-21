//! A project's GDS: the stock one without the channels plugin, with the project's plugins.

fn main() -> std::process::ExitCode {
    custom_gds::gds().run()
}
