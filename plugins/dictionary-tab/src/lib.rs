//! The Dictionaries tab. It has no route of its own: the tab reads `GET /api/v1/dictionary`.

use fgds_core::{Manifest, Plugin, Registry, Result, StaticFiles, UiBundle};

static FILES: StaticFiles = StaticFiles(&[("dictionary.js", include_bytes!("../ui/dictionary.js"))]);

pub struct DictionaryTabPlugin;

impl Plugin for DictionaryTabPlugin {
    fn manifest(&self) -> Manifest {
        Manifest { name: "dictionary-tab", version: env!("CARGO_PKG_VERSION"), ..Default::default() }
    }

    fn register(&self, reg: &mut Registry) -> Result<()> {
        reg.add_tab(UiBundle {
            tab_id: "dictionary",
            title: Some("Dictionaries"),
            order: 40,
            module: "dictionary.js",
            files: &FILES,
        })
    }
}
