//! The stock GDS with one more tab, which fails: the browser tests check that the page contains it.

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
    fgds::Gds::builder().with_defaults().with(BrokenTabPlugin).run()
}
