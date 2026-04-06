use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize)]
pub struct SkillMeta {
    pub name: String,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: Option<String>,
    pub enabled: bool,
    pub content: String,
    pub path: PathBuf,
}

#[derive(Debug)]
pub struct SkillManager {
    skills: HashMap<String, Skill>,
}

impl SkillManager {
    /// Scan a directory for skill folders (each containing SKILL.md).
    pub fn load(skills_dir: &Path) -> Result<Self> {
        let mut skills = HashMap::new();

        if !skills_dir.exists() {
            tracing::info!("Skills directory not found: {}", skills_dir.display());
            return Ok(Self { skills });
        }

        for entry in std::fs::read_dir(skills_dir)
            .with_context(|| format!("Failed to read skills directory: {}", skills_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_file = path.join("SKILL.md");
            if !skill_file.exists() {
                continue;
            }

            match Self::parse_skill(&skill_file, &path) {
                Ok(skill) => {
                    tracing::info!("Loaded skill '{}' from {}", skill.name, path.display());
                    skills.insert(skill.name.clone(), skill);
                }
                Err(e) => {
                    tracing::warn!("Failed to parse skill at {}: {}", path.display(), e);
                }
            }
        }

        Ok(Self { skills })
    }

    /// Parse a SKILL.md file with YAML frontmatter.
    fn parse_skill(file_path: &Path, dir_path: &Path) -> Result<Skill> {
        let content = std::fs::read_to_string(file_path)
            .with_context(|| format!("Failed to read {}", file_path.display()))?;

        let (meta, body) = Self::parse_frontmatter(&content)?;

        Ok(Skill {
            name: meta.name,
            description: meta.description,
            enabled: meta.enabled,
            content: body.trim().to_string(),
            path: dir_path.to_path_buf(),
        })
    }

    /// Parse YAML frontmatter delimited by `---`.
    fn parse_frontmatter(content: &str) -> Result<(SkillMeta, String)> {
        let content = content.trim_start();

        if !content.starts_with("---") {
            // No frontmatter: use folder name as skill name
            let body = content.to_string();
            let meta = SkillMeta {
                name: String::new(), // will be overridden
                description: None,
                enabled: true,
            };
            return Ok((meta, body));
        }

        let rest = &content[3..];
        let end = rest
            .find("\n---")
            .with_context(|| "Frontmatter not closed with ---")?;

        let yaml = &rest[..end];
        let meta: SkillMeta = serde_yaml::from_str(yaml)
            .with_context(|| "Failed to parse skill frontmatter")?;

        let body = rest[end + 4..].to_string();

        Ok((meta, body))
    }

    /// Build the system prompt from all enabled skills.
    pub fn build_system_prompt(&self, base_prompt: &str) -> String {
        let enabled_skills: Vec<&Skill> = self.skills.values().filter(|s| s.enabled).collect();

        if enabled_skills.is_empty() {
            return base_prompt.to_string();
        }

        let mut prompt = base_prompt.to_string();
        prompt.push_str("\n\n# Skills\n");

        for skill in &enabled_skills {
            prompt.push_str(&format!("\n## {}\n", skill.name));
            if let Some(desc) = &skill.description {
                prompt.push_str(&format!("{}\n", desc));
            }
            prompt.push('\n');
            prompt.push_str(&skill.content);
            prompt.push('\n');
        }

        prompt
    }

    /// Enable a skill by name. Returns false if not found.
    pub fn enable(&mut self, name: &str) -> bool {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.enabled = true;
            true
        } else {
            false
        }
    }

    /// Disable a skill by name. Returns false if not found.
    pub fn disable(&mut self, name: &str) -> bool {
        if let Some(skill) = self.skills.get_mut(name) {
            skill.enabled = false;
            true
        } else {
            false
        }
    }

    /// List all skills with their status.
    pub fn list(&self) -> Vec<(&str, &str, bool)> {
        let mut result: Vec<_> = self
            .skills
            .values()
            .map(|s| (s.name.as_str(), s.description.as_deref().unwrap_or(""), s.enabled))
            .collect();
        result.sort_by(|a, b| a.0.cmp(b.0));
        result
    }
}
