//! The stock GDS with the data products plugin, for the checks of `tests/dp`: it is
//! the `fgds` binary once the plugin is in the default list.

use data_products::{DataProductsPlugin, PLUGIN};

fn main() -> std::process::ExitCode {
    let gds = fgds::Gds::builder().with_defaults();
    match fgds::default_plugins().iter().any(|plugin| plugin.manifest().name == PLUGIN) {
        true => gds.run(),
        false => gds.with(DataProductsPlugin).run(),
    }
}
