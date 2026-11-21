//! Two plugins registering the same record kind: this binary refuses to start,
//! whatever the command, and names both plugins.

fn main() -> std::process::ExitCode {
    custom_gds::gds().with(custom_gds::ConflictingPlugin).run()
}
