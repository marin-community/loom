//! Text rendering for the runtime settings table.

use crate::dto::SettingsEnvelope;
use crate::operations::settings;
use crate::operations::{NoView, Render};

impl Render for settings::get::Op {
    // Each row carries the layer its value came from, so a value that looks
    // wrong can be traced to the built-in default, the deployment manifest, or
    // a runtime override without a second command.
    fn text(output: &SettingsEnvelope, _: &NoView) -> String {
        if output.settings.is_empty() {
            return "(no settings)".to_string();
        }
        output
            .settings
            .iter()
            .map(|setting| format!("{} = {}  ({})", setting.key, setting.value, setting.source))
            .collect::<Vec<_>>()
            .join("\n")
    }
}
