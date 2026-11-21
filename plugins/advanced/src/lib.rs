//! The Advanced tab. It has no route of its own: the tab reads the status, version, kind
//! and run routes of the core API, and keeps its settings in the browser.

use fgds_core::{Manifest, Plugin, Registry, Result, StaticFiles, UiBundle};

static FILES: StaticFiles = StaticFiles(&[("advanced.js", include_bytes!("../ui/advanced.js"))]);

pub struct AdvancedPlugin;

impl Plugin for AdvancedPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "advanced", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_tab(UiBundle {
            tab_id: "advanced",
            title: Some("Advanced"),
            order: 100,
            module: "advanced.js",
            files: &FILES,
        })
    }
}
