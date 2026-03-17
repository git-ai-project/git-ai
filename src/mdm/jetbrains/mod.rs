pub mod detection;
pub mod download;
pub mod ide_types;

pub use detection::{find_jetbrains_installations, is_plugin_installed};
pub use download::{
    download_plugin_from_marketplace, install_plugin_to_directory, install_plugin_via_cli,
};
pub use ide_types::{DetectedIde, MARKETPLACE_URL, MIN_INTELLIJ_BUILD, PLUGIN_ID};
