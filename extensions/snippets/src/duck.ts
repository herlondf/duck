// SDK do Duck para extensoes em TypeScript (Node 18+), sem dependencias.
//
// A extensao e um processo que o Duck abre; as mensagens sao JSON, uma por
// linha: o Duck escreve no stdin e le o stdout. Use stderr (console.error)
// para log: ele vai para %LOCALAPPDATA%/duck/ext-<nome>.log.
// Protocolo completo: docs/EXTENSIONS.md.

import * as readline from "node:readline";

export type Action = { id: string; title: string; primary?: boolean };
export type Item = {
  id: string;
  title: string;
  subtitle?: string;
  /** Nome de icone (github, bell, issue, issue-closed, pr, pr-merged, pr-draft, repo, comment, tag, check...) ou um glifo. */
  icon?: string;
  badge?: string;
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

let seq = 0;
const waiting = new Map<number, (v: string | null) => void>();

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
  status(id: string, text: string, opts: { color?: string; view?: string } = {}) { send({ type: "status", id, text, ...opts }); },
  /** Aviso: cartao no Duck (janela em foco) ou toast do Windows com botoes. */
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
};

/** Liga os handlers e comeca a ler o stdin. O processo sai quando o Duck fecha. */
export function run(h: Handlers): void {
  const dispatch = async (m: any) => {
    if (m.type === "secret") { waiting.get(m.id)?.(m.value ?? null); waiting.delete(m.id); return; }
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
