use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Deserialize, Serialize)]
pub struct AppConfig {
    pub llm: LlmConfig,
    #[serde(default)]
    pub mcp_servers: Vec<McpServerConfig>,
    #[serde(default)]
    pub skills: SkillsConfig,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SkillsConfig {
    #[serde(default = "default_skills_dir")]
    pub dir: String,
}

impl Default for SkillsConfig {
    fn default() -> Self {
        Self {
            dir: default_skills_dir(),
        }
    }
}

fn default_skills_dir() -> String {
    dirs::config_dir()
        .map(|p| p.join("tiny-claw").join("skills").to_string_lossy().to_string())
        .unwrap_or_else(|| "~/.config/tiny-claw/skills".to_string())
}

#[derive(Debug, Deserialize, Serialize)]
pub struct LlmConfig {
    pub api_endpoint: String,
    pub model: String,
    #[serde(default = "default_api_key_env")]
    pub api_key_env: String,
    #[serde(default = "default_temperature")]
    pub temperature: f32,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct McpServerConfig {
    pub name: String,
    pub url: String,
}

fn default_api_key_env() -> String {
    "OPENAI_API_KEY".to_string()
}

fn default_temperature() -> f32 {
    0.7
}

impl AppConfig {
    pub fn load(config_path: Option<&str>) -> Result<Self> {
        let path = match config_path {
            Some(p) => PathBuf::from(p),
            None => default_config_path()?,
        };

        if !path.exists() {
            anyhow::bail!(
                "Config file not found at {}\n\
                 Create it with:\n\
                 mkdir -p ~/.config/tiny-claw\n\
                 cat > ~/.config/tiny-claw/config.toml << 'EOF'\n\
                 [llm]\n\
                 api_endpoint = \"https://api.openai.com\"\n\
                 model = \"gpt-4o\"\n\
                 temperature = 0.7\n\
                 \n\
                 [[mcp_servers]]\n\
                 name = \"example\"\n\
                 url = \"http://localhost:3001/sse\"\n\
                 EOF",
                path.display()
            );
        }

        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read config file: {}", path.display()))?;
        let config: Self = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config file: {}", path.display()))?;
        Ok(config)
    }

    pub fn api_key(&self) -> Result<String> {
        std::env::var(&self.llm.api_key_env).with_context(|| {
            format!(
                "API key not found. Set the {} environment variable.",
                self.llm.api_key_env
            )
        })
    }
}

fn default_config_path() -> Result<PathBuf> {
    dirs::config_dir()
        .map(|p| p.join("tiny-claw").join("config.toml"))
        .context("Could not determine config directory")
}
