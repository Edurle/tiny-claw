# tiny-claw

A minimal Rust agent CLI with MCP and tool calling support.

## Features

- OpenAI-compatible API via raw HTTP requests (no SDK dependency)
- MCP server integration via SSE transport
- Parallel tool calling
- Interactive REPL with history

## Usage

1. Create config file `~/.config/tiny-claw/config.toml`:

```toml
[llm]
api_endpoint = "https://api.openai.com"
model = "gpt-4o"
temperature = 0.7

[[mcp_servers]]
name = "my-server"
url = "http://localhost:3001/sse"
```

2. Set API key:

```bash
export OPENAI_API_KEY=sk-...
```

3. Run:

```bash
cargo run
```

## REPL Commands

| Command  | Description             |
|----------|-------------------------|
| `/quit`  | Exit the program        |
| `/clear` | Clear conversation      |
| `/tools` | List available tools    |
| `/help`  | Show help               |

## License

MIT
