use std::path::Path;

fn packaging_path(path: &str) -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("packaging")
        .join(path)
}

#[test]
fn msi_is_per_user_and_updates_only_the_user_path() {
    let wix = std::fs::read_to_string(packaging_path("windows/git-ai.wxs")).unwrap();

    assert!(wix.contains("Scope=\"perUser\""));
    assert!(wix.contains("xmlns:util=\"http://wixtoolset.org/schemas/v4/wxs/util\""));
    assert!(wix.contains("<util:QueryWindowsDirectories />"));
    assert!(wix.contains("Id=\"INSTALLFOLDER\""));
    assert!(wix.contains("Value=\"[WIX_DIR_PROFILE]\\.git-ai\\bin\""));
    assert!(wix.contains("Before=\"CostFinalize\""));
    assert!(wix.contains("Directory Id=\"INSTALLFOLDER\" Name=\"bin\""));
    assert!(wix.contains("Part=\"first\""));
    assert!(wix.contains("System=\"no\""));
    assert!(!wix.contains("perMachine"));
    assert!(!wix.contains("ProgramFiles"));
    assert!(!wix.contains("LocalAppDataFolder"));
    assert!(!wix.contains("UserProfileFolder"));
    assert!(!wix.contains("System=\"yes\""));
}

#[test]
fn msi_accepts_hidden_api_properties_and_configures_the_installing_user() {
    let wix = std::fs::read_to_string(packaging_path("windows/git-ai.wxs")).unwrap();
    let readme = std::fs::read_to_string(packaging_path("README.md")).unwrap();

    assert!(wix.contains("Property Id=\"API_BASE\" Hidden=\"yes\""));
    assert!(wix.contains("Property Id=\"API_KEY\" Hidden=\"yes\""));
    assert!(wix.contains("FileRef=\"GitAiExe\""));
    assert!(wix.contains("ExeCommand=\"[CustomActionData]\""));
    assert!(!wix.contains("ExeCommand=\"[ConfigureGitAi]\""));
    assert!(wix.contains("Execute=\"deferred\""));
    assert!(wix.contains("Impersonate=\"yes\""));
    assert!(wix.contains("HideTarget=\"yes\""));
    assert!(wix.contains("install-hooks --api-base"));
    assert!(readme.contains("msiexec /i"));
    assert!(readme.contains("API_BASE="));
    assert!(readme.contains("API_KEY="));
}

#[test]
fn pkg_installs_only_for_the_console_user_and_fails_without_one() {
    let builder = std::fs::read_to_string(packaging_path("macos/build-pkg.sh")).unwrap();
    let postinstall = std::fs::read_to_string(packaging_path("macos/scripts/postinstall")).unwrap();

    assert!(builder.contains("--nopayload"));
    assert!(builder.contains("$SCRIPTS/git-ai"));
    assert!(!builder.contains("/opt/git-ai"));
    assert!(!builder.contains("/usr/local/bin"));
    assert!(postinstall.contains("/usr/bin/stat -f%Su /dev/console"));
    assert!(postinstall.contains("no valid console user is logged in"));
    assert!(postinstall.contains("GIT_AI_HOME=\"$USER_HOME/.git-ai\""));
    assert!(postinstall.contains("/usr/sbin/chown \"$CONSOLE_USER:$USER_GROUP\""));
    assert!(postinstall.contains("install-hooks"));
    assert!(!postinstall.contains("setup-package"));
    assert!(!postinstall.contains("|| true"));
}

#[test]
fn packaging_supports_only_msi_and_pkg() {
    assert!(packaging_path("windows").is_dir());
    assert!(packaging_path("macos").is_dir());
    assert!(!packaging_path("debian/build-deb.sh").exists());
    assert!(!packaging_path("homebrew/update-formula.sh").exists());
}

#[test]
fn msi_builder_uses_the_sponsored_wix_v7_toolchain() {
    let builder = std::fs::read_to_string(packaging_path("windows/build-msi.ps1")).unwrap();

    assert!(builder.contains("$wixVersion = '7.0.0'"));
    assert!(builder.contains("dotnet tool update --global wix --version $wixVersion"));
    assert!(builder.contains("$wixUtilExtension = 'WixToolset.Util.wixext/7.0.0'"));
    assert!(builder.contains("eula accept wix7"));
    assert!(builder.contains("extension add --global $wixUtilExtension"));
    assert!(builder.contains("-ext $wixUtilExtension"));
    assert!(builder.contains("-acceptEula wix7"));
}
