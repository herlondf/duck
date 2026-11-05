// SDK do Duck para extensoes em TypeScript (Node 18+), sem dependencias.
//
// A extensao e um processo que o Duck abre; as mensagens sao JSON, uma por
// linha: o Duck escreve no stdin e le o stdout. Use stderr (console.error)
// para log: ele vai para %LOCALAPPDATA%/duck/ext-<nome>.log.
// Protocolo completo: docs/EXTENSIONS.md.

import * as readline from "node:readline";
import { spawn } from "node:child_process";
import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";

export type Action = { id: string; title: string; primary?: boolean };
export type Item = {
  id: string;
  title: string;
  subtitle?: string;
  /** Nome de icone (github, bell, issue, issue-closed, pr, pr-merged, pr-draft, repo, comment, tag, check...) ou um glifo. */
  icon?: string;
  badge?: string;
  /** Desde quando (epoch ms): ordena listas que juntam extensoes. */
  ts?: number;
  /** Cor do tema: accent, done, working, blocked, muted, fg, ou #rrggbb. */
  color?: string;
  unread?: boolean;
  actions?: Action[];
};
export type Section = { title?: string; items: Item[] };
export type ViewDef = { id: string; title: string; icon?: string };
export type Command = { id: string; title: string };

export type Init = { type: "init"; settings: Record<string, unknown> | null; duck: { version: string; os: string } };
export type ActionMsg = { type: "action"; action: string; item: string; view: string };
export type CommandMsg = { type: "command"; command: string };
export type InputMsg = { type: "input"; id: string; value: string };
export type ViewMsg = { type: "view-open" | "view-close"; view: string };
/** Pane ativo do Duck (manifesto precisa de "permissions": ["context"]). Chega no init e a cada troca de pane. */
export type ContextMsg = { type: "context"; cwd: string; pane: number; domain: string; title: string };
/** Texto copiado dentro do Duck (manifesto precisa de "permissions": ["clipboard"]). */
export type ClipboardMsg = { type: "clipboard"; text: string };

export type AgentOptions = {
  /** O pedido, como voce digitaria para o Claude Code. */
  prompt: string;
  /** Pasta onde o agente trabalha (repositorio). Padrao: a pasta da extensao. */
  cwd?: string;
  /** Perfil do Claude Code: "" = ~/.claude; "trabalho" = ~/.claude-trabalho; ou um caminho. */
  profile?: string;
  /** Ferramentas liberadas sem pedir, ex.: ["WebSearch", "Read", "Bash(gh issue list:*)"].
   *  O resto e negado (nao ha ninguem para aprovar). */
  tools?: string[];
  /** acceptEdits deixa editar arquivos do cwd sem pedir. */
  permissionMode?: "default" | "acceptEdits" | "plan";
  maxTurns?: number;
  model?: string;
  /** Texto somado ao prompt de sistema. */
  system?: string;
  /** Continua uma conversa anterior (sessionId de um resultado). */
  resume?: string;
  /** Mata o agente depois disso. Padrao: 30 min. */
  timeoutSec?: number;
};
export type AgentResult = {
  ok: boolean;
  /** Resposta final do agente. */
  text: string;
  error?: string;
  sessionId?: string;
  turns?: number;
  durationMs?: number;
  /** Custo estimado pelo Claude Code; no plano (Pro/Max) nao e cobrado a parte. */
  costUsd?: number;
};

/** Pasta de config do perfil do Claude Code ("" = a padrao, ~/.claude). */
function claudeDir(profile: string): string | null {
  if (!profile || profile === "default") return null;
  if (/[\\/]/.test(profile)) return profile;
  return path.join(os.homedir(), `.claude-${profile}`);
}

function claudeExe(): string {
  const local = path.join(os.homedir(), ".local", "bin", process.platform === "win32" ? "claude.exe" : "claude");
  return fs.existsSync(local) ? local : "claude";
}

/** Pane com agente aberto no Duck (manifesto precisa de "permissions": ["agents"]). */
export type AgentPane = {
  pane: number; workspace: string; tab: string; title: string; cwd: string; parent?: number | null; idle_secs: number;
  agent: { name: string; state: string; since: number; source: string; session?: string | null; config_dir?: string | null; [k: string]: unknown };
};

let seq = 0;
const waiting = new Map<number, (v: any) => void>();

type Handlers = {
  init?: (m: Init) => unknown;
  action?: (m: ActionMsg) => unknown;
  command?: (m: CommandMsg) => unknown;
  input?: (m: InputMsg) => unknown;
  viewOpen?: (m: ViewMsg) => unknown;
  viewClose?: (m: ViewMsg) => unknown;
  context?: (m: ContextMsg) => unknown;
  clipboard?: (m: ClipboardMsg) => unknown;
};

// Rodando dentro do hospedeiro compartilhado (sdk/host.js): um Node para todas
// as extensoes. O nome da extensao e capturado na carga do modulo.
const host: any = (globalThis as any).__duckHost;
const NAME: string | null = host?.current ?? null;

function send(m: Record<string, unknown>): void {
  if (host && NAME) host.send(NAME, m);
  else process.stdout.write(JSON.stringify(m) + "\n");
}

export const duck = {
  /** Visoes (aparecem como icone na barra e na paleta) e comandos da paleta. */
  register(views: ViewDef[], commands: Command[] = []) { send({ type: "register", views, commands }); },
  /** Conteudo de uma visao. loading=true mostra "carregando"; error aparece em vermelho no topo. */
  view(id: string, sections: Section[], opts: { title?: string; loading?: boolean; error?: string } = {}) {
    send({ type: "view", id, sections, ...opts });
  },
  /** Item na barra de titulo; clicar abre a visao `view`. text vazio remove. */
  status(id: string, text: string, opts: { color?: string; view?: string; command?: string } = {}) { send({ type: "status", id, text, ...opts }); },
  /** Aviso: cartao no Duck (janela em foco) ou no canto da tela, com botoes. */
  notify(n: { title: string; body?: string; item?: string; actions?: Action[]; color?: string; seconds?: number }) { send({ type: "notify", ...n }); },
  /** Painel flutuante com texto rolavel e botoes. */
  detail(d: { title: string; subtitle?: string; body: string; item?: string; actions?: (Action & { item?: string })[] }) { send({ type: "detail", ...d }); },
  /** Pergunta um texto; a resposta chega em `input` com o mesmo id. */
  prompt(id: string, title: string, value = "") { send({ type: "prompt", id, title, value }); },
  openUrl(url: string) { send({ type: "open-url", url }); },
  /** Abre um comando num pane: aba nova (padrao) ou flutuante. */
  openPane(cmd: string[], opts: { where?: "tab" | "float"; cwd?: string; title?: string } = {}) { send({ type: "open-pane", cmd, ...opts }); },
  /** Le um segredo do Duck (Gerenciador de Credenciais). Sem permissao ou inexistente: null.
   *  Grave com: echo <token> | duck secret set <extensao>/<conta> */
  secret(name: string): Promise<string | null> {
    const id = ++seq;
    return new Promise((res) => { waiting.set(id, res); send({ type: "secret", id, name }); });
  },
  /** Agentes abertos nos panes do Duck: pasta, estado (working, blocked, done, idle)...
   *  Sem a permissao "agents" no manifesto, a resposta nao vem (null em 3 s). */
  agents(): Promise<AgentPane[] | null> {
    const id = ++seq;
    return new Promise((res) => {
      const t = setTimeout(() => { waiting.delete(id); res(null); }, 3000);
      waiting.set(id, (v) => { clearTimeout(t); res(v); });
      send({ type: "agents", id });
    });
  },
  /** Lista para escolher; a escolha chega em `input` com value = id da opcao. */
  choose(id: string, title: string, options: { id: string; title: string; hint?: string }[]) { send({ type: "choose", id, title, options }); },
  /** Cola no pane ativo (manifesto precisa de "permissions": ["paste"]). enter=true executa.
   *  Texto com varias linhas ou perigoso passa pela confirmacao do Duck. */
  paste(text: string, enter = false) { send({ type: "paste", text, enter }); },
  /** Abre uma visao registrada na lateral. */
  showView(id: string) { send({ type: "show-view", id }); },
  /** Mensagem curta no canto da janela. */
  flash(text: string) { send({ type: "flash", text }); },
  log(...a: unknown[]) { if (host && NAME) host.log(NAME, a); else console.error(new Date().toISOString(), ...a); },
  /** Roda o Claude Code sem tela (`claude -p`) e devolve a resposta final.
   *  Usa o login do perfil (assinatura Pro/Max), nunca uma chave de API:
   *  ANTHROPIC_API_KEY e removida do ambiente do agente. */
  agent(o: AgentOptions): Promise<AgentResult> {
    const args = ["-p", "--output-format", "json"];
    if (o.tools?.length) args.push("--allowedTools", o.tools.join(","));
    if (o.permissionMode) args.push("--permission-mode", o.permissionMode);
    if (o.maxTurns) args.push("--max-turns", String(o.maxTurns));
    if (o.model) args.push("--model", o.model);
    if (o.system) args.push("--append-system-prompt", o.system);
    if (o.resume) args.push("--resume", o.resume);
    const env: NodeJS.ProcessEnv = { ...process.env };
    for (const k of Object.keys(env)) {
      if (k === "CLAUDECODE" || k.startsWith("CLAUDE_CODE_") || k === "ANTHROPIC_API_KEY" || k === "ANTHROPIC_AUTH_TOKEN" || k === "CLAUDE_CONFIG_DIR") delete env[k];
    }
    const dir = claudeDir(o.profile ?? "");
    if (dir) env.CLAUDE_CONFIG_DIR = dir;
    const cwd = o.cwd ?? process.cwd();
    const started = Date.now();
    duck.log(`agente: inicio (perfil ${o.profile || "padrao"}, ${cwd})`);
    return new Promise((resolve) => {
      let out = "", err = "", done = false;
      const finish = (r: AgentResult) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        duck.log(`agente: fim em ${Math.round((Date.now() - started) / 1000)}s, ok=${r.ok}${r.error ? `, ${r.error}` : ""}`);
        resolve(r);
      };
      // O prompt vai pelo stdin: na linha de comando do Windows aspas e quebras se perdem.
      const child = spawn(claudeExe(), args, { cwd, env, windowsHide: true, stdio: ["pipe", "pipe", "pipe"] });
      const timer = setTimeout(() => { child.kill(); finish({ ok: false, text: "", error: "tempo esgotado" }); }, (o.timeoutSec ?? 1800) * 1000);
      child.stdout.on("data", (d) => { out += d; });
      child.stderr.on("data", (d) => { err += d; });
      child.on("error", (e) => finish({ ok: false, text: "", error: `claude nao abriu: ${e.message}` }));
      child.on("close", (code) => {
        try {
          const j = JSON.parse(out.trim().split("\n").pop() || "{}");
          const ok = j.type === "result" && !j.is_error && j.subtype === "success";
          finish({
            ok, text: j.result ?? "", sessionId: j.session_id, turns: j.num_turns, durationMs: j.duration_ms, costUsd: j.total_cost_usd,
            error: ok ? undefined : (j.subtype && j.subtype !== "success" ? j.subtype : (j.result || err.trim() || `saiu com ${code}`)),
          });
        } catch {
          finish({ ok: false, text: "", error: (err.trim() || out.trim() || `saiu com ${code}`).slice(0, 500) });
        }
      });
      child.stdin.end(o.prompt);
    });
  },
};

/** Liga os handlers e comeca a ler o stdin. O processo sai quando o Duck fecha. */
export function run(h: Handlers): void {
  const dispatch = async (m: any) => {
    if (m.type === "secret") { waiting.get(m.id)?.(m.value ?? null); waiting.delete(m.id); return; }
    if (m.type === "agents") { waiting.get(m.id)?.(m.agents ?? []); waiting.delete(m.id); return; }
    const fn = ({
      init: h.init, action: h.action, command: h.command, input: h.input,
      "view-open": h.viewOpen, "view-close": h.viewClose, context: h.context, clipboard: h.clipboard,
    } as Record<string, ((m: any) => unknown) | undefined>)[m.type];
    if (!fn) return;
    try { await fn(m); } catch (e) {
      duck.log("erro em", m.type, e);
      duck.flash(`Erro na extensao: ${(e as Error).message ?? e}`);
    }
  };
  if (host && NAME) { host.register(NAME, dispatch); return; }
  const rl = readline.createInterface({ input: process.stdin });
  rl.on("line", (line) => {
    let m: any;
    try { m = JSON.parse(line); } catch { return; }
    void dispatch(m);
  });
  rl.on("close", () => process.exit(0));
}
