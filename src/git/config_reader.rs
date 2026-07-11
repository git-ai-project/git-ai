use crate::error::GitAiError;
use crate::utils::read_file_with_limit;
use bstr::{BStr, BString, ByteSlice, ByteVec};
use gix_config::file::Metadata;
use std::borrow::Cow;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

const MAX_GIT_CONFIG_FILE_BYTES: u64 = 256 * 1_024;
const MAX_GIT_CONFIG_TOTAL_BYTES: u64 = 1_024 * 1_024;
const MAX_GIT_CONFIG_FILES: usize = 32;
const MAX_GIT_CONFIG_SECTIONS: usize = 4_096;
const MAX_GIT_CONFIG_VALUES: usize = 16_384;
const MAX_GIT_CONFIG_ENV_ITEMS: usize = 4_096;
const MAX_GIT_CONFIG_INCLUDE_DEPTH: u8 = 10;

struct ConfigLimits {
    bytes: u64,
    files: usize,
    sections: usize,
    values: usize,
}

impl ConfigLimits {
    fn new() -> Self {
        Self {
            bytes: 0,
            files: 0,
            sections: 0,
            values: 0,
        }
    }

    fn reserve_input(&mut self, kind: &str, bytes: usize) -> Result<(), GitAiError> {
        self.bytes = self.bytes.saturating_add(bytes as u64);
        if self.bytes > MAX_GIT_CONFIG_TOTAL_BYTES {
            return Err(GitAiError::Generic(format!(
                "{kind} exceeded the {MAX_GIT_CONFIG_TOTAL_BYTES} cumulative byte limit ({})",
                self.bytes
            )));
        }
        Ok(())
    }

    fn reserve_file(&mut self, path: &Path, bytes: usize) -> Result<(), GitAiError> {
        self.files = self.files.saturating_add(1);
        if self.files > MAX_GIT_CONFIG_FILES {
            return Err(GitAiError::Generic(format!(
                "Git config exceeded the {MAX_GIT_CONFIG_FILES} file limit while reading {}",
                path.display()
            )));
        }
        self.reserve_input("Git config", bytes)
    }

    fn reserve_structure(&mut self, sections: usize, values: usize) -> Result<(), GitAiError> {
        self.sections = self.sections.saturating_add(sections);
        if self.sections > MAX_GIT_CONFIG_SECTIONS {
            return Err(GitAiError::Generic(format!(
                "Git config exceeded the {MAX_GIT_CONFIG_SECTIONS} section limit ({})",
                self.sections
            )));
        }
        self.values = self.values.saturating_add(values);
        if self.values > MAX_GIT_CONFIG_VALUES {
            return Err(GitAiError::Generic(format!(
                "Git config exceeded the {MAX_GIT_CONFIG_VALUES} value limit ({})",
                self.values
            )));
        }
        Ok(())
    }
}

struct ConfigLoader<'a> {
    git_dir: Option<&'a Path>,
    canonical_git_dir: Option<PathBuf>,
    home_dir: Option<PathBuf>,
    limits: ConfigLimits,
}

impl<'a> ConfigLoader<'a> {
    fn new(git_dir: Option<&'a Path>) -> Self {
        Self {
            git_dir,
            canonical_git_dir: git_dir.and_then(|path| path.canonicalize().ok()),
            home_dir: dirs::home_dir(),
            limits: ConfigLimits::new(),
        }
    }

    fn load_globals(&mut self) -> Result<gix_config::File<'static>, GitAiError> {
        let mut config = gix_config::File::default();
        let mut seen = HashSet::new();
        for kind in [
            gix_config::source::Kind::GitInstallation,
            gix_config::source::Kind::System,
            gix_config::source::Kind::Global,
        ] {
            for source in kind.sources() {
                let Some(path) = source
                    .storage_location(&mut |name| std::env::var_os(name))
                    .map(Cow::into_owned)
                else {
                    continue;
                };
                if !path.is_file() || !seen.insert(path.clone()) {
                    continue;
                }
                config.append(self.load_file(&path, *source, 0, Vec::new())?);
            }
        }
        Ok(config)
    }

    fn load_optional_file(
        &mut self,
        path: &Path,
        source: gix_config::Source,
    ) -> Result<Option<gix_config::File<'static>>, GitAiError> {
        match std::fs::metadata(path) {
            Ok(metadata) if metadata.is_file() => {
                self.load_file(path, source, 0, Vec::new()).map(Some)
            }
            Ok(_) => Ok(None),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error.into()),
        }
    }

    fn load_file(
        &mut self,
        path: &Path,
        source: gix_config::Source,
        depth: u8,
        mut search_remote_urls: Vec<BString>,
    ) -> Result<gix_config::File<'static>, GitAiError> {
        let parsed = self.parse_file(path, source, depth, true)?;
        if depth == 0 {
            extend_remote_urls(&parsed, &mut search_remote_urls)?;
        }
        self.expand_includes(parsed, Some(path), source, depth, &mut search_remote_urls)
    }

    fn parse_file(
        &mut self,
        path: &Path,
        source: gix_config::Source,
        depth: u8,
        lossy: bool,
    ) -> Result<gix_config::File<'static>, GitAiError> {
        if depth >= MAX_GIT_CONFIG_INCLUDE_DEPTH {
            return Err(GitAiError::Generic(format!(
                "Git config include depth exceeded {MAX_GIT_CONFIG_INCLUDE_DEPTH} while reading {}",
                path.display()
            )));
        }
        let mut bytes = read_file_with_limit(path, MAX_GIT_CONFIG_FILE_BYTES, "Git config")?;
        self.limits.reserve_file(path, bytes.len())?;
        let mut metadata = Metadata::try_from_path(path, source)?;
        metadata.level = depth;
        let parsed = gix_config::File::from_bytes_owned(
            &mut bytes,
            metadata,
            gix_config::file::init::Options {
                includes: gix_config::file::includes::Options::no_follow(),
                lossy,
                ..Default::default()
            },
        )
        .map_err(|error| GitAiError::GixError(error.to_string()))?;
        self.reserve_parsed_structure(&parsed)?;
        Ok(parsed)
    }

    fn reserve_parsed_structure(
        &mut self,
        config: &gix_config::File<'_>,
    ) -> Result<(), GitAiError> {
        let mut sections = 0usize;
        let mut values = 0usize;
        for section in config.sections() {
            sections = sections.saturating_add(1);
            values = values.saturating_add(section.body().num_values());
        }
        self.limits.reserve_structure(sections, values)
    }

    fn expand_includes(
        &mut self,
        mut parsed: gix_config::File<'static>,
        source_path: Option<&Path>,
        source: gix_config::Source,
        depth: u8,
        search_remote_urls: &mut Vec<BString>,
    ) -> Result<gix_config::File<'static>, GitAiError> {
        let section_ids: Vec<_> = parsed.section_ids().collect();
        let mut expanded = gix_config::File::default();

        for section_id in section_ids {
            let section = parsed
                .remove_section_by_id(section_id)
                .expect("section id came from the same Git config");
            let include_paths = if self.include_section_matches(
                &section,
                source_path,
                search_remote_urls,
            )? {
                let paths = section.body().values("path");
                if paths.len() > MAX_GIT_CONFIG_FILES {
                    return Err(GitAiError::Generic(format!(
                        "Git config include section exceeded the {MAX_GIT_CONFIG_FILES} path limit ({})",
                        paths.len()
                    )));
                }
                paths
                    .into_iter()
                    .map(|path| gix_config::Path::from(Cow::Owned(path.into_owned())))
                    .collect::<Vec<_>>()
            } else {
                Vec::new()
            };
            expanded.push_section(section);

            for include_path in include_paths {
                let Some(include_path) = self.resolve_include_path(include_path, source_path)?
                else {
                    continue;
                };
                if !include_path.is_file() {
                    continue;
                }
                let included = self.load_file(
                    &include_path,
                    source,
                    depth.saturating_add(1),
                    search_remote_urls.clone(),
                )?;
                extend_remote_urls(&included, search_remote_urls)?;
                expanded.append(included);
            }
        }
        Ok(expanded)
    }

    fn include_section_matches(
        &self,
        section: &gix_config::file::Section<'_>,
        source_path: Option<&Path>,
        search_remote_urls: &[BString],
    ) -> Result<bool, GitAiError> {
        let name = section.header().name();
        if name.eq_ignore_ascii_case(b"include") && section.header().subsection_name().is_none() {
            return Ok(true);
        }
        if !name.eq_ignore_ascii_case(b"includeIf") {
            return Ok(false);
        }
        let Some(condition) = section.header().subsection_name() else {
            return Ok(false);
        };
        let Some(separator) = condition.iter().position(|byte| *byte == b':') else {
            return Ok(false);
        };
        let (prefix, condition) = condition.split_at(separator);
        let condition = &condition[1..];
        match prefix {
            b"gitdir" => self.gitdir_matches(condition.as_bstr(), source_path, false),
            b"gitdir/i" => self.gitdir_matches(condition.as_bstr(), source_path, true),
            b"onbranch" => Ok(false),
            b"hasconfig" => Ok(hasconfig_matches(condition, search_remote_urls)),
            _ => Ok(false),
        }
    }

    fn gitdir_matches(
        &self,
        condition: &BStr,
        source_path: Option<&Path>,
        ignore_case: bool,
    ) -> Result<bool, GitAiError> {
        let Some(git_dir) = self.git_dir else {
            return Ok(false);
        };
        let interpolated = match gix_config::Path::from(Cow::Borrowed(condition)).interpolate(
            gix_config::path::interpolate::Context {
                home_dir: self.home_dir.as_deref(),
                home_for_user: Some(gix_config::path::interpolate::home_for_user),
                ..Default::default()
            },
        ) {
            Ok(path) => path,
            Err(_) => return Ok(false),
        };
        let mut pattern =
            gix_path::to_unix_separators_on_windows(gix_path::into_bstr(interpolated));
        if let Some(relative) = pattern.strip_prefix(b"./") {
            let Some(parent) = source_path.and_then(Path::parent) else {
                return Ok(false);
            };
            let mut joined =
                gix_path::to_unix_separators_on_windows(gix_path::into_bstr(parent)).into_owned();
            joined.push(b'/');
            joined.extend_from_slice(relative);
            pattern = Cow::Owned(joined);
        }
        if !gix_path::from_bstr(pattern.clone()).is_absolute() {
            let mut prefixed = pattern.into_owned();
            prefixed.insert_str(0, "**/");
            pattern = Cow::Owned(prefixed);
        }
        if pattern.ends_with(b"/") {
            let mut suffixed = pattern.into_owned();
            suffixed.push_str("**");
            pattern = Cow::Owned(suffixed);
        }

        let mut mode = gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL;
        if ignore_case {
            mode |= gix_glob::wildmatch::Mode::IGNORE_CASE;
        }
        for candidate in
            std::iter::once(git_dir).chain(self.canonical_git_dir.as_deref().into_iter())
        {
            let candidate = gix_path::to_unix_separators_on_windows(gix_path::into_bstr(candidate));
            if gix_glob::wildmatch(pattern.as_bstr(), candidate.as_bstr(), mode) {
                return Ok(true);
            }
        }
        Ok(false)
    }

    fn resolve_include_path(
        &self,
        path: gix_config::Path<'static>,
        source_path: Option<&Path>,
    ) -> Result<Option<PathBuf>, GitAiError> {
        let path = match path.interpolate(gix_config::path::interpolate::Context {
            home_dir: self.home_dir.as_deref(),
            home_for_user: Some(gix_config::path::interpolate::home_for_user),
            ..Default::default()
        }) {
            Ok(path) => path,
            Err(_) => return Ok(None),
        };
        if path.is_absolute() {
            return Ok(Some(path.into_owned()));
        }
        Ok(source_path
            .and_then(Path::parent)
            .map(|parent| parent.join(path)))
    }

    fn load_environment_overrides(&mut self) -> Result<gix_config::File<'static>, GitAiError> {
        let Some(count) = std::env::var("GIT_CONFIG_COUNT")
            .ok()
            .map(|value| value.parse::<usize>())
            .transpose()
            .map_err(|error| GitAiError::GixError(error.to_string()))?
        else {
            return Ok(gix_config::File::default());
        };
        if count > MAX_GIT_CONFIG_ENV_ITEMS {
            return Err(GitAiError::Generic(format!(
                "Git environment config exceeded the {MAX_GIT_CONFIG_ENV_ITEMS} item limit ({count})"
            )));
        }
        let mut bytes = 0usize;
        for index in 0..count {
            for prefix in ["GIT_CONFIG_KEY_", "GIT_CONFIG_VALUE_"] {
                if let Some(value) = std::env::var_os(format!("{prefix}{index}")) {
                    bytes = bytes.saturating_add(value.as_encoded_bytes().len());
                    if bytes as u64 > MAX_GIT_CONFIG_TOTAL_BYTES {
                        return Err(GitAiError::Generic(format!(
                            "Git environment config exceeded the {MAX_GIT_CONFIG_TOTAL_BYTES} byte limit ({bytes})"
                        )));
                    }
                }
            }
        }
        self.limits.reserve_input("Git environment config", bytes)?;
        let Some(parsed) = gix_config::File::from_env(gix_config::file::init::Options {
            includes: gix_config::file::includes::Options::no_follow(),
            ..Default::default()
        })
        .map_err(|error| GitAiError::GixError(error.to_string()))?
        else {
            return Ok(gix_config::File::default());
        };
        self.reserve_parsed_structure(&parsed)?;
        let mut search_remote_urls = Vec::new();
        extend_remote_urls(&parsed, &mut search_remote_urls)?;
        self.expand_includes(
            parsed,
            None,
            gix_config::Source::Env,
            0,
            &mut search_remote_urls,
        )
    }
}

fn extend_remote_urls(
    config: &gix_config::File<'_>,
    urls: &mut Vec<BString>,
) -> Result<(), GitAiError> {
    for section in config.sections() {
        if !section.header().name().eq_ignore_ascii_case(b"remote") {
            continue;
        }
        for url in section.body().values("url") {
            if urls.len() >= MAX_GIT_CONFIG_SECTIONS {
                return Err(GitAiError::Generic(format!(
                    "Git config exceeded the {MAX_GIT_CONFIG_SECTIONS} remote URL limit"
                )));
            }
            urls.push(url.into_owned());
        }
    }
    Ok(())
}

fn hasconfig_matches(condition: &[u8], remote_urls: &[BString]) -> bool {
    let Some(separator) = condition.iter().position(|byte| *byte == b':') else {
        return false;
    };
    let (key_glob, value_glob) = condition.split_at(separator);
    if key_glob != b"remote.*.url" {
        return false;
    }
    let value_glob = &value_glob[1..];
    remote_urls.iter().any(|url| {
        gix_glob::wildmatch(
            value_glob.as_bstr(),
            url.as_bstr(),
            gix_glob::wildmatch::Mode::NO_MATCH_SLASH_LITERAL,
        )
    })
}

pub(crate) fn load_global_git_config() -> Result<gix_config::File<'static>, GitAiError> {
    ConfigLoader::new(None).load_globals()
}

pub(crate) fn load_single_git_config(
    path: &Path,
    source: gix_config::Source,
) -> Result<Option<gix_config::File<'static>>, GitAiError> {
    match std::fs::metadata(path) {
        Ok(metadata) if metadata.is_file() => ConfigLoader::new(None)
            .parse_file(path, source, 0, false)
            .map(Some),
        Ok(_) => Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error.into()),
    }
}

pub(crate) fn load_repository_git_config(
    git_dir: &Path,
    git_common_dir: &Path,
) -> Result<gix_config::File<'static>, GitAiError> {
    let mut loader = ConfigLoader::new(Some(git_dir));
    let mut config = loader.load_globals()?;

    let local_path = git_common_dir.join("config");
    let local = loader.load_optional_file(&local_path, gix_config::Source::Local)?;
    let worktree_config_enabled = local
        .as_ref()
        .and_then(|config| config.boolean("extensions.worktreeConfig"))
        .and_then(Result::ok)
        .unwrap_or(false);
    if let Some(local) = local {
        config.append(local);
    }

    if worktree_config_enabled {
        let worktree_path = git_dir.join("config.worktree");
        if let Some(worktree) =
            loader.load_optional_file(&worktree_path, gix_config::Source::Worktree)?
        {
            config.append(worktree);
        }
    }

    config.append(loader.load_environment_overrides()?);
    Ok(config)
}
