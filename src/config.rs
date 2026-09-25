use std::{
    collections::BTreeMap,
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};
use facet::Facet;
use serde::Deserialize;

const DEFAULT_BIND_ADDRESS: &str = "127.0.0.1:8000";

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
    pub code_snippets: CodeSnippetsConfig,
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
        Ok(config)
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
