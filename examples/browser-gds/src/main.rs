//! The GDS the browser tests run: the default plugins and, with `BROWSER_GDS_BROKEN_TAB` set, a tab that fails.

use fgds::core::{Manifest, Plugin, Registry, Result, StaticFiles, UiBundle};

static FILES: StaticFiles = StaticFiles(&[("broken.js", include_bytes!("../ui/broken.js"))]);

struct BrokenTabPlugin;

impl Plugin for BrokenTabPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "broken", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_tab(UiBundle { tab_id: "broken", title: Some("Broken"), order: 70, module: "broken.js", files: &FILES })
    }
}

fn main() -> std::process::ExitCode {
    let mut gds = fgds::Gds::builder().with_defaults();
    if std::env::var_os("BROWSER_GDS_BROKEN_TAB").is_some() {
        gds = gds.with(BrokenTabPlugin);
    }
    gds.run()
}
