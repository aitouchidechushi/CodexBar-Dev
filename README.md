# Win-CodexBar

> **CodexBar-Dev publication notice:** main contains the current 0.46.0 / build 87 development snapshot. The separately shared, owner-confirmed 0.45.2 executable is an older Debug build, not a build of this source snapshot. See [usage and collaboration instructions](PUBLICATION.md) before downloading or developing. Existing upstream release guidance below does not certify this snapshot as a stable release.

[English](README.md) | [简体中文](README.zh-CN.md)

Win-CodexBar is a Windows desktop quota monitor for AI coding services. It keeps provider usage, reset times, account-level monthly limits, and independently managed API keys visible in the main window, tray surfaces, FloatBar, and floating quota window.

Built with Tauri, React, and Rust. Inspired by the original macOS [CodexBar](https://github.com/steipete/CodexBar).

<table>
  <tr>
    <td align="center">
      <img src="docs/images/tray-panel.png" width="320" alt="Win-CodexBar tray panel showing provider usage"/>
      <br/>
      Tray panel
    </td>
    <td align="center">
      <img src="docs/images/settings-providers.png" width="520" alt="Win-CodexBar provider settings"/>
      <br/>
      Provider settings
    </td>
  </tr>
</table>

## What It Does

- Tracks quota and usage returned by each provider, including session or 5-hour limits, weekly limits, real monthly limits, credits, and reset times where available.
- Keeps multiple credentials for the same provider separate. One failing credential does not hide or overwrite the others.
- Shows the same quota model in the main window, tray-hover window, FloatBar, and floating quota window.
- Gives monthly quota a distinct visual treatment and groups it with the account and API key or keys that were verifiably linked to that account.
- Keeps API keys without a verified account link visible as independent entries; it never guesses a relationship.
- Refreshes usage in the background using the application's existing refresh schedule.
- Supports Chinese, English, Japanese, Korean, and Spanish interfaces.

Provider capabilities differ. Win-CodexBar displays only quota periods and balances that the provider actually returns. A weekly window, billing total, or MCP allowance is not relabeled as a model monthly quota.

## Kimi Monthly Quota from an Existing Browser Login

Win-CodexBar can read the Kimi accounts already signed in to Microsoft Edge and Google Chrome and display their account-level monthly quota.

1. Enable the feature and approve the one-time, read-only browser-account access request.
2. Win-CodexBar reads the signed-in Kimi account identity and monthly quota. No browser extension is required, and no additional Kimi login is required while the browser session remains valid.
3. The browser page is not controlled or refreshed. The latest renewal token needed for later quota refreshes is protected by Windows Credential Manager.
4. Each account's monthly quota is stored and displayed once. It is grouped with one or more Kimi API keys only when verified identity data establishes the relationship.
5. Each linked API key keeps its own 5-hour and weekly quota. Unlinked keys remain separate and clearly marked.

Multiple Edge and Chrome profiles and multiple Kimi accounts are supported. Closing Win-CodexBar or restarting Windows does not normally require consent again. If Kimi invalidates the login or renewal token, the app reports that the account needs attention instead of silently attaching it to another key.

The existing **Log in to a new Kimi account** flow inside Win-CodexBar remains available as an optional alternative. Browser-based monthly quota access does not remove or change that flow.

## Quota Behavior Highlights

| Provider or quota type | Display behavior |
| --- | --- |
| Codex | Session and weekly usage, plus credits or related data when available |
| Claude | 5-hour and weekly quota when returned by the account |
| GLM (Zhipu BigModel / z.ai) | API-key model quota uses Rate Limit (5-hour) and Weekly windows; a separate MCP monthly allowance is not presented as model-API monthly quota |
| Kimi API key | Independent 5-hour and weekly quota for every key |
| Kimi browser account | Account-level monthly quota, strictly grouped with verified matching Kimi API key or keys |
| Other providers with a real monthly limit | Monthly quota is shown with the same monthly-quota strategy when the upstream response provides a finite monthly window and stable account identity |

Temporary monthly-quota refresh failures keep the last valid monthly result visible with an error state; they do not replace independent key quotas.

## Supported Providers

The current provider registry contains **62 active integrations**:

<details>
<summary>Show the complete provider list</summary>

Codex, Claude, Cursor, Factory, Gemini, Antigravity, Copilot, GLM (Zhipu BigModel / z.ai), MiniMax, Kiro, Vertex AI, Augment, OpenCode, Kimi, Amp, Warp, Ollama, Azure OpenAI, T3 Chat, OpenRouter, JetBrains AI, Alibaba, Alibaba Token Plan, NanoGPT, Infini, Perplexity, Abacus AI, Mistral, OpenCode Go, Kilo, AWS Bedrock, Codebuff, DeepSeek, DeepInfra, ai&, Windsurf, Manus, Xiaomi MiMo, Doubao, Command Code, Crof, StepFun, Venice, OpenAI API, Grok, ElevenLabs, Deepgram, Groq, LLM Proxy, Chutes, LiteLLM, Poe, Devin, Zed, Qoder, Sakana AI, sub2api, Wayfinder, ZenMux, ClinePass, LongCat, and Neuralwatt.

</details>

Authentication methods and available metrics depend on the provider, account type, plan, region, and upstream API. Legacy Kimi K2 and CrossModel identifiers remain resolvable for existing configurations but are not presented as active supported providers.

## Install a Stable Release

Only stable Windows releases are intended for public distribution. Open this repository's **Releases** page and download exactly one of:

- \`CodexBar-<version>-Setup.exe\` — installer for normal Windows use.
- \`CodexBar-<version>-portable.exe\` — portable executable; no installation required.

Each release also includes one matching \`.sha256\` sidecar for each executable. Verify a download in PowerShell:

\`\`\`powershell
Get-FileHash -Algorithm SHA256 .\CodexBar-<version>-Setup.exe
\`\`\`

A stable release should contain one authoritative installer, one authoritative portable executable, and their two hashes. Debug executables, duplicate binaries, old rebuilds, test outputs, and local proof files are not release assets. Historical stable releases may remain available, but only the newest stable release is marked **Latest**.

The About page can report that a newer stable version exists. It does not silently download an update or force installation.

## First Run

1. Launch the installed or portable executable.
2. Open **Settings → Providers** and enable the providers you use.
3. Add or import the credentials requested by each provider.
4. Add multiple API keys separately when you want each key's quota shown independently.
5. To display Kimi account monthly quota, open **Settings → Kimi Accounts** and choose browser-account access or the optional in-app Kimi login.

## Privacy and Local Data

- Usage data and credentials remain local unless a provider request is needed to retrieve usage.
- App-managed API keys, manual cookies, and account tokens use the application's protected local credential storage.
- Kimi browser renewal tokens are protected with Windows Credential Manager.
- Browser-account access is read-only and starts only after user consent.
- Raw API keys, cookies, Bearer tokens, OAuth tokens, passwords, and verification codes must not appear in diagnostics or public release files.
- Win-CodexBar does not control or refresh Edge or Chrome pages to update Kimi monthly quota.

## Build from Source

Requirements:

- Windows 10 or 11
- Rust stable with the \`x86_64-pc-windows-msvc\` target
- Microsoft Visual Studio Build Tools with **Desktop development with C++**
- Node.js 20+ and pnpm 10

\`\`\`powershell
cd apps\desktop-tauri
pnpm install --frozen-lockfile
pnpm run tauri:build
\`\`\`

The release executable is written to \`target\release\codexbar-desktop-tauri.exe\`. This direct build is not the complete public-release process; installer packaging, hashing, clean-checkout verification, and smoke testing are required before publishing.

See [Building from Source](docs/BUILDING.md), [Windows notes](docs/WSL.md), [cookie handling](docs/COOKIES.md), and [release checks](docs/release/ci-cd.md) for details.

## Supported Languages

- English
- Simplified Chinese
- Traditional Chinese
- Japanese
- Korean
- Spanish (Mexico)

## Credits

- Original macOS project: [steipete/CodexBar](https://github.com/steipete/CodexBar), created by Peter Steinberger
- Windows desktop port and provider integrations: Win-CodexBar contributors

## License

Released under the [MIT License](LICENSE).
