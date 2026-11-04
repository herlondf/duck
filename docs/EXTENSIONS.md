# Duck extensions

An extension is a separate program that Duck starts and talks to with JSON
messages, one per line: Duck writes to the extension's **stdin** and reads its
**stdout**. The extension never draws anything; it sends data (lists, details,
notifications) and Duck renders it with its own look. Anything written to
**stderr** goes to `%LOCALAPPDATA%\duck\ext-<name>.log` (Linux/macOS:
`~/.local/share/duck/ext-<name>.log`).

The official SDK is TypeScript (Node 18+, no dependencies):
[extensions/github/src/duck.ts](../extensions/github/src/duck.ts). The GitHub
extension in [extensions/github](../extensions/github) is the reference
implementation.

## Layout and install

```
~/.duck/extensions/<name>/
  duck-extension.json      manifest
  dist/index.js            your code (any language works; the manifest says how to start it)
```

```json
{
  "name": "github",
  "title": "GitHub",
  "icon": "github",
  "version": "0.1.0",
  "description": "Notifications, issues, PRs and repositories",
  "command": ["node", "dist/index.js"]
}
```

### Manifest fields for the Extensions menu

The Extensions menu (`Ctrl+Shift+P` → "Extensões") reads these optional fields to
enable/disable, configure and add accounts without editing Lua by hand:

```json
{
  "category": "Dev",
  "permissions": ["context", "paste", "clipboard"],
  "secrets": ["jira/trabalho"],
  "settings": [
    { "key": "poll_seconds", "title": "Atualizar a cada (s)", "type": "number", "default": 120 },
    { "key": "project", "title": "Projeto", "type": "string" },
    { "key": "only_mine", "title": "Só os meus", "type": "bool", "default": true },
    { "key": "repos", "title": "Repositórios (separados por vírgula)", "type": "list" }
  ],
  "accounts": {
    "key": "accounts",
    "title": "Contas",
    "fields": [
      { "key": "name", "title": "Nome curto (ex.: trabalho)" },
      { "key": "url", "title": "Endereço", "default": "https://" }
    ],
    "secret": "jira/{name}",
    "secret_title": "Token pessoal"
  }
}
```

- `settings` are written to `~/.duck-local.lua` as `config.extensions.<name>.<key>`.
  Types: `string`, `number`, `bool`, `list` (comma separated → array of strings).
- `accounts` is a list of tables at `config.extensions.<name>.<key>`; the wizard asks
  each field in order, then the secret (masked) and stores it with `duck secret set`
  under `secret` (`{field}` placeholders are replaced).
- `secrets`: extra secret names the extension may read besides `<name>/...`.
- `permissions`:
  - `context`: receives `{"type":"context","cwd","pane","domain","title"}` on start and
    whenever the active pane changes (to act on the pane's folder/repository);
  - `paste`: may send `{"type":"paste","text","enter"}` to paste into the active pane
    (multi-line or dangerous text still goes through Duck's confirmation);
  - `clipboard`: receives `{"type":"clipboard","text"}` when the user copies inside Duck;
  - `agents`: may ask for the open agent panes with `duck.agents()` (pane, tab,
    folder, state `working`/`blocked`/`done`/`idle`, session). Private panes are left out.

Extensions live in two places: `<duck>/extensions/<name>` (next to the installation,
shared by every OS user) and `~/.duck/extensions/<name>` (personal; same name wins).
Bundled extensions start disabled except the ones enabled in the config
(`extensions = { github = { enabled = true } }`), so idle ones cost no memory.

`command` runs with the extension folder as the working directory. Extensions
run inside the Duck server (`duckw server`), not the window: they start with the
server, keep running while the window is closed, and end when its stdin closes
(the SDK handles that). Each OS user has their own server, so extensions run as
that user, with their config, secrets and logins. A window that opens receives
the current views and status items from the server.

With no window open, `notify` shows as a Duck card in the corner of the screen,
with the theme from the config (its buttons still reach the extension) and `open-pane` opens a background tab, so a polling extension can
work on its own while Duck is running. Errors written to stderr go to
`%LOCALAPPDATA%\duck\ext-host.log`.

Settings come from `~/.duck.lua` and are passed in `init`:

```lua
extensions = {
  github = { accounts = { 'personal', 'work' }, poll_seconds = 60 },
  jira   = { enabled = false },            -- does not start
},
```

## Messages from Duck

| type | fields | when |
|---|---|---|
| `init` | `settings`, `duck: {version, os}` | right after start |
| `view-open` / `view-close` | `view` | the user opened/closed one of your views |
| `action` | `action`, `item`, `view` | a row or a button was clicked (`refresh` comes from the panel header) |
| `command` | `command` | a palette command you registered was chosen |
| `input` | `id`, `value` | answer to a `prompt` |

## Messages to Duck

| type | fields | effect |
|---|---|---|
| `register` | `views: [{id, title, icon}]`, `commands: [{id, title}]` | views get a title-bar icon and a palette entry; commands go to the palette |
| `view` | `id`, `sections: [{title, items}]`, `loading?`, `error?`, `title?` | content of a view (side panel) |
| `status` | `id`, `text`, `color?`, `view?` | title-bar item; clicking opens `view`; empty `text` removes it |
| `notify` | `title`, `body?`, `item?`, `actions?`, `color?`, `seconds?` | a card in Duck when the window is focused, otherwise the same card in the corner of the screen (Windows toast during full screen or presentations) |
| `detail` | `title`, `subtitle?`, `body`, `item?`, `actions?` | floating panel with scrollable text and buttons (first button is the primary one) |
| `prompt` | `id`, `title`, `value?` | asks for a line of text, answered with `input` |
| `show-view` | `id` | opens one of your views in the side panel |
| `open-url` | `url` | opens the browser |
| `open-pane` | `cmd: [...]`, `where?: "tab" \| "float"`, `cwd?`, `title?` | runs a command in a Duck pane |
| `flash` | `text` | short message in the corner |

An **item** is `{id, title, subtitle?, icon?, badge?, color?, unread?, actions?}`.
Clicking the row runs the action marked `primary` (or the first one); the other
actions show up as buttons on hover. Item ids are yours: encode whatever you
need (account, repository, number) and parse it back in `action`.

**Icons** are names (`github`, `bell`, `issue`, `issue-closed`, `pr`,
`pr-merged`, `pr-draft`, `repo`, `comment`, `tag`, `check`, `x`, `star`,
`eye`, `clock`, `user`, `branch`, `bug`, `rocket`, `warning`, `list`,
`refresh`, `jira`, `azure`) or any Nerd Font glyph. **Colors** are theme names
(`accent`, `done`, `working`, `blocked`, `muted`, `fg`) or `#rrggbb`.

## Running Claude Code from an extension

`duck.agent()` runs Claude Code headless (`claude -p`) and resolves with the
final answer. It uses the login of a Claude Code profile (Pro/Max plan), never
an API key: `ANTHROPIC_API_KEY` is removed from the agent's environment.

```ts
const r = await duck.agent({
  prompt: "Resuma as notícias de IA das últimas 6 horas em 3 frases",
  profile: "",                  // "" = ~/.claude, "work" = ~/.claude-work, or a path
  tools: ["WebSearch", "WebFetch"],
  maxTurns: 10,
});
if (r.ok) duck.notify({ title: "Notícias", body: r.text });
```

| option | meaning |
|---|---|
| `prompt` | the request, sent through stdin |
| `cwd` | folder the agent works in (default: the extension folder) |
| `profile` | Claude Code profile whose login is used |
| `tools` | tools allowed without asking (`"Bash(gh issue list:*)"` style); everything else is denied, since nobody is there to approve |
| `permissionMode` | `acceptEdits` lets it edit files in `cwd` |
| `maxTurns`, `model`, `system`, `resume`, `timeoutSec` | passed to Claude Code (`system` is appended to the system prompt; timeout defaults to 30 min) |

The result is `{ok, text, error?, sessionId?, turns?, durationMs?, costUsd?}`.
Pass `sessionId` back as `resume` to continue the same conversation. Start and
end of each run go to the extension log. Runs share the plan's usage limits
with your interactive sessions, so keep schedules sparse.

## Minimal extension (TypeScript)

```ts
import { duck, run } from "./duck";

run({
  init() {
    duck.register([{ id: "hello", title: "Hello", icon: "star" }]);
    duck.view("hello", [{ title: "Items", items: [
      { id: "1", title: "First item", subtitle: "click me", actions: [{ id: "open", title: "Open", primary: true }] },
    ] }]);
    duck.status("hello", "★ 1", { view: "hello" });
  },
  action(m) {
    if (m.action === "open") duck.detail({ title: `Item ${m.item}`, body: "Hello from an extension" });
  },
});
```

Build with `tsc`, copy `duck-extension.json` and `dist/` to
`~/.duck/extensions/hello/` and enable it in the extensions menu (turning it
off and on reloads it after a change). Reopening the window is not enough, since
extensions live in the server.

## Design notes

- **Accounts and secrets:** never store tokens in files. The GitHub extension
  reads them from the `gh` CLI (`gh auth token --user <login>`), which already
  keeps several accounts in the system keyring.
- **Cost:** poll with conditional requests (`If-Modified-Since`, ETags) and
  load heavy views only on `view-open`.
- **Isolation:** a crashing extension does not affect Duck; its views show the
  error and the log file explains why.
