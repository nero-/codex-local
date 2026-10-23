use std::env;
use std::ffi::OsStr;
use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

const DEFAULT_BASE_URL: &str = "http://127.0.0.1:8000/v1";
const DEFAULT_CONTEXT_WINDOW: u64 = 131_072;
const DEFAULT_REASONING_EFFORT: &str = "high";

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ModelInfo>,
}

#[derive(Debug, Clone, Deserialize)]
struct ModelInfo {
    id: String,
    #[serde(default)]
    max_model_len: Option<u64>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    max_context_length: Option<u64>,
}

impl ModelInfo {
    fn context_window(&self) -> u64 {
        self.max_model_len
            .or(self.context_length)
            .or(self.max_context_length)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW)
            .max(4_096)
    }

    fn fallback(id: String) -> Self {
        Self {
            id,
            max_model_len: None,
            context_length: None,
            max_context_length: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
struct ClxSettings {
    base_url: Option<String>,
    model: Option<String>,
}

struct Options {
    base_url: String,
    model: Option<String>,
    list_models: bool,
    passthrough: Vec<OsString>,
}

fn main() {
    if let Err(err) = run() {
        eprintln!("clx: {err:#}");
        std::process::exit(1);
    }
}

fn run() -> Result<()> {
    // `--help`/`--version` must work before touching CLX_HOME.
    if let Some(first) = env::args_os().nth(1) {
        match first.to_string_lossy().as_ref() {
            "-h" | "--help" => {
                print_help();
                return Ok(());
            }
            "-V" | "--version" => {
                println!("clx {}", env!("CARGO_PKG_VERSION"));
                return Ok(());
            }
            _ => {}
        }
    }

    let home = clx_home()?;
    fs::create_dir_all(&home).with_context(|| format!("create {}", home.display()))?;
    let settings = load_settings(&home)?;
    let options = parse_args(&settings)?;
    let client = http_client()?;

    let mut models = match fetch_models(&client, &options.base_url) {
        Ok(models) => models,
        Err(err) => {
            if options.list_models {
                return Err(err);
            }
            if options.model.is_none() {
                return Err(err).with_context(|| {
                    format!(
                        "could not list models at {}/models; pass --model or set CLX_MODEL",
                        options.base_url.trim_end_matches('/')
                    )
                });
            }
            eprintln!(
                "clx: warning: could not list models at {}/models ({err:#}); using the requested model only",
                options.base_url.trim_end_matches('/')
            );
            Vec::new()
        }
    };

    if options.list_models {
        for model in &models {
            println!("{}\t{}", model.id, model.context_window());
        }
        return Ok(());
    }

    let selected = match options
        .model
        .clone()
        .or_else(|| models.first().map(|model| model.id.clone()))
    {
        Some(model) => model,
        None => bail!("no model selected: endpoint returned no models; pass --model or set CLX_MODEL"),
    };

    if !models.iter().any(|model| model.id == selected) {
        models.push(ModelInfo::fallback(selected.clone()));
    }

    let context = models
        .iter()
        .find(|model| model.id == selected)
        .map(ModelInfo::context_window)
        .unwrap_or(DEFAULT_CONTEXT_WINDOW);

    let catalog_path = home.join("model-catalog.json");
    let catalog = build_catalog(&models, &options.base_url);
    fs::write(&catalog_path, serde_json::to_vec_pretty(&catalog)?)
        .with_context(|| format!("write {}", catalog_path.display()))?;

    let config_path = home.join("config.toml");
    fs::write(&config_path, build_config(&selected, context, &catalog_path))
        .with_context(|| format!("write {}", config_path.display()))?;

    let codex = resolve_codex(&home)?;
    launch(&codex, &home, &options.base_url, &selected, &options.passthrough)
}

fn parse_args(settings: &ClxSettings) -> Result<Options> {
    let args: Vec<OsString> = env::args_os().skip(1).collect();
    let mut base_url = env_string(&["CLX_BASE_URL", "CODEX_LOCAL_BASE_URL", "CODEX_OSS_BASE_URL"])
        .or_else(|| settings.base_url.clone())
        .unwrap_or_else(|| DEFAULT_BASE_URL.to_string());
    let mut model = env_string(&["CLX_MODEL", "CODEX_LOCAL_MODEL"]).or_else(|| settings.model.clone());
    let mut passthrough = Vec::new();
    let mut list_models = false;
    let mut index = 0;

    while index < args.len() {
        let arg = args[index].to_string_lossy();
        match arg.as_ref() {
            "-h" | "--help" => {
                print_help();
                std::process::exit(0);
            }
            "-V" | "--version" => {
                println!("clx {}", env!("CARGO_PKG_VERSION"));
                std::process::exit(0);
            }
            "models" if passthrough.is_empty() => {
                list_models = true;
                index += 1;
            }
            "--endpoint" | "--base-url" => {
                index += 1;
                base_url = value_at(&args, index, arg.as_ref())?;
                index += 1;
            }
            "--model" | "-m" => {
                index += 1;
                model = Some(value_at(&args, index, arg.as_ref())?);
                index += 1;
            }
            "--" => {
                passthrough.extend(args[index + 1..].iter().cloned());
                break;
            }
            other if other.starts_with("--endpoint=") => {
                base_url = other["--endpoint=".len()..].to_string();
                index += 1;
            }
            other if other.starts_with("--base-url=") => {
                base_url = other["--base-url=".len()..].to_string();
                index += 1;
            }
            other if other.starts_with("--model=") => {
                model = Some(other["--model=".len()..].to_string());
                index += 1;
            }
            other if other.starts_with("-m") && other.len() > 2 && !other.starts_with("--") => {
                model = Some(other[2..].to_string());
                index += 1;
            }
            _ => {
                passthrough.extend(args[index..].iter().cloned());
                break;
            }
        }
    }

    // Codex can also receive a model after a subcommand, e.g. `clx exec -m foo`.
    if let Some(passthrough_model) = model_from_args(&passthrough) {
        model = Some(passthrough_model);
    }

    Ok(Options {
        base_url,
        model,
        list_models,
        passthrough,
    })
}

fn load_settings(home: &Path) -> Result<ClxSettings> {
    let path = home.join("clx.toml");
    let Ok(contents) = fs::read_to_string(&path) else {
        return Ok(ClxSettings::default());
    };
    toml::from_str(&contents).with_context(|| format!("parse {}", path.display()))
}

fn env_string(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

fn value_at(args: &[OsString], index: usize, flag: &str) -> Result<String> {
    args.get(index)
        .map(|value| value.to_string_lossy().to_string())
        .filter(|value| !value.is_empty())
        .with_context(|| format!("{flag} requires a value"))
}

fn model_from_args(args: &[OsString]) -> Option<String> {
    let mut index = 0;
    while index < args.len() {
        let arg = args[index].to_string_lossy();
        match arg.as_ref() {
            "--model" | "-m" => {
                return args
                    .get(index + 1)
                    .map(|value| value.to_string_lossy().to_string());
            }
            other if other.starts_with("--model=") => {
                return Some(other["--model=".len()..].to_string());
            }
            other if other.starts_with("-m") && other.len() > 2 && !other.starts_with("--") => {
                return Some(other[2..].to_string());
            }
            _ => {}
        }
        index += 1;
    }
    None
}

fn has_model_flag(args: &[OsString]) -> bool {
    model_from_args(args).is_some()
}

fn http_client() -> Result<Client> {
    Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")
}

fn fetch_models(client: &Client, base_url: &str) -> Result<Vec<ModelInfo>> {
    let url = format!("{}/models", base_url.trim_end_matches('/'));
    let mut request = client.get(&url);
    if let Some(api_key) = env_string(&["CLX_API_KEY", "CODEX_LOCAL_API_KEY"]) {
        request = request.bearer_auth(api_key);
    }
    let response = request.send().with_context(|| format!("GET {url}"))?;
    let response = response
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let parsed: ModelsResponse = response
        .json()
        .with_context(|| format!("parse model list from {url}"))?;
    Ok(parsed.data)
}

fn build_catalog(models: &[ModelInfo], base_url: &str) -> serde_json::Value {
    let entries = models
        .iter()
        .map(|model| {
            let context = model.context_window();
            json!({
                "slug": model.id,
                "display_name": model.id,
                "description": format!("Local model served by clx at {base_url}"),
                "default_reasoning_level": DEFAULT_REASONING_EFFORT,
                "supported_reasoning_levels": [
                    {"effort": "none", "description": "Off — no reasoning"},
                    {"effort": "low", "description": "Fast responses with lighter reasoning"},
                    {"effort": "medium", "description": "Balanced reasoning (sent as high on this backend)"},
                    {"effort": "high", "description": "Greater reasoning depth"},
                    {"effort": "xhigh", "description": "Extra high reasoning depth"},
                    {"effort": "max", "description": "Maximum reasoning depth"},
                ],
                "shell_type": "unified_exec",
                "visibility": "list",
                "supported_in_api": true,
                "priority": 1,
                "additional_speed_tiers": [],
                "service_tiers": [],
                "available_access_programs": {"cyber": ["standard"]},
                "availability_nux": null,
                "upgrade": null,
                "model_messages": null,
                "include_skills_usage_instructions": false,
                "include_plugin_usage_instructions": false,
                "include_apps_usage_instructions": false,
                "default_reasoning_summary": "detailed",
                "support_verbosity": true,
                "default_verbosity": "low",
                "apply_patch_tool_type": "freeform",
                "web_search_tool_type": "text_and_image",
                "truncation_policy": {"mode": "tokens", "limit": 10000},
                "supports_image_detail_original": false,
                "context_window": context,
                "max_context_window": context,
                "comp_hash": "3000",
                "effective_context_window_percent": 95,
                "experimental_supported_tools": ["send_user_message_async", "clock"],
                "input_modalities": ["text"],
                "supports_search_tool": false,
                "supports_experimental_context": true,
                "use_responses_lite": false,
                "supports_reasoning_effort_updates": true,
                "node_repl_auto_review_required": false,
                "node_repl_disabled": true,
                "tool_mode": "direct",
                "multi_agent_version": "v2",
                "multi_agent_reasoning_effort": DEFAULT_REASONING_EFFORT,
                "base_instructions": "You are Codex, a coding agent running locally against a model served by clx. Follow the developer and user instructions, and use the provided tools to complete tasks.",
            })
        })
        .collect::<Vec<_>>();
    json!({ "models": entries })
}

fn build_config(model: &str, context: u64, catalog_path: &Path) -> String {
    let auto_compact = context.saturating_mul(90) / 100;
    format!(
        "# Managed by clx. Regenerated on every launch.\n\
         model = {model}\n\
         model_provider = \"local\"\n\
         model_context_window = {context}\n\
         model_auto_compact_token_limit = {auto_compact}\n\
         model_reasoning_effort = \"{DEFAULT_REASONING_EFFORT}\"\n\
         model_reasoning_summary = \"detailed\"\n\
         show_raw_agent_reasoning = true\n\
         hide_agent_reasoning = false\n\
         check_for_update_on_startup = false\n\
         model_catalog_json = {catalog}\n",
        model = toml_string(model),
        catalog = toml_string(&catalog_path.display().to_string()),
    )
}

fn toml_string(value: &str) -> String {
    format!(
        "\"{}\"",
        value.replace('\\', "\\\\").replace('"', "\\\"")
    )
}

fn clx_home() -> Result<PathBuf> {
    if let Some(home) = env::var_os("CLX_HOME") {
        return Ok(PathBuf::from(home));
    }
    dirs::home_dir()
        .map(|home| home.join(".clx"))
        .context("could not determine home directory for CLX_HOME")
}

fn resolve_codex(home: &Path) -> Result<OsString> {
    if let Some(binary) = env::var_os("CLX_CODEX_BIN") {
        return Ok(binary);
    }
    let local_binary = home.join("bin").join("codex");
    if local_binary.is_file() {
        return Ok(local_binary.into_os_string());
    }
    if let Some(home_dir) = dirs::home_dir() {
        let installed = home_dir
            .join(".local")
            .join("lib")
            .join("clx")
            .join("codex");
        if installed.is_file() {
            return Ok(installed.into_os_string());
        }
    }
    if let Ok(current) = env::current_exe() {
        if let Some(parent) = current.parent() {
            let sibling = parent.join("codex");
            if sibling.is_file() {
                return Ok(sibling.into_os_string());
            }
        }
    }
    Ok(OsString::from("codex"))
}

fn launch(
    codex: &OsStr,
    home: &Path,
    base_url: &str,
    model: &str,
    passthrough: &[OsString],
) -> Result<()> {
    let mut command = Command::new(codex);
    command.env("CODEX_HOME", home);
    command.env("CLX_BASE_URL", base_url);
    command.args(["--oss", "--local-provider", "local"]);
    if !has_model_flag(passthrough) {
        command.arg("--model").arg(model);
    }
    command.args(passthrough);

    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        let error = command.exec();
        Err(error).with_context(|| format!("failed to launch {}", Path::new(codex).display()))
    }

    #[cfg(not(unix))]
    {
        let status = command.status().context("failed to launch codex")?;
        std::process::exit(status.code().unwrap_or(1));
    }
}

fn print_help() {
    println!(
        "clx — run Codex against a local OpenAI-compatible Responses endpoint\n\
         \n\
         Usage:\n\
           clx [--endpoint URL] [--model MODEL] [codex args...]\n\
           clx models\n\
         \n\
         Examples:\n\
           clx\n\
           clx --model glm-5.3\n\
           clx exec \"fix the failing test\"\n\
           clx models\n\
         \n\
         Environment:\n\
           CLX_BASE_URL      endpoint (default {DEFAULT_BASE_URL})\n\
           CLX_MODEL         default model\n\
           CLX_HOME          state/config directory (default ~/.clx)\n\
           CLX_API_KEY       optional bearer token\n\
           CLX_CODEX_BIN     codex binary override\n\
         \n\
         Config file:\n\
           ~/.clx/clx.toml   optional base_url / model defaults\n"
    );
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn model_flags_are_detected_after_subcommands() {
        let args = vec![
            OsString::from("exec"),
            OsString::from("-m"),
            OsString::from("glm-5.3"),
        ];
        assert_eq!(model_from_args(&args).as_deref(), Some("glm-5.3"));
        assert!(has_model_flag(&args));
    }

    #[test]
    fn context_window_prefers_max_model_len() {
        let model = ModelInfo {
            id: "qwen".to_string(),
            max_model_len: Some(1_048_576),
            context_length: Some(32_768),
            max_context_length: Some(65_536),
        };
        assert_eq!(model.context_window(), 1_048_576);
    }

    #[test]
    fn catalog_uses_discovered_context_and_slug() {
        let model = ModelInfo {
            id: "glm-5.3".to_string(),
            max_model_len: Some(200_000),
            context_length: None,
            max_context_length: None,
        };
        let catalog = build_catalog(&[model], "http://localhost:8000/v1");
        assert_eq!(catalog["models"][0]["slug"], "glm-5.3");
        assert_eq!(catalog["models"][0]["context_window"], 200_000);
        assert_eq!(catalog["models"][0]["tool_mode"], "direct");
    }

    #[test]
    fn config_enables_raw_reasoning_and_catalog() {
        let config = build_config("qwen", 131_072, Path::new("/tmp/catalog.json"));
        assert!(config.contains("show_raw_agent_reasoning = true"));
        assert!(config.contains("model_catalog_json = \"/tmp/catalog.json\""));
        assert!(config.contains("model_context_window = 131072"));
    }
}
