mod config;
mod llm_client;
mod mcp_client;
mod tool_runner;
mod types;

use anyhow::Result;
use clap::Parser;
use config::AppConfig;
use futures::future::join_all;
use llm_client::LlmClient;
use tool_runner::ToolRegistry;
use types::ToolCall;
use types::ChatMessage;

#[derive(Parser)]
#[command(name = "tiny-claw", about = "A minimal agent CLI")]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,
}

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("tiny_claw=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();
    let config = AppConfig::load(cli.config.as_deref())?;
    let api_key = config.api_key()?;

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

    // Conversation history
    let mut conversation: Vec<ChatMessage> = vec![ChatMessage::System {
        content: "You are a helpful assistant. Use tools when needed.".to_string(),
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
                    conversation.truncate(1); // Keep system message
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
                "/help" => {
                    println!("Commands:");
                    println!("  /quit   - Exit the program");
                    println!("  /clear  - Clear conversation history");
                    println!("  /tools  - List available tools");
                    println!("  /help   - Show this help");
                    continue;
                }
                _ => {
                    println!("Unknown command: {}. Type /help for available commands.", trimmed);
                    continue;
                }
            }
        }

        // Add user message
        conversation.push(ChatMessage::User {
            content: trimmed.to_string(),
        });

        // Agent loop: keep calling LLM until we get a final text response
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
                    // Remove the user message so we can retry
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

                        // Execute all tool calls in parallel
                        let results: Vec<(ToolCall, std::result::Result<String, anyhow::Error>)> = join_all(
                            calls.iter().map(|tc| async {
                                (tc.clone(), registry.execute_tool_call(tc).await)
                            }),
                        )
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
                    // Loop back to call LLM with tool results
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

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.len() <= max_len {
        s.to_string()
    } else {
        format!("{}...", &s[..max_len])
    }
}
