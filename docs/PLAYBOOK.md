# Playbook: install and configure Duck

Step by step to get Duck ready: installation, checks, extensions (one recipe
each) and routines. [Versão em português](PLAYBOOK.pt-BR.md).

Duck's interface is in Portuguese. Menu and command names below are quoted as
they appear on screen, with a translation the first time.

- [1. Install](#1-install)
- [2. First use](#2-first-use)
- [3. Extensions: the basics](#3-extensions-the-basics)
- [4. Recipes per extension](#4-recipes-per-extension)
- [5. Routines](#5-routines)
- [6. Messages between computers](#6-messages-between-computers)
- [7. Troubleshooting](#7-troubleshooting)

---

## 1. Install

### Windows: quick path

In PowerShell 7 (`pwsh`):

```powershell
# Prerequisites (skip what you already have)
winget install Rustlang.Rustup
winget install Microsoft.VisualStudio.2022.BuildTools --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
winget install OpenJS.NodeJS.LTS       # extensions
winget install Git.Git
winget install Microsoft.PowerShell    # if you don't have pwsh 7 yet

# Duck
git clone https://github.com/herlondf/duck.git
cd duck
pwsh -File .\install.ps1 -All
```

Open a new terminal after `winget` so the new commands are on the PATH.

`-All` does everything at once:

| Option | What it does |
|---|---|
| (always) | Builds. Puts `duck`/`duckw` in `.\bin` and on the PATH. Creates `~/.duck.lua`. Adds a Start menu shortcut. |
| `-Boot` | Opens Duck at logon, restoring the last checkpoint. |
| `-ClaudeHooks` | Registers Duck's hooks in every `~/.claude*/settings.json`, so it knows each agent's exact state. Also turns on Duck's statusline where there is none yet: it feeds "Claude usage" and the Routines usage limit. |
| `-Extensions` | Builds the bundled extensions. They stay off until you turn them on. |
| `-SkipBuild` | Reuses the binary already built (handy to redo only extensions or hooks). |

The first build takes about 5 minutes. The installer stops with the install
command when Rust, the MSVC Build Tools or Node is missing.

### Linux and macOS

```bash
git clone https://github.com/herlondf/duck.git && cd duck
./install.sh --all
```

The installer offers to install what is missing: Rust (rustup, in your home)
and the build packages (with `sudo`, through apt, dnf, pacman or zypper; on
macOS it asks you to run `xcode-select --install`). It also adds `~/.local/bin`
to the PATH and the shell integration to `~/.bashrc` / `~/.zshrc`, in a
`# >>> duck >>>` block that reinstalls replace instead of duplicate.
`--yes` answers yes to everything; `--no-shell` leaves the rc files alone.

Manual packages, if you prefer:

```bash
sudo apt install build-essential pkg-config libxkbcommon-dev libwayland-dev libx11-dev   # Debian/Ubuntu
sudo dnf install gcc pkgconf-pkg-config libxkbcommon-devel wayland-devel libX11-devel    # Fedora
sudo pacman -S --needed base-devel pkgconf libxkbcommon wayland libx11                    # Arch
```

Windows is the daily driver. Linux builds and passes the tests but has had
little use; macOS is untested.

### Check

```powershell
duck doctor
```

Each line starts with `ok` or `!!`:

```
ok config C:\Users\you\.duck.lua
ok server 0.1.0 (pid 1234), 3 panes, 3 hosts vivos
ok hooks do Duck em ~/.claude/settings.json
ok WSL: Ubuntu
extensoes: 4 de 16 ligadas (Ctrl+Shift+P > Extensoes liga, configura e pede os tokens)
ok node v22.11.0
ok  github
!!  jira: falta o token jira/work (echo <token> | duck secret set jira/work)
```

Every `!!` line says what to do ("falta o token" = token missing).

### Shell integration in WSL

For command blocks in WSL (exit code, duration, minimap), add the line that
`duck doctor` prints to the distro's `~/.bashrc`. It looks like this:

```bash
. /mnt/c/Users/<you>/AppData/Local/duck/shell/duck.sh
```

pwsh on Windows has it on already.

### Update

```powershell
cd duck
git pull
pwsh -File .\install.ps1 -Extensions
```

The installer restarts the server without closing your shells. Then close and
reopen the Duck window.

---

## 2. First use

The window has three bands:

- **Top:** home, workspace, tabs and the inbox bell (next to minimize, with
  the number of unread notices; it turns red when an agent is waiting for
  you).
- **Footer:** one badge per extension (System, Agents, Azure, Jira…). Click
  opens the panel; drag to reorder.
- **Shortcut bar:** the main shortcuts with their keys; clicking works too.

You don't need to memorize keys. The topic menus gather everything:

| Key | Opens |
|---|---|
| `Ctrl+Shift+P` | Palette: every command, tab, session, history and file |
| `Ctrl+Shift+1` | Orchestration: agents, loop, map, inbox, worktree |
| `Ctrl+Shift+2` | Extensions: extension panels and commands |
| `Ctrl+Shift+3` | Window: tabs, panes and workspaces |
| `Ctrl+Shift+4` | Settings |
| `Ctrl+Shift+/` | Every bound shortcut |
| `Ctrl+,` | Settings (theme, font, opacity…) |

Notices (an agent asking for approval, extensions, loops) show up in the
corner of the screen with buttons, and are kept in the inbox (`Ctrl+Shift+J`).

### Customize the bar and the menus

In `~/.duck.lua` ([duck.example.lua](../duck.example.lua) has everything
commented):

```lua
shortcuts = {
  { label = 'Palette', action = A('palette') },
  { label = 'Orchestration', action = A('menu', 'orquestracao') },
  { label = 'Deploy', action = A('menu', 'deploy') },
},
menus = {
  -- prefixes of the label (or hint) of palette commands; accents and case ignored
  deploy = { title = 'Deploy', match = { 'Azure', 'Git:' } },
},
keys = {
  { key = '5', mods = 'CTRL|SHIFT', action = A('menu', 'deploy') },
},
ui = { footer = true, shortcuts = true },   -- false hides the footer / the shortcut bar
```

Saving the file reloads it right away. The built-in menu names are
`orquestracao`, `extensoes`, `janela`, `config` and `atalhos` (all shortcuts).

---

## 3. Extensions: the basics

### Turn on

Bundled extensions start **off**: an extension that is off costs no memory.
There are two ways to turn one on:

1. **In the window:** `Ctrl+Shift+2` (or `Ctrl+Shift+P` → "Extensões"). Pick
   the extension, turn it on, fill in its settings and add accounts. The token
   is asked right there, masked. This saves to `~/.duck-local.lua`.
2. **In the file:** in `~/.duck.lua`, inside `extensions = { ... }` (see the
   recipes below).

Some extensions (Azure, Jira, GitHub) are configured only in the file. The
Extensions screen turns any of them on and off.

### Tokens

A token **never** goes in the config file. Save it from the Extensions menu
or from the command line:

```powershell
echo <token> | duck secret set jira/work     # save
duck secret has jira/work                    # check (sim = yes, nao = no)
duck secret delete jira/work                 # delete
```

The name is always `<extension>/<account>`, where `<account>` is the account's
`name` in the config. On Windows tokens live in Credential Manager
(`duck:<name>`); on Linux and macOS, in `~/.duck/secrets`.

A whole JSON file (Google service account):

```powershell
Get-Content account.json -Raw | duck secret set googleplay/company
```

### Where things live

| File | Purpose |
|---|---|
| `~/.duck.lua` | Your config (worth versioning) |
| `~/.duck-local.lua` | What the settings and extensions screens save (per machine) |
| `~/.duck/rotinas/*.md` | Routine tasks |
| `~/.duck/queries/<account>.sql` | Saved database queries |
| `~/.duck/snippets.md` | Snippets |
| `~/.duck/notes/` | Notes per project |
| `~/.duck/extensions/<name>` | Your own extensions ([docs/EXTENSIONS.md](EXTENSIONS.md)) |

### Check

`duck doctor` lists enabled extensions that aren't built or are missing a
token. Inside Duck, a configuration error shows at the top of the extension's
panel.

---

## 4. Recipes per extension

Each recipe says what the extension is for, what it needs, the token and the
config. In the examples, replace account names and URLs with yours.

### GitHub

- **For:** notifications, review requests, issues and PRs, repositories,
  across several accounts.
- **Needs:** the [GitHub CLI](https://cli.github.com), logged in. No token of
  its own; it reads the accounts from `gh`.
  ```powershell
  winget install GitHub.cli
  gh auth login            # repeat for each account
  ```
- **Config:**
  ```lua
  github = { enabled = true },                                  -- every gh account
  -- github = { enabled = true, accounts = { 'my-login' }, poll_seconds = 60 },
  ```

### Azure DevOps

- **For:** PRs to review, your PRs, builds and **what is deployed in each
  environment** (releases), across one or more organizations.
- **Token:** a PAT, created at `https://dev.azure.com/<org>/_usersSettings/tokens`
  (or User settings → Personal access tokens) with **Code (Read)**,
  **Build (Read)** and **Release (Read)**:
  ```powershell
  echo <pat> | duck secret set azure/company
  ```
- **Config** (file only):
  ```lua
  azure = { enabled = true, orgs = {
    { name = 'company', url = 'https://dev.azure.com/company', project = 'proj',
      releases = {
        -- Every project at once: each classic release deployed in the last
        -- 30 days becomes a line "develop ✓ #12 · tst ✓ #13 · master ✗ #14".
        { auto = true, branches = { 'develop', 'tst', 'master' } },
        -- A single project: the latest deploy of each environment.
        -- 'label=Environment' renames what is shown; 'Prd|Prod' takes either.
        { name = 'API', definition = 'api-cd', stages = { 'develop=Dev', 'tst=Tst', 'master=Prd|Prod' } },
        -- YAML or CI pipeline: the latest build of each branch.
        { name = 'Portal', pipeline = 'portal-ci', branches = { 'develop', 'tst', 'master' } },
      } },
  } },
  ```
  - **Automatic mode (`auto = true`):** no need to list any project. The
    environment for each branch comes from the branch filter on the
    environment's own trigger (the Dev that deploys `develop`); without a
    filter, from the name (develop → Dev, tst → Tst/QA/Hml, master →
    Prd/Prod). Filters: `path = 'backend'` (release folder), `match =
    'api'` and `exclude = 'legacy'` (patterns on the name), `days = 30`
    (window) and `name = 'Releases'` (section title). You can have more than
    one, each with its own filters and name.
  - Projects with a failed deploy or waiting for approval stay on top. A line
    opens the detail with the last 5 deploys of each environment.
  - `notify = false` on a `releases` item turns off notices for that item only.
  - Older orgs (`https://company.visualstudio.com`) work too.
  - Each release takes `project = '...'` when it lives in another project.
  - `release_notify = false` on the org turns off the "deploy finished" notice.
  - `poll_seconds` (default 120) changes the interval.
- **Finding the names:** palette → "Azure: definicoes de release (para
  configurar)" (release definitions, to configure). It lists the definitions
  and the environments of each, with a snippet ready to copy. A mistyped
  environment shows in the panel together with the names that exist.
- **In the panel:** four tabs on top, with counts: Revisao (review), Meus
  PRs (my PRs), Releases and Builds recentes (recent builds). In automatic mode each line opens the project detail; for
  items configured one by one, each line opens the release in the browser. "Historico"
  (history) shows the last 15 deploys of the environment. The footer badge
  counts failed deploys.

### Jira

- **For:** your tasks (with due dates), issues you watch, notices for
  assignment, comments and status changes. Works with Server/DC and Cloud.
- **Token:**
  - Server/DC: Profile → Personal Access Tokens.
  - Cloud: <https://id.atlassian.com/manage-profile/security/api-tokens>.
  ```powershell
  echo <token> | duck secret set jira/work
  ```
- **Config** (file only):
  ```lua
  jira = { enabled = true, due_days = 3, accounts = {
    { name = 'work', url = 'https://jira.company.com' },                                    -- Server/DC
    { name = 'cloud', url = 'https://company.atlassian.net', type = 'cloud', email = 'me@company.com' },
  } },
  ```

### Grafana (Loki)

- **For:** LogQL queries with the count in a time window, the latest lines,
  and a notice when a new occurrence shows up.
- **Token:** your Grafana `user:password` (Basic auth):
  ```powershell
  echo 'user:password' | duck secret set grafana/company
  ```
- **Loki UID:** Grafana → Connections → Data sources → Loki. The UID is in the
  URL (`/datasources/edit/<uid>`).
- **Config:**
  ```lua
  grafana = { enabled = true, window_hours = 24, poll_seconds = 300,
    accounts = { { name = 'company', url = 'https://grafana.company.com', datasource_uid = '<uid>' } },
    queries = { 'API errors={service_name="api"} |= "ERROR"' } },     -- Name=LogQL
  ```

### Rollbar

- **For:** active errors and warnings, with resolve and mute.
- **Token:** in Project → Settings → Project Access Tokens, a token with
  `read`. Resolving and muting also need `write`.
  ```powershell
  echo <token> | duck secret set rollbar/app
  ```
- **Config:**
  ```lua
  rollbar = { enabled = true, environments = { 'production' },
    accounts = { { name = 'app', account_slug = 'company', project_slug = 'app' } } },   -- slugs only for links
  ```

### Calendar (Google / Outlook)

- **For:** upcoming events, a reminder before each one, and a "Join" button.
- **Token:** the calendar's **secret iCal URL**.
  - Google Calendar: calendar settings → "Secret address in iCal format".
  - Outlook: Settings → Calendar → Shared calendars → Publish a calendar →
    ICS link.
  ```powershell
  echo 'https://calendar.google.com/calendar/ical/.../basic.ics' | duck secret set calendar/work
  ```
- **Config:**
  ```lua
  calendar = { enabled = true, remind_minutes = 5, days = 7, accounts = { { name = 'work' } } },
  ```

### Home Assistant

- **For:** lights, switches, covers, climate and locks, plus sensor alerts.
- **Token:** HA → Profile → Security → "Long-lived access tokens":
  ```powershell
  echo <token> | duck secret set homeassistant/home
  ```
- **Config:**
  ```lua
  homeassistant = { enabled = true,
    accounts = { { name = 'home', url = 'http://homeassistant.local:8123' } },
    favorites = { 'light.living_room', 'switch.coffee' },    -- empty = everything, by type
    alerts = { 'binary_sensor.front_door' } },               -- notifies when it turns on
  ```

### Google Play

- **For:** app tracks and releases, and reviews with replies.
- **Token:** the JSON of a Google Cloud service account with Play Console
  access (Play Console → Users and permissions → invite the service account's
  email).
  ```powershell
  Get-Content service-account.json -Raw | duck secret set googleplay/company
  ```
- **Config:**
  ```lua
  googleplay = { enabled = true, packages = { 'com.company.app' }, accounts = { { name = 'company' } } },
  ```

### Database (Postgres and Redis)

- **For:** running saved queries, with the result as a table. Read-only unless
  `allow_write = true`.
- **Token:** the database password (may be empty):
  ```powershell
  echo <password> | duck secret set database/local
  ```
- **Config:**
  ```lua
  database = { enabled = true, accounts = {
    { name = 'local', kind = 'postgres', host = 'localhost', port = 5432, database = 'app', user = 'postgres' },
    { name = 'cache', kind = 'redis', host = 'localhost' },
  } },
  ```
- **Queries:** in `~/.duck/queries/<account>.sql`, each block starts with
  `-- name: Title`. The file is created with an example the first time. For
  Redis it is one command per line.

### Containers

- **For:** Docker and Podman containers (also inside WSL), with logs, shell,
  start, stop and restart. Notifies when a container dies.
- **Needs:** `docker` and/or `podman` (on Windows or in a WSL distro).
- **Config:**
  ```lua
  containers = { enabled = true, engines = { 'docker', 'Ubuntu' } },   -- distro name = podman inside it
  ```
  The default `auto` starts every WSL distro to look for podman. Name the ones
  you use.

### Claude usage

- **For:** the 5-hour and 7-day limits, today's cost and recent sessions, per
  Claude Code profile (`~/.claude`, `~/.claude-work`…).
- **Needs:** Duck's statusline. `install.ps1 -ClaudeHooks` turns it on where
  there is no statusline. If you already have one, call
  `<duck>\bin\duck-statusline.ps1` from it with the same stdin, or write the
  JSON yourself (the fields are listed in the script's header).
- **Config:** `['claude-usage'] = { enabled = true, warn_percent = 80 }`.

### Git, Ports, Notes, Snippets

These need no token. Just turn them on:

```lua
git = { enabled = true },        -- branch, changes, worktrees and stashes of the active pane (Windows and WSL)
ports = { enabled = true, ignore = { 'svchost' } },   -- listening TCP ports and their process
notes = { enabled = true },      -- notes and tasks per project in ~/.duck/notes
snippets = { enabled = true },   -- snippets in ~/.duck/snippets.md and history of what you copied
```

To save the clipboard history to disk, set `persist = true` on Snippets.
Copies that look like a password or token never enter the history.

---

## 5. Routines

Routines ("Rotinas") send a request to Claude Code at a set time and bring the
answer back as a notice: morning news, a summary of new issues, checking a
deploy. They use your plan's login (no API key) and run in Duck's server, so
they work with the window closed.

### Turn on

1. Have [Claude Code](https://claude.com/claude-code) installed and logged in
   (`claude` in a terminal).
2. Turn the extension on: `Ctrl+Shift+2` → Rotinas, or
   `rotinas = { enabled = true }`.
3. The first time, it creates `~/.duck/rotinas/exemplo.md`, turned off.

### Create a routine

Use the palette → "Rotinas: nova rotina" (new routine; creates `rotina-N.md`
from a commented template), or create a file `~/.duck/rotinas/<id>.md` yourself:

```markdown
---
title: New issues in the project
cron: 0 9 * * 1-5
profile: work
cwd: ~/projects/api
tools: Bash(gh issue list:*), Read
quiet: NOTHING
---
List the issues opened in the last 24 hours in this directory's repository
(gh issue list). For each one, one line with number, title and whether it
looks like a bug. If there are none, answer only NOTHING.
```

Saving schedules it. The Rotinas panel shows the next time, the last run and
the buttons **Rodar agora** (run now), Editar (edit), Pausar (pause) and Ver
(view).

The routine adds its own instructions to the prompt, in Portuguese, including
"answer in Portuguese". Ask for another language in your prompt if you need
one.

### Fields

| Field | Default | What it does |
|---|---|---|
| `title` | file name | Name shown. |
| `cron` | — | Time in 5 fields: `min hour day month weekday`. Accepts `*`, `1-5`, `1,15`, `*/10`. Sunday is 0 or 7. |
| `every` | — | Instead of `cron`: `30m`, `2h`, `1d`. The minimum is `5m`. |
| `profile` | `~/.claude` | Claude profile: `work` = `~/.claude-work`, or a path. |
| `cwd` | `~/.duck/rotinas` | Folder Claude runs in (must exist). |
| `tools` | none | Allowed tools: `WebSearch, WebFetch, Read, Bash(gh issue list:*)`. |
| `mode` | — | `acceptEdits` (may edit files) or `plan`. |
| `model` | — | Model (e.g. `sonnet`). |
| `max_turns` | — | Limit on agent turns. |
| `quiet` | — | If the answer starts with this text (e.g. `NOTHING`), no notice. |
| `notify` | `true` | Shows the notice with the answer. |
| `catchup` | `false` | Runs a missed time (Duck closed, PC asleep). Without it, a run only happens if it is at most 10 minutes late. |
| `enabled` | `true` | `false` turns it off without deleting it. |
| `uso_max` | `85` | Waits if the profile's 5-hour limit is at X% or more. `0` turns the check off. |
| `ceder` | `true` | Waits while one of your agents works in the same repository as `cwd`. |
| `falar` | — | Name of the Echo that reads the answer aloud (via Home Assistant). |
| `celular` | `false` | Push to the phone (via Home Assistant). |
| `falar_noite` | `false` | Also speaks between 22:00 and 7:00. Without it, only the push goes out at night. |
| `falar_max` | `250` | Maximum characters spoken (up to 600). |

For yes/no values: `false`, `nao`, `não`, `no`, `0` and `off` turn a field
off; anything else turns it on.

**Deferral.** With `uso_max` or `ceder`, a routine that finds a high usage
limit, or one of your agents in the same repository, waits, and the panel
shows "adiada: …" (deferred). If it is still deferred when its next time
comes, that run is skipped. `uso_max` depends on Duck's statusline (see
"Claude usage").

**History.** Every run is kept in `~/.duck/rotinas/historico/<id>/`. The
"Continuar no Claude" (continue in Claude) button opens that run's session so
you can carry on the conversation.

### Answer on Alexa or on the phone (optional)

Routines send the answer to a Home Assistant automation through a webhook.

1. **Save the webhook id** in Duck. Make up a long random id; it works like a
   password.
   ```powershell
   echo <webhook-id> | duck secret set rotinas/ha-webhook
   ```
2. **Tell Duck where Home Assistant is**, in the order to try (local network
   first, then a tunnel):
   ```lua
   rotinas = { enabled = true, ha_urls = { 'http://192.168.0.10:8123', 'https://home.example.com' } },
   ```
3. **Create the automation in HA** (Settings → Automations → YAML). Duck sends
   `{ mensagem, canal, echo, titulo }` (message, channel, echo, title); `canal`
   is `echo`, `celular` (phone) or `ambos` (both). An example with
   [Alexa Media Player](https://github.com/alandtse/alexa_media_player) and the
   Home Assistant phone app (adjust the names):
   ```yaml
   alias: Duck - routine answers
   triggers:
     - trigger: webhook
       webhook_id: "<webhook-id>"
       allowed_methods: [POST]
       local_only: false
   actions:
     - if: "{{ trigger.json.canal in ['echo', 'ambos'] }}"
       then:
         - action: notify.alexa_media
           data:
             target: "media_player.{{ trigger.json.echo }}"
             message: "{{ trigger.json.mensagem }}"
             data: { type: announce }
     - if: "{{ trigger.json.canal in ['celular', 'ambos'] }}"
       then:
         - action: notify.mobile_app_my_phone
           data:
             title: "{{ trigger.json.titulo }}"
             message: "{{ trigger.json.mensagem }}"
   ```
4. **In the routine**, use `falar: office` (the Echo's `media_player` name,
   without the prefix) and/or `celular: true`. The panel's **Testar fala/push**
   (test speech/push) button sends a test right away, even at night.

---

## 6. Messages between computers

`duck msg` lets the agent on one computer send a message to the agent on
another. Use it when you work on two machines (or a VM) and keep copying text
from one session to the other. The Ducks talk directly over an encrypted
connection. Outside the local network, an optional relay bridges them.

### Before you start

- Both computers run Duck 0.2.0 or newer. An older Duck still receives
  messages from peers paired before, but without encryption: update and pair
  again.
- One reaches the other: same local network, a VM on the same computer, both
  on [Tailscale](https://tailscale.com), or a relay (below).

### Security

- **Everything is encrypted.** The connection uses the Noise protocol (X25519,
  ChaCha20-Poly1305). Each Duck has a key pair and recognizes the other by its
  public key. No token crosses the network.
- **Confirmation number.** When pairing, both screens show a 9-digit number
  taken from the connection itself. If someone is in the middle, the numbers
  differ: accept only if they match.
- **You approve.** Messages, files and pairing requests wait for you.
  `trust = true` skips the question for messages and files. Use it only
  between your own computers.

### Pair from the panel

1. On both computers: **Ctrl+Shift+P > Computadores** (Computers).
2. If the first line says **desligado** (off), press Enter. Duck listens on all
   networks (port 7878) and restarts the server. Shells keep running.
   - **Rede** (network) picks where to listen: all networks (default), or one
     interface only (home, VM, VPN).
   - If the firewall blocks the port, the panel says so (⚠ Firewall) and Enter
     copies the command that opens it. On Windows: **Firewall do Windows** item.
3. On one of them, either:
   - **Procurar computadores** (find computers): finds Ducks on the local
     network, VMs and Tailscale. Enter on **+ Conectar com ...**.
   - **Conectar por IP**: when the search does not find it.
   - **Gerar convite** (create invite) on one side and **Usar convite** (use
     invite) on the other: pairs without approval. The invite is valid for
     10 minutes, once. Send it by chat or e-mail.
4. Without an invite, both screens show the **confirmation number**. The other
   side gets **Pedido de conexão** (connection request; the taskbar flashes if
   the window is in the background). Same number: **Conectar**. Different, or
   you did not ask for it: **Negar** (deny).
   - The request stays under **Pedidos** (requests) in the panel until answered.
   - If the acceptance cannot travel back (firewall, one-way network), the
     requester polls and completes the pairing by itself. The side that
     accepted gets a notice that it cannot reach the other.
5. Each peer shows under **Conectados** (connected) with its OS, latency, Duck
   version (⚠ when outdated) and whether the connection is encrypted. Enter on
   it:
   - **Agentes de lá** (agents there): sessions open on the other computer;
     Enter on one sends it a message.
   - **Mandar mensagem** (message), **Enviar arquivo** (send a file, up to
     64 MB; it lands in Downloads/Duck after it is accepted).
   - **Diagnosticar** (diagnose): tests each address, encryption, version and
     whether the other side reaches this one back, and says what to do.
   - **Endereços** (addresses): one computer can have several (home, VPN,
     relay). Duck tries them in order and moves the one that worked to the
     front. New addresses are added when the other side talks to this one or
     shows up in a search.
   - **Parear de novo** (pair again), **Remover** (remove).
6. **Conversas** (conversations) lists the exchanges between agents and how
   many round trips each had. Enter closes one: new messages in it are refused.

From the terminal:

```sh
duck peer find                     # local network, VMs and Tailscale
duck peer connect <ip>             # asks to connect, shows the number, waits
duck peer invite                   # creates an invite (10 min, once)
duck peer join '<invite>'          # uses the other side's invite
duck peer list                     # peers: online, version, encrypted or not
duck peer doctor [name]            # diagnosis (no name: this computer only)
duck peer sessions <name>          # agents open there
duck peer addr <name> add|remove|list [address]
duck peer convs                    # conversations; duck peer close <id> closes one
duck peer remove <name>
duck send-file <name> <file>
```

### Outside the local network

- **Tailscale (simplest).** Install it on both. The search finds Ducks on the
  tailnet by itself, and the Tailscale address joins the peer's list.
- **Relay.** On a computer both can reach (a VPS, for example):

  ```sh
  duck relay --listen 0.0.0.0:7879 --secret <password>
  ```

  On each Duck: **Computadores > Relay > Usar um relay** (`host:7879`) and the
  password. Then pair with an invite: it carries the relay route. The relay
  only splices connections. Encryption is end to end, so it can neither read
  nor change anything.

### By hand

In `~/.duck-local.lua` (the panel writes the same file):

```lua
config.peer = { listen = '0.0.0.0:7878', name = 'laptop', trust = false, max_hops = 10,
                relay = 'my-vps.com:7879', relay_secret = '...' }
config.peers = { fedora = '192.168.0.7:7878,100.64.0.2:7878,relay:my-vps.com:7879' }
```

Each peer's key lives in the system vault, not in the config. After changing
`listen` or `relay`: **Ctrl+Shift+P > Reiniciar server** (restart server).

### Use

From an agent (or from you, in any pane):

```sh
duck msg fedora "Install package X on Fedora and tell me the version"
cat config.toml | duck msg fedora          # text from stdin
duck msg fedora --to MyShopping "..."      # to a specific session there
```

On the other computer:

1. "Mensagem de outro computador" (message from another computer) shows up.
   **Entregar ao agente** (deliver) or **Negar** (deny). It also goes to the inbox.
2. It goes to the conversation's agent. For a first message, it goes to the
   agent of the pane open in the window. A pane without an agent does not
   receive: the sender gets an error. With `--to <name>`, it goes to the agent
   whose tab name, title, folder or pane number matches (`duck peer sessions
   <name>` lists them).
3. If the agent is working, the message waits until it stops.
4. The delivered text tells the agent how to reply
   (`duck msg <name> --conv <id> "..."`). The reply goes back to the pane that
   sent the first message.

### Limits

- **No endless conversations.** After `max_hops` messages (10) in one
  conversation, Duck refuses and asks you. Send without `--conv` to start a new
  one, or close the conversation in the panel.
- **Secrets masked.** Known tokens are masked.
- **Up to 16 thousand characters per message** and 64 MB per file.

---

## 7. Troubleshooting

| Symptom | What to do |
|---|---|
| `install.ps1` complains about MSVC or `link.exe` | Run the `winget install Microsoft.VisualStudio.2022.BuildTools ...` it prints and open a new terminal. |
| `duck` is not recognized | Open a new terminal (only new ones see the PATH change). |
| An enabled extension shows no data | `duck doctor`. Missing token: `echo <token> \| duck secret set <ext>/<account>`. Not built: `install.ps1 -SkipBuild -Extensions`. |
| Azure: "PAT vencido ou sem permissao" (PAT expired or lacks permission) | Create another PAT with Code, Build and Release (Read) and save it over the old one with `duck secret set`. |
| Azure: environment "nao existe" (doesn't exist) | The panel lists the environments that exist. Copy the exact name, or use "Azure: definicoes de release". |
| GitHub: "Nenhuma conta" (no account) | `gh auth login`. |
| Claude usage: "Pasta de uso não encontrada" (usage folder not found) | Duck's statusline is missing: `install.ps1 -SkipBuild -ClaudeHooks`. Then open a Claude session. |
| Routine shows "erro no arquivo" (error in file) | The `---` header is missing, `cron:`/`every:` is missing, the body is empty, or `cwd` doesn't exist. |
| A routine doesn't run | Check whether it is paused, `enabled: false`, or "adiada" (deferred) in the panel. "Rodar agora" (run now) tests it immediately. |
| Agent notices don't show | Check `notify = true` in `~/.duck.lua` (the default). A notice only goes out when you aren't looking at that pane. In full screen it goes to the Windows notification center. |
| Agents show no state in tabs | `install.ps1 -SkipBuild -ClaudeHooks`, then reopen Claude. |
| I updated and nothing changed | Close and reopen the Duck window (the server was already restarted). |
| `duck msg`: "nao respondeu" (did not answer) | `duck peer doctor <name>` (or Computadores > name > Diagnosticar): tells whether the other side is not listening, the firewall blocks it, the version is old or only one direction works, and what to do. |
| `duck msg`: "nao esta pareado la" (not paired there) or "chave ... nao e a esperada" (unexpected key) | The other side forgot this computer or was reinstalled. Pair again (an invite is quickest). |
| Peer marked "⚠ sem criptografia" (no encryption) | Paired with version 0.1.8 or older. Update both and use **Parear de novo**. |
| `duck msg`: "nenhum agente para receber" (no agent to receive) | On the other computer, open the tab of an agent (Claude running) and send again. |

Still stuck? The server log is at `%LOCALAPPDATA%\duck\server.log` and the
extensions log at `%LOCALAPPDATA%\duck\ext-host.log`.
