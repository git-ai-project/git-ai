/// Minimum IntelliJ build number required for the plugin
/// Build 252 corresponds to IntelliJ 2025.2
pub const MIN_INTELLIJ_BUILD: u32 = 252;

/// Plugin ID for the Git AI IntelliJ plugin
pub const PLUGIN_ID: &str = "com.usegitai.plugins.jetbrains";

/// JetBrains Marketplace URL for manual installation
pub const MARKETPLACE_URL: &str =
    "https://plugins.jetbrains.com/plugin/com.usegitai.plugins.jetbrains";

/// Definition of a JetBrains IDE
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct JetBrainsIde {
    /// Human-readable name
    pub name: &'static str,
    /// macOS bundle IDs
    pub bundle_ids: &'static [&'static str],
    /// Binary name on macOS (inside Contents/MacOS/)
    pub binary_name_macos: &'static str,
    /// Binary name on Windows (e.g., idea64.exe)
    pub binary_name_windows: &'static str,
    /// Binary name on Linux (e.g., idea.sh)
    pub binary_name_linux: &'static str,
    /// Product code used in config directories (IU, IC, PY, etc.)
    pub product_code: &'static str,
    /// Toolbox app directory name
    pub toolbox_app_name: &'static str,
}

/// All supported JetBrains IDEs
pub const JETBRAINS_IDES: &[JetBrainsIde] = &[
    JetBrainsIde {
        name: "IntelliJ IDEA Ultimate",
        bundle_ids: &["com.jetbrains.intellij"],
        binary_name_macos: "idea",
        binary_name_windows: "idea64.exe",
        binary_name_linux: "idea.sh",
        product_code: "IU",
        toolbox_app_name: "IDEA-U",
    },
    JetBrainsIde {
        name: "IntelliJ IDEA Community",
        bundle_ids: &["com.jetbrains.intellij.ce"],
        binary_name_macos: "idea",
        binary_name_windows: "idea64.exe",
        binary_name_linux: "idea.sh",
        product_code: "IC",
        toolbox_app_name: "IDEA-C",
    },
    JetBrainsIde {
        name: "PyCharm Professional",
        bundle_ids: &["com.jetbrains.pycharm"],
        binary_name_macos: "pycharm",
        binary_name_windows: "pycharm64.exe",
        binary_name_linux: "pycharm.sh",
        product_code: "PY",
        toolbox_app_name: "PyCharm-P",
    },
    JetBrainsIde {
        name: "PyCharm Community",
        bundle_ids: &["com.jetbrains.pycharm.ce"],
        binary_name_macos: "pycharm",
        binary_name_windows: "pycharm64.exe",
        binary_name_linux: "pycharm.sh",
        product_code: "PC",
        toolbox_app_name: "PyCharm-C",
    },
    JetBrainsIde {
        name: "WebStorm",
        bundle_ids: &["com.jetbrains.WebStorm"],
        binary_name_macos: "webstorm",
        binary_name_windows: "webstorm64.exe",
        binary_name_linux: "webstorm.sh",
        product_code: "WS",
        toolbox_app_name: "WebStorm",
    },
    JetBrainsIde {
        name: "GoLand",
        bundle_ids: &["com.jetbrains.goland"],
        binary_name_macos: "goland",
        binary_name_windows: "goland64.exe",
        binary_name_linux: "goland.sh",
        product_code: "GO",
        toolbox_app_name: "GoLand",
    },
    JetBrainsIde {
        name: "CLion",
        bundle_ids: &["com.jetbrains.CLion"],
        binary_name_macos: "clion",
        binary_name_windows: "clion64.exe",
        binary_name_linux: "clion.sh",
        product_code: "CL",
        toolbox_app_name: "CLion",
    },
    JetBrainsIde {
        name: "PhpStorm",
        bundle_ids: &["com.jetbrains.PhpStorm"],
        binary_name_macos: "phpstorm",
        binary_name_windows: "phpstorm64.exe",
        binary_name_linux: "phpstorm.sh",
        product_code: "PS",
        toolbox_app_name: "PhpStorm",
    },
    JetBrainsIde {
        name: "Rider",
        bundle_ids: &["com.jetbrains.rider"],
        binary_name_macos: "rider",
        binary_name_windows: "rider64.exe",
        binary_name_linux: "rider.sh",
        product_code: "RD",
        toolbox_app_name: "Rider",
    },
    JetBrainsIde {
        name: "RubyMine",
        bundle_ids: &["com.jetbrains.rubymine"],
        binary_name_macos: "rubymine",
        binary_name_windows: "rubymine64.exe",
        binary_name_linux: "rubymine.sh",
        product_code: "RM",
        toolbox_app_name: "RubyMine",
    },
    JetBrainsIde {
        name: "DataGrip",
        bundle_ids: &["com.jetbrains.datagrip"],
        binary_name_macos: "datagrip",
        binary_name_windows: "datagrip64.exe",
        binary_name_linux: "datagrip.sh",
        product_code: "DB",
        toolbox_app_name: "DataGrip",
    },
    JetBrainsIde {
        name: "Android Studio",
        bundle_ids: &["com.google.android.studio"],
        binary_name_macos: "studio",
        binary_name_windows: "studio64.exe",
        binary_name_linux: "studio.sh",
        product_code: "AI",
        toolbox_app_name: "AndroidStudio",
    },
];

/// A detected JetBrains IDE installation
#[derive(Debug, Clone)]
pub struct DetectedIde {
    /// The IDE definition
    pub ide: &'static JetBrainsIde,
    /// Path to the IDE installation (app bundle on macOS, install dir on Windows/Linux)
    pub install_path: std::path::PathBuf,
    /// Path to the IDE binary
    pub binary_path: std::path::PathBuf,
    /// Build number (e.g., "252.12345")
    pub build_number: Option<String>,
    /// Major build number (e.g., 252)
    pub major_build: Option<u32>,
    /// Path to the plugins directory for this IDE
    pub plugins_dir: std::path::PathBuf,
}

impl DetectedIde {
    /// Check if this IDE meets the minimum version requirement
    pub fn is_compatible(&self) -> bool {
        self.major_build
            .map(|build| build >= MIN_INTELLIJ_BUILD)
            .unwrap_or(false)
    }
}
