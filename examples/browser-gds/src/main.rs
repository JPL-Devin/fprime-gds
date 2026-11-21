//! The GDS the browser tests run: the default plugins and the three tab plugins that are
//! not yet in the default list.

fn main() -> std::process::ExitCode {
    fgds::Gds::builder()
        .with_defaults()
        .with(dictionary_tab::DictionaryTabPlugin)
        .with(logs::LogsPlugin)
        .with(advanced::AdvancedPlugin)
        .run()
}
