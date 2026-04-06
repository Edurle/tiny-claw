mod config;
mod llm_client;
mod mcp_client;
mod skills;
mod tool_runner;
mod types;

use anyhow::Result;
use clap::Parser;
use config::AppConfig;
use futures::future::join_all;
use llm_client::LlmClient;
use skills::SkillManager;
use std::path::PathBuf;
use tool_runner::ToolRegistry;
use types::{ChatMessage, ToolCall};

#[derive(Parser)]
#[command(name = "tiny-claw", about = "A minimal agent CLI")]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tiny_claw=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let config = AppConfig::load(cli.config.as_deref())?;
    let api_key = config.api_key()?;

    // Load skills
    let skills_dir = PathBuf::from(&config.skills.dir);
    let mut skill_mgr = SkillManager::load(&skills_dir)?;
    let skill_count = skill_mgr.list().len();
    let enabled_count = skill_mgr.list().iter().filter(|(_, _, on)| *on).count();
    if skill_count > 0 {
        println!(
            "Loaded {} skills ({} enabled).",
            skill_count, enabled_count
        );
    }

    // Connect to MCP servers
    println!("Connecting to MCP servers...");
    let registry = ToolRegistry::build(&config.mcp_servers).await?;
    let tools = registry.get_openai_tools();
    println!(
        "Ready. {} tools available from {} servers.",
        tools.len(),
        config.mcp_servers.len()
    );

    let llm = LlmClient::new(&config.llm, api_key)?;

    // Build system prompt from skills
    let base_prompt = "You are a helpful assistant. Use tools when needed.";
    let system_content = skill_mgr.build_system_prompt(base_prompt);
    let mut conversation: Vec<ChatMessage> = vec![ChatMessage::System {
        content: system_content,
    }];

    let mut rl = rustyline::DefaultEditor::new()?;

    println!("Type /help for commands, /quit to exit.\n");

    loop {
        let line = match rl.readline("tiny-claw> ") {
            Ok(line) => line,
            Err(rustyline::error::ReadlineError::Interrupted) => {
                println!("(Ctrl+C, use /quit to exit)");
                continue;
            }
            Err(rustyline::error::ReadlineError::Eof) => {
                println!("Goodbye!");
                break;
            }
            Err(e) => {
                anyhow::bail!("Readline error: {}", e);
            }
        };

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        rl.add_history_entry(&line)?;

        // Handle REPL commands
        if trimmed.starts_with('/') {
            match trimmed {
                "/quit" | "/exit" => {
                    println!("Goodbye!");
                    break;
                }
                "/clear" => {
                    rebuild_system_prompt(&mut conversation, &skill_mgr, base_prompt);
                    println!("Conversation cleared.");
                    continue;
                }
                "/tools" => {
                    let names = registry.tool_names();
                    if names.is_empty() {
                        println!("No tools available.");
                    } else {
                        println!("Available tools:");
                        for name in names {
                            println!("  - {}", name);
                        }
                    }
                    continue;
                }
                "/skills" => {
                    let skills = skill_mgr.list();
                    if skills.is_empty() {
                        println!("No skills loaded.");
                    } else {
                        println!("Skills:");
                        for (name, desc, enabled) in skills {
                            let status = if enabled { "ON" } else { "OFF" };
                            if desc.is_empty() {
                                println!("  [{}] {}", status, name);
                            } else {
                                println!("  [{}] {} - {}", status, name, desc);
                            }
                        }
                    }
                    continue;
                }
                "/help" => {
                    println!("Commands:");
                    println!("  /quit          - Exit the program");
                    println!("  /clear         - Clear conversation history");
                    println!("  /tools         - List available tools");
                    println!("  /skills        - List loaded skills");
                    println!("  /skill on <n>  - Enable a skill");
                    println!("  /skill off <n> - Disable a skill");
                    println!("  /help          - Show this help");
                    continue;
                }
                _ => {
                    // Handle /skill on/off <name>
                    if let Some(rest) = trimmed.strip_prefix("/skill ") {
                        handle_skill_command(rest, &mut skill_mgr, &mut conversation, base_prompt);
                        continue;
                    }
                    println!(
                        "Unknown command: {}. Type /help for available commands.",
                        trimmed
                    );
                    continue;
                }
            }
        }

        // Add user message
        conversation.push(ChatMessage::User {
            content: trimmed.to_string(),
        });

        // Agent loop
        loop {
            let tools_ref = if tools.is_empty() {
                None
            } else {
                Some(tools.as_slice())
            };

            let response = match llm.chat(&conversation, tools_ref).await {
                Ok(r) => r,
                Err(e) => {
                    eprintln!("Error: {}", e);
                    conversation.pop();
                    break;
                }
            };

            let choice = &response.choices[0];
            let assistant_msg = choice.message.clone();
            conversation.push(assistant_msg);

            match choice.finish_reason.as_str() {
                "stop" => {
                    if let ChatMessage::Assistant {
                        content: Some(text),
                        ..
                    } = &choice.message
                    {
                        println!("\n{}\n", text);
                    }
                    break;
                }
                "tool_calls" => {
                    if let ChatMessage::Assistant {
                        tool_calls: Some(calls),
                        ..
                    } = &choice.message
                    {
                        for tc in calls {
                            println!(
                                "[Tool Call] {}({})",
                                tc.function.name, tc.function.arguments
                            );
                        }

                        let results: Vec<(ToolCall, std::result::Result<String, anyhow::Error>)> =
                            join_all(calls.iter().map(|tc| async {
                                (tc.clone(), registry.execute_tool_call(tc).await)
                            }))
                            .await;

                        for (tc, result) in results {
                            match result {
                                Ok(content) => {
                                    println!("[Tool Result] {}", truncate_str(&content, 200));
                                    conversation.push(ChatMessage::Tool {
                                        tool_call_id: tc.id,
                                        content,
                                    });
                                }
                                Err(e) => {
                                    let err_msg = format!("Error: {}", e);
                                    eprintln!("[Tool Error] {}", e);
                                    conversation.push(ChatMessage::Tool {
                                        tool_call_id: tc.id,
                                        content: err_msg,
                                    });
                                }
                            }
                        }
                    }
                    continue;
                }
                other => {
                    eprintln!("Unexpected finish_reason: {}", other);
                    break;
                }
            }
        }
    }

    Ok(())
}

fn handle_skill_command(
    rest: &str,
    skill_mgr: &mut SkillManager,
    conversation: &mut Vec<ChatMessage>,
    base_prompt: &str,
) {
    let parts: Vec<&str> = rest.trim().splitn(2, |c: char| c.is_whitespace()).collect();
    if parts.len() != 2 {
        println!("Usage: /skill on <name> or /skill off <name>");
        return;
    }

    let action = parts[0];
    let name = parts[1];

    let found = match action {
        "on" => skill_mgr.enable(name),
        "off" => skill_mgr.disable(name),
        _ => {
            println!("Usage: /skill on <name> or /skill off <name>");
            return;
        }
    };

    if !found {
        println!("Skill '{}' not found.", name);
        return;
    }

    rebuild_system_prompt(conversation, skill_mgr, base_prompt);
    let status = if action == "on" { "enabled" } else { "disabled" };
    println!("Skill '{}' {}.", name, status);
}

fn rebuild_system_prompt(
    conversation: &mut Vec<ChatMessage>,
    skill_mgr: &SkillManager,
    base_prompt: &str,
) {
    let new_prompt = skill_mgr.build_system_prompt(base_prompt);
    if !conversation.is_empty() {
        if let ChatMessage::System { content } = &mut conversation[0] {
            *content = new_prompt;
        }
    } else {
        conversation.push(ChatMessage::System { content: new_prompt });
    }
    conversation.truncate(1);
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}
