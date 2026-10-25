//! Local model discovery for the codex-local fork.
//!
//! `codex models refresh` queries the configured local Responses endpoint and
//! rewrites the model catalog used by `/model`, so discovery lives in `codex`
//! itself rather than in a second launcher.

use anyhow::Context;
use anyhow::Result;
use anyhow::bail;
use clap::Args;
use clap::Subcommand;
use serde::Deserialize;
use serde_json::Value;
use serde_json::json;
use std::env;
use std::fs;
use std::path::PathBuf;
use std::time::Duration;

const DEFAULT_ENDPOINT: &str = "http://127.0.0.1:8000/v1";
const DEFAULT_CONTEXT_WINDOW: u64 = 131_072;

#[derive(Debug, Args)]
pub struct ModelsCommand {
    #[command(subcommand)]
    pub subcommand: ModelsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ModelsSubcommand {
    /// Query the local endpoint and rewrite the model catalog.
    Refresh(RefreshArgs),

    /// Print the models currently in the local catalog.
    List,
}

#[derive(Debug, Args)]
pub struct RefreshArgs {
    /// Local OpenAI-compatible endpoint; defaults to the active provider URL.
    #[arg(long)]
    pub endpoint: Option<String>,

    /// Catalog path; defaults to `model_catalog_json` (or CODEX_HOME/model-catalog.json).
    #[arg(long)]
    pub catalog: Option<PathBuf>,
}

pub async fn run(command: ModelsCommand) -> Result<()> {
    match command.subcommand {
        ModelsSubcommand::Refresh(args) => refresh(args).await,
        ModelsSubcommand::List => list(),
    }
}

#[derive(Debug, Deserialize)]
struct ModelsResponse {
    #[serde(default)]
    data: Vec<ApiModel>,
}

#[derive(Debug, Clone, Deserialize)]
struct ApiModel {
    id: String,
    #[serde(default)]
    max_model_len: Option<u64>,
    #[serde(default)]
    context_length: Option<u64>,
    #[serde(default)]
    max_context_length: Option<u64>,
}

impl ApiModel {
    fn context_window(&self) -> u64 {
        self.max_model_len
            .or(self.context_length)
            .or(self.max_context_length)
            .unwrap_or(DEFAULT_CONTEXT_WINDOW)
            .max(4_096)
    }
}

struct LocalSettings {
    endpoint: String,
    catalog_path: PathBuf,
}

fn codex_home() -> PathBuf {
    env::var_os("CODEX_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home_dir().join(".codex"))
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
}

fn load_settings(args: &RefreshArgs) -> Result<LocalSettings> {
    let home = codex_home();
    let config_path = home.join("config.toml");
    let config: Value = fs::read_to_string(&config_path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or(Value::Null);

    let configured_catalog = config
        .get("model_catalog_json")
        .and_then(Value::as_str)
        .map(expand_home)
        .unwrap_or_else(|| home.join("model-catalog.json"));
    let catalog_path = args.catalog.clone().unwrap_or(configured_catalog);

    let provider_id = config
        .get("model_provider")
        .and_then(Value::as_str)
        .unwrap_or("openai");
    let provider_base_url = config
        .get("model_providers")
        .and_then(|providers| providers.get(provider_id))
        .and_then(|provider| provider.get("base_url"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned);
    let endpoint = args
        .endpoint
        .clone()
        .or_else(|| env_string(&["CLX_BASE_URL", "CODEX_LOCAL_BASE_URL", "CODEX_OSS_BASE_URL"]))
        .or(provider_base_url)
        .unwrap_or_else(|| DEFAULT_ENDPOINT.to_string());

    Ok(LocalSettings {
        endpoint,
        catalog_path,
    })
}

fn expand_home(value: &str) -> PathBuf {
    if let Some(rest) = value.strip_prefix("~/") {
        home_dir().join(rest)
    } else {
        PathBuf::from(value)
    }
}

fn env_string(names: &[&str]) -> Option<String> {
    names.iter().find_map(|name| {
        env::var(name)
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    })
}

async fn refresh(args: RefreshArgs) -> Result<()> {
    let settings = load_settings(&args)?;
    let url = format!("{}/models", settings.endpoint.trim_end_matches('/'));
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(15))
        .build()
        .context("build HTTP client")?;
    let mut request = client.get(&url);
    if let Some(api_key) = env_string(&["CLX_API_KEY", "CODEX_LOCAL_API_KEY"]) {
        request = request.bearer_auth(api_key);
    }
    let response = request
        .send()
        .await
        .with_context(|| format!("GET {url}"))?
        .error_for_status()
        .with_context(|| format!("GET {url}"))?;
    let response: ModelsResponse = response
        .json()
        .await
        .with_context(|| format!("parse model list from {url}"))?;
    if response.data.is_empty() {
        bail!("no models found at {url}");
    }

    let catalog = build_catalog(&response.data, &settings.endpoint);
    if let Some(parent) = settings.catalog_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create {}", parent.display()))?;
    }
    fs::write(
        &settings.catalog_path,
        serde_json::to_vec_pretty(&catalog).context("serialize model catalog")?,
    )
    .with_context(|| format!("write {}", settings.catalog_path.display()))?;

    println!(
        "refreshed {} model(s) from {} into {}",
        response.data.len(),
        settings.endpoint,
        settings.catalog_path.display()
    );
    for model in &response.data {
        println!("{}\t{}", model.id, model.context_window());
    }
    Ok(())
}

fn list() -> Result<()> {
    let home = codex_home();
    let config_path = home.join("config.toml");
    let config: Value = fs::read_to_string(&config_path)
        .ok()
        .and_then(|text| toml::from_str(&text).ok())
        .unwrap_or(Value::Null);
    let catalog_path = config
        .get("model_catalog_json")
        .and_then(Value::as_str)
        .map(expand_home)
        .unwrap_or_else(|| home.join("model-catalog.json"));
    let catalog: Value = serde_json::from_slice(
        &fs::read(&catalog_path)
            .with_context(|| format!("read {}", catalog_path.display()))?,
    )
    .with_context(|| format!("parse {}", catalog_path.display()))?;
    let models = catalog
        .get("models")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if models.is_empty() {
        println!("no models in {}", catalog_path.display());
        return Ok(());
    }
    for model in models {
        let slug = model.get("slug").and_then(Value::as_str).unwrap_or("?");
        let context = model
            .get("context_window")
            .and_then(Value::as_u64)
            .unwrap_or_default();
        println!("{}\t{}", slug, context);
    }
    Ok(())
}

fn build_catalog(models: &[ApiModel], endpoint: &str) -> Value {
    let entries = models
        .iter()
        .map(|model| {
            let context = model.context_window();
            json!({
                "slug": model.id,
                "display_name": model.id,
                "description": format!("Local model served by codex at {endpoint}"),
                "default_reasoning_level": "high",
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
                "supports_image_detail_original": true,
                "context_window": context,
                "max_context_window": context,
                "comp_hash": "3000",
                "effective_context_window_percent": 95,
                "experimental_supported_tools": ["send_user_message_async", "clock"],
                "input_modalities": ["text", "image"],
                "supports_search_tool": false,
                "supports_experimental_context": true,
                "use_responses_lite": false,
                "supports_reasoning_effort_updates": true,
                "node_repl_auto_review_required": false,
                "node_repl_disabled": true,
                "tool_mode": "direct",
                "multi_agent_reasoning_effort": "high",
                "base_instructions": "You are Codex, a coding agent running locally against a model served by codex-local. Follow the developer and user instructions, and use the provided tools to complete tasks.",
            })
        })
        .collect::<Vec<_>>();
    json!({ "models": entries })
}
