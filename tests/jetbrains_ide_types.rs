/// Comprehensive tests for JetBrains IDE type definitions and compatibility checking
use git_ai::mdm::jetbrains::ide_types::{
    DetectedIde, JETBRAINS_IDES, MARKETPLACE_URL, MIN_INTELLIJ_BUILD, PLUGIN_ID,
};
use std::path::PathBuf;

#[test]
fn test_constants() {
    // Verify plugin constants are correctly defined
    assert_eq!(MIN_INTELLIJ_BUILD, 252, "Min build should be 252 (2025.2)");
    assert_eq!(PLUGIN_ID, "com.usegitai.plugins.jetbrains");
    assert!(MARKETPLACE_URL.starts_with("https://plugins.jetbrains.com/"));
    assert!(MARKETPLACE_URL.contains(PLUGIN_ID));
}

#[test]
fn test_jetbrains_ides_definitions() {
    // Verify we have all major JetBrains IDEs defined
    assert!(!JETBRAINS_IDES.is_empty(), "Should have IDE definitions");

    let ide_names: Vec<&str> = JETBRAINS_IDES.iter().map(|ide| ide.name).collect();

    // Check for major IDEs
    assert!(
        ide_names
            .iter()
            .any(|n| n.contains("IntelliJ IDEA Ultimate"))
    );
    assert!(
        ide_names
            .iter()
            .any(|n| n.contains("IntelliJ IDEA Community"))
    );
    assert!(ide_names.iter().any(|n| n.contains("PyCharm")));
    assert!(ide_names.iter().any(|n| n.contains("WebStorm")));
    assert!(ide_names.iter().any(|n| n.contains("GoLand")));
    assert!(ide_names.iter().any(|n| n.contains("CLion")));
    assert!(ide_names.iter().any(|n| n.contains("PhpStorm")));
    assert!(ide_names.iter().any(|n| n.contains("Rider")));
    assert!(ide_names.iter().any(|n| n.contains("RubyMine")));
    assert!(ide_names.iter().any(|n| n.contains("DataGrip")));
    assert!(ide_names.iter().any(|n| n.contains("Android Studio")));
}

#[test]
fn test_intellij_ultimate_definition() {
    let intellij = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "IntelliJ IDEA Ultimate")
        .expect("IntelliJ Ultimate should be defined");

    assert!(intellij.bundle_ids.contains(&"com.jetbrains.intellij"));
    assert_eq!(intellij.binary_name_macos, "idea");
    assert_eq!(intellij.binary_name_windows, "idea64.exe");
    assert_eq!(intellij.binary_name_linux, "idea.sh");
    assert_eq!(intellij.product_code, "IU");
    assert_eq!(intellij.toolbox_app_name, "IDEA-U");
}

#[test]
fn test_intellij_community_definition() {
    let intellij_ce = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "IntelliJ IDEA Community")
        .expect("IntelliJ Community should be defined");

    assert!(
        intellij_ce
            .bundle_ids
            .contains(&"com.jetbrains.intellij.ce")
    );
    assert_eq!(intellij_ce.binary_name_macos, "idea");
    assert_eq!(intellij_ce.product_code, "IC");
    assert_eq!(intellij_ce.toolbox_app_name, "IDEA-C");
}

#[test]
fn test_pycharm_definitions() {
    let pycharm_pro = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "PyCharm Professional")
        .expect("PyCharm Pro should be defined");

    assert!(pycharm_pro.bundle_ids.contains(&"com.jetbrains.pycharm"));
    assert_eq!(pycharm_pro.binary_name_macos, "pycharm");
    assert_eq!(pycharm_pro.binary_name_windows, "pycharm64.exe");
    assert_eq!(pycharm_pro.product_code, "PY");

    let pycharm_ce = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "PyCharm Community")
        .expect("PyCharm CE should be defined");

    assert!(pycharm_ce.bundle_ids.contains(&"com.jetbrains.pycharm.ce"));
    assert_eq!(pycharm_ce.product_code, "PC");
}

#[test]
fn test_webstorm_definition() {
    let webstorm = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "WebStorm")
        .expect("WebStorm should be defined");

    assert!(webstorm.bundle_ids.contains(&"com.jetbrains.WebStorm"));
    assert_eq!(webstorm.binary_name_macos, "webstorm");
    assert_eq!(webstorm.binary_name_windows, "webstorm64.exe");
    assert_eq!(webstorm.product_code, "WS");
    assert_eq!(webstorm.toolbox_app_name, "WebStorm");
}

#[test]
fn test_goland_definition() {
    let goland = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "GoLand")
        .expect("GoLand should be defined");

    assert!(goland.bundle_ids.contains(&"com.jetbrains.goland"));
    assert_eq!(goland.binary_name_macos, "goland");
    assert_eq!(goland.product_code, "GO");
}

#[test]
fn test_clion_definition() {
    let clion = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "CLion")
        .expect("CLion should be defined");

    assert!(clion.bundle_ids.contains(&"com.jetbrains.CLion"));
    assert_eq!(clion.binary_name_macos, "clion");
    assert_eq!(clion.product_code, "CL");
}

#[test]
fn test_phpstorm_definition() {
    let phpstorm = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "PhpStorm")
        .expect("PhpStorm should be defined");

    assert!(phpstorm.bundle_ids.contains(&"com.jetbrains.PhpStorm"));
    assert_eq!(phpstorm.binary_name_macos, "phpstorm");
    assert_eq!(phpstorm.product_code, "PS");
}

#[test]
fn test_rider_definition() {
    let rider = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "Rider")
        .expect("Rider should be defined");

    assert!(rider.bundle_ids.contains(&"com.jetbrains.rider"));
    assert_eq!(rider.binary_name_macos, "rider");
    assert_eq!(rider.product_code, "RD");
}

#[test]
fn test_rubymine_definition() {
    let rubymine = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "RubyMine")
        .expect("RubyMine should be defined");

    assert!(rubymine.bundle_ids.contains(&"com.jetbrains.rubymine"));
    assert_eq!(rubymine.binary_name_macos, "rubymine");
    assert_eq!(rubymine.product_code, "RM");
}

#[test]
fn test_datagrip_definition() {
    let datagrip = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "DataGrip")
        .expect("DataGrip should be defined");

    assert!(datagrip.bundle_ids.contains(&"com.jetbrains.datagrip"));
    assert_eq!(datagrip.binary_name_macos, "datagrip");
    assert_eq!(datagrip.product_code, "DB");
}

#[test]
fn test_android_studio_definition() {
    let android = JETBRAINS_IDES
        .iter()
        .find(|ide| ide.name == "Android Studio")
        .expect("Android Studio should be defined");

    assert!(android.bundle_ids.contains(&"com.google.android.studio"));
    assert_eq!(android.binary_name_macos, "studio");
    assert_eq!(android.binary_name_windows, "studio64.exe");
    assert_eq!(android.product_code, "AI");
}

#[test]
fn test_all_ides_have_bundle_ids() {
    for ide in JETBRAINS_IDES {
        assert!(
            !ide.bundle_ids.is_empty(),
            "{} should have bundle IDs",
            ide.name
        );
    }
}

#[test]
fn test_all_ides_have_binary_names() {
    for ide in JETBRAINS_IDES {
        assert!(
            !ide.binary_name_macos.is_empty(),
            "{} should have macOS binary",
            ide.name
        );
        assert!(
            !ide.binary_name_windows.is_empty(),
            "{} should have Windows binary",
            ide.name
        );
        assert!(
            !ide.binary_name_linux.is_empty(),
            "{} should have Linux binary",
            ide.name
        );
    }
}

#[test]
fn test_all_ides_have_product_codes() {
    for ide in JETBRAINS_IDES {
        assert!(
            !ide.product_code.is_empty(),
            "{} should have product code",
            ide.name
        );
        assert!(
            ide.product_code.chars().all(|c| c.is_ascii_uppercase()),
            "{} product code should be uppercase ASCII",
            ide.name
        );
    }
}

#[test]
fn test_all_ides_have_toolbox_names() {
    for ide in JETBRAINS_IDES {
        assert!(
            !ide.toolbox_app_name.is_empty(),
            "{} should have toolbox name",
            ide.name
        );
    }
}

#[test]
fn test_detected_ide_compatible_with_min_build() {
    let ide = &JETBRAINS_IDES[0]; // Use first IDE as example

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/Applications/IntelliJ IDEA.app"),
        binary_path: PathBuf::from("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
        build_number: Some("252.12345".to_string()),
        major_build: Some(252),
        plugins_dir: PathBuf::from("/Users/test/Library/Application Support/JetBrains/IU2025.2"),
    };

    assert!(detected.is_compatible(), "Build 252 should be compatible");
}

#[test]
fn test_detected_ide_compatible_with_newer_build() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/Applications/IntelliJ IDEA.app"),
        binary_path: PathBuf::from("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
        build_number: Some("300.12345".to_string()),
        major_build: Some(300),
        plugins_dir: PathBuf::from("/Users/test/Library/Application Support/JetBrains/IU2025.2"),
    };

    assert!(detected.is_compatible(), "Build 300 should be compatible");
}

#[test]
fn test_detected_ide_incompatible_with_old_build() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/Applications/IntelliJ IDEA.app"),
        binary_path: PathBuf::from("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
        build_number: Some("251.99999".to_string()),
        major_build: Some(251),
        plugins_dir: PathBuf::from("/Users/test/Library/Application Support/JetBrains/IU2024.1"),
    };

    assert!(
        !detected.is_compatible(),
        "Build 251 should be incompatible"
    );
}

#[test]
fn test_detected_ide_incompatible_without_build_number() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/Applications/IntelliJ IDEA.app"),
        binary_path: PathBuf::from("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
        build_number: None,
        major_build: None,
        plugins_dir: PathBuf::from("/Users/test/Library/Application Support/JetBrains/IU2025.2"),
    };

    assert!(
        !detected.is_compatible(),
        "Should be incompatible without build number"
    );
}

#[test]
fn test_detected_ide_incompatible_with_only_build_string() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/Applications/IntelliJ IDEA.app"),
        binary_path: PathBuf::from("/Applications/IntelliJ IDEA.app/Contents/MacOS/idea"),
        build_number: Some("252.12345".to_string()),
        major_build: None, // Missing parsed major build
        plugins_dir: PathBuf::from("/Users/test/Library/Application Support/JetBrains/IU2025.2"),
    };

    assert!(
        !detected.is_compatible(),
        "Should be incompatible without parsed major build"
    );
}

#[test]
fn test_binary_names_have_correct_extensions() {
    for ide in JETBRAINS_IDES {
        // macOS and Linux should not have .exe
        assert!(
            !ide.binary_name_macos.ends_with(".exe"),
            "{} macOS binary should not end with .exe",
            ide.name
        );
        assert!(
            !ide.binary_name_linux.ends_with(".exe"),
            "{} Linux binary should not end with .exe",
            ide.name
        );

        // Windows should have .exe
        assert!(
            ide.binary_name_windows.ends_with(".exe"),
            "{} Windows binary should end with .exe",
            ide.name
        );

        // Linux should typically have .sh
        assert!(
            ide.binary_name_linux.ends_with(".sh"),
            "{} Linux binary should end with .sh",
            ide.name
        );
    }
}

#[test]
fn test_product_codes_are_unique() {
    use std::collections::HashSet;

    let mut product_codes = HashSet::new();
    for ide in JETBRAINS_IDES {
        assert!(
            product_codes.insert(ide.product_code),
            "Product code {} is not unique",
            ide.product_code
        );
    }
}

#[test]
fn test_toolbox_names_are_unique() {
    use std::collections::HashSet;

    let mut toolbox_names = HashSet::new();
    for ide in JETBRAINS_IDES {
        assert!(
            toolbox_names.insert(ide.toolbox_app_name),
            "Toolbox name {} is not unique",
            ide.toolbox_app_name
        );
    }
}

#[test]
fn test_detected_ide_clone() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/test/path"),
        binary_path: PathBuf::from("/test/binary"),
        build_number: Some("252.1".to_string()),
        major_build: Some(252),
        plugins_dir: PathBuf::from("/test/plugins"),
    };

    let cloned = detected.clone();
    assert_eq!(cloned.install_path, detected.install_path);
    assert_eq!(cloned.binary_path, detected.binary_path);
    assert_eq!(cloned.build_number, detected.build_number);
    assert_eq!(cloned.major_build, detected.major_build);
    assert_eq!(cloned.plugins_dir, detected.plugins_dir);
}

#[test]
fn test_detected_ide_debug_format() {
    let ide = &JETBRAINS_IDES[0];

    let detected = DetectedIde {
        ide,
        install_path: PathBuf::from("/test"),
        binary_path: PathBuf::from("/test/bin"),
        build_number: Some("252.1".to_string()),
        major_build: Some(252),
        plugins_dir: PathBuf::from("/test/plugins"),
    };

    let debug_str = format!("{:?}", detected);
    assert!(debug_str.contains("DetectedIde"));
}

#[test]
fn test_jetbrains_ide_clone() {
    let ide = &JETBRAINS_IDES[0];
    let cloned = ide.clone();

    assert_eq!(ide.name, cloned.name);
    assert_eq!(ide.bundle_ids, cloned.bundle_ids);
    assert_eq!(ide.binary_name_macos, cloned.binary_name_macos);
    assert_eq!(ide.binary_name_windows, cloned.binary_name_windows);
    assert_eq!(ide.binary_name_linux, cloned.binary_name_linux);
    assert_eq!(ide.product_code, cloned.product_code);
    assert_eq!(ide.toolbox_app_name, cloned.toolbox_app_name);
}
