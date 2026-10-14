import process from 'node:process'

// Test subprocesses must use fixture credentials, even when launched by a live
// coding agent whose environment includes a manager authentication socket.
export function isolateTestCredentials() {
  for (const key of ['CAIRN_AUTH_SOCKET', 'CLAUDE_SECURESTORAGE_CONFIG_DIR', 'CAIRN_MCP_RUN_TOKEN', 'CODEX_HOME', 'OPENAI_API_KEY', 'CODEX_API_KEY', 'CLAUDE_CONFIG_DIR', 'CLAUDE_CODE_OAUTH_TOKEN', 'ANTHROPIC_API_KEY', 'ANTHROPIC_AUTH_TOKEN', 'CAIRN_OPENCODE_HOME', 'OPENCODE_AUTH_CONTENT', 'OPENCODE_CONFIG_CONTENT', 'OPENCODE_CONFIG_DIR', 'OPENCODE_CONFIG', 'OPENCODE_SERVER_PASSWORD', 'OPENROUTER_API_KEY', 'GEMINI_API_KEY', 'GOOGLE_GENERATIVE_AI_API_KEY', 'GROQ_API_KEY', 'DEEPSEEK_API_KEY', 'MISTRAL_API_KEY', 'XAI_API_KEY'])
    delete process.env[key]
}
