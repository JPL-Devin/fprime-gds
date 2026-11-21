//! The Charts tab. It has no route of its own: the tab reads the channel records.

use fgds_core::{Manifest, Plugin, Registry, Result, StaticFiles, UiBundle};

/// Chart.js 4.4.1 (MIT, `chart.umd.js` of the npm package, unchanged) is vendored: nothing is
/// fetched from elsewhere.
static FILES: StaticFiles = StaticFiles(&[
    ("charts.js", include_bytes!("../ui/charts.js")),
    ("chart.umd.js", include_bytes!("../ui/chart.umd.js")),
]);

pub struct ChartsPlugin;

impl Plugin for ChartsPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "charts", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_tab(UiBundle { tab_id: "charts", title: Some("Charts"), order: 70, module: "charts.js", files: &FILES })
    }
}
