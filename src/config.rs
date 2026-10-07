use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use facet::Facet;
use serde::Deserialize;

use crate::input_limits::InputLimitsConfig;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8000";
const DEFAULT_MAX_WORKERS: usize = 2;
const DEFAULT_QUEUE_CAPACITY: usize = 8;
const DEFAULT_PREWARMED_WORKERS: usize = 1;

#[derive(Debug, Clone, Deserialize, Facet)]
pub struct AppConfig {
    #[serde(default = "default_bind_address")]
    pub bind_addr: String,
    pub allowed_hosts: Option<Vec<String>>,
    #[serde(default)]
    pub http_allowed_hosts: Vec<String>,
    #[serde(default)]
    pub skip_host_header_verification: bool,
    pub asset_dir: Option<PathBuf>,
    #[serde(default)]
    pub preinstall_packages: Vec<String>,
    #[serde(default = "default_runtime_package_installation")]
    pub runtime_package_installation: bool,
    #[serde(default)]
    pub workers: WorkersConfig,
    pub package_cache_dir: Option<PathBuf>,
    #[serde(default)]
    pub code_snippets: CodeSnippetsConfig,
    #[serde(default)]
    pub search: SearchConfig,
    #[serde(default)]
    pub input_limits: InputLimitsConfig,
}

#[derive(Debug, Clone, Deserialize, Facet)]
#[serde(default)]
pub struct SearchConfig {
    /// Minimum number of ranked snippets to return, capped by the visible catalog size.
    pub min_hits: usize,
}

impl Default for SearchConfig {
    fn default() -> Self {
        Self { min_hits: 3 }
    }
}

#[derive(Debug, Clone, Deserialize, Facet)]
#[serde(default)]
pub struct WorkersConfig {
    #[serde(default = "default_max_workers")]
    pub max_workers: usize,
    #[serde(default = "default_queue_capacity")]
    pub queue_capacity: usize,
    #[serde(default = "default_prewarmed_workers")]
    pub prewarmed_workers: usize,
}

#[derive(Debug, Clone, Deserialize, Facet)]
#[serde(default)]
pub struct CodeSnippetsConfig {
    #[serde(default = "default_code_snippets_enabled")]
    pub enabled: bool,
    pub snippets: BTreeMap<String, CodeSnippet>,
    pub include_snippet_ids: Vec<String>,
    pub include_snippet_tags: Vec<String>,
    pub exclude_snippet_ids: Vec<String>,
}

fn default_code_snippets_enabled() -> bool {
    true
}

#[derive(Debug, Clone, Deserialize, Facet)]
pub struct CodeSnippet {
    pub title: String,
    #[serde(alias = "descripton")]
    pub description: String,
    #[serde(default)]
    pub required_python_packages: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    pub code_snippet: String,
}

fn default_runtime_package_installation() -> bool {
    true
}

fn default_max_workers() -> usize {
    DEFAULT_MAX_WORKERS
}

fn default_queue_capacity() -> usize {
    DEFAULT_QUEUE_CAPACITY
}

fn default_prewarmed_workers() -> usize {
    DEFAULT_PREWARMED_WORKERS
}

fn default_bind_address() -> String {
    DEFAULT_BIND_ADDRESS.to_owned()
}

impl AppConfig {
    pub fn load() -> Result<Self> {
        let explicit_paths = parse_config_args(env::args().skip(1))?;
        let paths = match explicit_paths {
            Some(paths) => paths,
            None => discover_config_files(Path::new("."))?,
        };
        let mut config = Self::from_files(&paths)?;
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_BIND_ADDR") {
            config.bind_addr = value;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_ALLOWED_HOSTS") {
            config.allowed_hosts =
                Some(value.split(',').map(str::trim).map(str::to_owned).collect());
        }
        if let Some(value) = env::var_os("CODE_INTERPRETER_MCP_ASSET_DIR") {
            config.asset_dir = Some(PathBuf::from(value));
        }
        if let Some(value) = env::var_os("CODE_INTERPRETER_MCP_PACKAGE_CACHE_DIR") {
            config.package_cache_dir = Some(PathBuf::from(value));
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_MAX_WORKERS") {
            config.workers.max_workers = value
                .parse()
                .context("CODE_INTERPRETER_MCP_MAX_WORKERS must be a positive integer")?;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_QUEUE_CAPACITY") {
            config.workers.queue_capacity = value
                .parse()
                .context("CODE_INTERPRETER_MCP_QUEUE_CAPACITY must be a positive integer")?;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_PREWARMED_WORKERS") {
            config.workers.prewarmed_workers = value
                .parse()
                .context("CODE_INTERPRETER_MCP_PREWARMED_WORKERS must be a non-negative integer")?;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_HTTP_ALLOWED_HOSTS") {
            config.http_allowed_hosts = value
                .split(',')
                .map(str::trim)
                .filter(|host| !host.is_empty())
                .map(str::to_owned)
                .collect();
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_SKIP_HOST_HEADER_VERIFICATION") {
            config.skip_host_header_verification = value.parse().context(
                "CODE_INTERPRETER_MCP_SKIP_HOST_HEADER_VERIFICATION must be true or false",
            )?;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_MAX_FILE_KB") {
            config.input_limits.max_file_kb = value.parse().context(
                "CODE_INTERPRETER_MCP_MAX_FILE_KB must be a positive integer in KB (1 KB = 1024 bytes)",
            )?;
        }
        if let Ok(value) = env::var("CODE_INTERPRETER_MCP_MAX_TOTAL_FILE_KB") {
            config.input_limits.max_total_file_kb = value.parse().context(
                "CODE_INTERPRETER_MCP_MAX_TOTAL_FILE_KB must be a positive integer in KB (1 KB = 1024 bytes)",
            )?;
        }
        config.input_limits.validate()?;
        Ok(config)
    }

    pub fn package_cache_dir(&self) -> PathBuf {
        self.package_cache_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(".code-interpreter-mcp-package-cache"))
    }

    pub fn validate_worker_settings(&self) -> Result<()> {
        if self.workers.max_workers == 0 {
            bail!("max_workers must be greater than zero");
        }
        if self.workers.queue_capacity == 0 {
            bail!("queue_capacity must be greater than zero");
        }
        if self.workers.prewarmed_workers > self.workers.max_workers {
            bail!("prewarmed_workers must not exceed max_workers");
        }
        Ok(())
    }

    fn from_files(paths: &[PathBuf]) -> Result<Self> {
        let mut merged = toml::Value::Table(Default::default());
        for path in paths {
            let source = fs::read_to_string(path)
                .with_context(|| format!("failed to read config file {}", path.display()))?;
            let value = toml::from_str::<toml::Value>(&source)
                .with_context(|| format!("invalid TOML in {}", path.display()))?;
            merge(&mut merged, value);
        }
        merged
            .try_into()
            .context("invalid code-interpreter-mcp configuration")
    }
}

impl Default for WorkersConfig {
    fn default() -> Self {
        Self {
            max_workers: default_max_workers(),
            queue_capacity: default_queue_capacity(),
            prewarmed_workers: default_prewarmed_workers(),
        }
    }
}

impl Default for CodeSnippetsConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            snippets: BTreeMap::new(),
            include_snippet_ids: Vec::new(),
            include_snippet_tags: Vec::new(),
            exclude_snippet_ids: Vec::new(),
        }
    }
}

fn parse_config_args(args: impl IntoIterator<Item = String>) -> Result<Option<Vec<PathBuf>>> {
    let mut paths = Vec::new();
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--config" {
            let path = args.next().context("--config requires a file path")?;
            if path.is_empty() || path.starts_with("--") {
                bail!("--config requires a file path");
            }
            paths.push(PathBuf::from(path));
        } else if let Some(path) = arg.strip_prefix("--config=") {
            if path.is_empty() {
                bail!("--config requires a file path");
            }
            paths.push(PathBuf::from(path));
        } else {
            bail!("unrecognized argument: {arg}");
        }
    }
    Ok((!paths.is_empty()).then_some(paths))
}

fn discover_config_files(directory: &Path) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for entry in fs::read_dir(directory)
        .with_context(|| format!("failed to list {}", directory.display()))?
    {
        let entry = entry?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        let filename = entry.file_name();
        let filename = filename.to_string_lossy();
        if filename == "code-interpreter-mcp.toml"
            || filename.ends_with(".auto.code-interpreter-mcp.toml")
        {
            paths.push(entry.path());
        }
    }
    paths.sort();
    Ok(paths)
}

fn merge(base: &mut toml::Value, incoming: toml::Value) {
    match (base, incoming) {
        (toml::Value::Table(base), toml::Value::Table(incoming)) => {
            for (key, value) in incoming {
                match base.get_mut(&key) {
                    Some(existing) => merge(existing, value),
                    None => {
                        base.insert(key, value);
                    }
                }
            }
        }
        (base, incoming) => *base = incoming,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_limits_default_parse_merge_and_validate() {
        let defaults = AppConfig::from_files(&[]).unwrap();
        assert_eq!(defaults.input_limits.max_file_kb, 10 * 1024);
        assert_eq!(defaults.input_limits.max_total_file_kb, 50 * 1024);
        let mut base: toml::Value =
            toml::from_str("[input_limits]\nmax_file_kb = 40960\nmax_total_file_kb = 81920")
                .unwrap();
        merge(
            &mut base,
            toml::from_str("[input_limits]\nmax_file_kb = 51200").unwrap(),
        );
        let config: AppConfig = base.try_into().unwrap();
        assert_eq!(config.input_limits.max_file_kb, 51200);
        assert_eq!(config.input_limits.max_total_file_kb, 81920);
        assert!(config.input_limits.validate().is_ok());
        assert_eq!(
            toml::from_str::<AppConfig>("[input_limits]\nmax_file_kb = 1")
                .unwrap()
                .input_limits
                .max_total_file_kb,
            50 * 1024
        );
        for value in ["-1", "1.5", "'3'", "true"] {
            assert!(
                toml::from_str::<AppConfig>(&format!("[input_limits]\nmax_file_kb = {value}"))
                    .is_err()
            );
        }
        for key in ["input_limits.max_file_kb", "input_limits.max_total_file_kb"] {
            assert!(
                crate::config_reference::generate_config_reference()
                    .get(key)
                    .is_some()
            );
        }
    }

    #[test]
    fn search_min_hits_defaults_and_validates() {
        assert_eq!(AppConfig::from_files(&[]).unwrap().search.min_hits, 3);
        assert_eq!(
            toml::from_str::<AppConfig>("[search]")
                .unwrap()
                .search
                .min_hits,
            3
        );
        for min_hits in [0, 1, 3, 100] {
            let config: AppConfig =
                toml::from_str(&format!("[search]\nmin_hits = {min_hits}")).unwrap();
            assert_eq!(config.search.min_hits, min_hits);
        }
        for value in ["-1", "1.5", "'3'", "true"] {
            assert!(toml::from_str::<AppConfig>(&format!("[search]\nmin_hits = {value}")).is_err());
        }
        assert!(
            crate::config_reference::generate_config_reference()
                .get("search.min_hits")
                .is_some()
        );
    }

    #[test]
    fn host_header_settings_default_and_merge_independently_of_package_hosts() {
        let defaults = AppConfig::from_files(&[]).unwrap();
        assert!(defaults.http_allowed_hosts.is_empty());
        assert!(!defaults.skip_host_header_verification);
        let mut base = toml::from_str::<toml::Value>(
            "allowed_hosts = ['pypi.org']\nhttp_allowed_hosts = ['old.example.com']",
        )
        .unwrap();
        merge(
            &mut base,
            toml::from_str(
                "http_allowed_hosts = ['mcp.example.com:8000']\nskip_host_header_verification = true",
            )
            .unwrap(),
        );
        let config: AppConfig = base.try_into().unwrap();
        assert_eq!(config.allowed_hosts.unwrap(), ["pypi.org"]);
        assert_eq!(config.http_allowed_hosts, ["mcp.example.com:8000"]);
        assert!(config.skip_host_header_verification);
        assert!(toml::from_str::<AppConfig>("http_allowed_hosts = 'example.com'").is_err());
        assert!(toml::from_str::<AppConfig>("skip_host_header_verification = 'true'").is_err());
    }

    #[test]
    fn package_policy_defaults_and_overrides() {
        let defaults = AppConfig::from_files(&[]).unwrap();
        assert!(defaults.runtime_package_installation);
        assert!(defaults.preinstall_packages.is_empty());
        let mut base: toml::Value = toml::from_str(
            "preinstall_packages = ['numpy', 'pandas']\nruntime_package_installation = true",
        )
        .unwrap();
        merge(&mut base, toml::from_str(
            "preinstall_packages = ['snowballstemmer==3.0.1']\nruntime_package_installation = false"
        ).unwrap());
        let config: AppConfig = base.try_into().unwrap();
        assert!(!config.runtime_package_installation);
        assert_eq!(config.preinstall_packages, ["snowballstemmer==3.0.1"]);
        assert!(toml::from_str::<AppConfig>("runtime_package_installation = 'false'").is_err());
    }

    #[test]
    fn merges_nested_tables_and_replaces_values() {
        let mut base: toml::Value =
            toml::from_str("[subscription]\nname = 'base'\nshared = 'yes'\n").unwrap();
        let override_value = toml::from_str("[subscription]\nname = 'override'\n").unwrap();
        merge(&mut base, override_value);
        assert_eq!(base["subscription"]["name"].as_str(), Some("override"));
        assert_eq!(base["subscription"]["shared"].as_str(), Some("yes"));
    }

    #[test]
    fn explicit_paths_preserve_order() {
        let paths = parse_config_args(
            ["--config", "first.toml", "--config=second.toml"].map(str::to_owned),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            paths,
            [PathBuf::from("first.toml"), PathBuf::from("second.toml")]
        );
    }

    #[test]
    fn discovers_and_loads_multiple_files() {
        let directory = env::temp_dir().join(format!(
            "code-interpreter-mcp-config-test-{}",
            std::process::id()
        ));
        fs::create_dir(&directory).unwrap();
        fs::write(
            directory.join("code-interpreter-mcp.toml"),
            "bind_addr = '127.0.0.1:8000'\n",
        )
        .unwrap();
        fs::write(
            directory.join("zz.auto.code-interpreter-mcp.toml"),
            "bind_addr = '127.0.0.1:9000'\nallowed_hosts = ['example.com']\n",
        )
        .unwrap();
        fs::write(
            directory.join("ignored.toml"),
            "bind_addr = '127.0.0.1:7000'\n",
        )
        .unwrap();

        let paths = discover_config_files(&directory).unwrap();
        assert_eq!(paths.len(), 2);
        let config = AppConfig::from_files(&paths).unwrap();
        assert_eq!(config.bind_addr, "127.0.0.1:9000");
        assert_eq!(config.allowed_hosts, Some(vec!["example.com".to_owned()]));

        fs::remove_dir_all(directory).unwrap();
    }
}
