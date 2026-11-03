// Extensao "Snippets": trechos guardados em ~/.duck/snippets.md e historico do
// que foi copiado dentro do Duck.
//
// Formato do arquivo: cada snippet e uma secao "## Nome" seguida de um bloco
// de codigo cercado (```). O historico do clipboard fica so em memoria, a nao
// ser com persist = true; entradas que parecem segredo sao descartadas.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { duck, run, Item } from "./duck";

const FILE = path.join(os.homedir(), ".duck", "snippets.md");
const CLIP_FILE = path.join(os.homedir(), ".duck", "clipboard.json");
const EXAMPLE = `# Snippets do Duck

<!--
Cada snippet e uma secao "## Nome" seguida de um bloco de codigo cercado por
tres crases. O texto do bloco e o que vai ser colado no terminal. Texto fora
do bloco (como este comentario) e ignorado. Edite a vontade: a lista recarrega
sozinha quando o arquivo muda.
-->

## Status do git

\`\`\`
git status -sb
\`\`\`

## Últimos commits

\`\`\`
git log --oneline -n 15
\`\`\`
`;

type Snippet = { name: string; text: string };
type Clip = { id: number; text: string; t: number };

const SECRET: RegExp[] = [
  /\bgh[pousr]_[A-Za-z0-9]{20,}/, /\bgithub_pat_\w{20,}/,
  /\bsk-[A-Za-z0-9_-]{16,}/, /\bAKIA[0-9A-Z]{16}\b/, /\bxox[abprs]-[\w-]{10,}/,
  /-----BEGIN [A-Z ]*PRIVATE KEY-----/,
  /\beyJ[\w-]{8,}\.[\w-]{8,}\.[\w-]{8,}/, // JWT
  /\b(password|passwd|pwd|senha|secret|token|api[_-]?key|client[_-]?secret)\s*[=:]\s*\S/i,
];
const looksSecret = (s: string) => SECRET.some((r) => r.test(s));

let snippets: Snippet[] = [];
let clips: Clip[] = [];
let clipSeq = 0;
let persist = false;
let maxItems = 30;
let pendingName = "";
let pendingText: string | null = null; // conteudo vindo do clipboard ao salvar como snippet
let timer: NodeJS.Timeout | undefined;

// ---------------------------------------------------------------- snippets

function parse(md: string): Snippet[] {
  const out: Snippet[] = [];
  const lines = md.replace(/\r/g, "").split("\n");
  let name: string | null = null;
  for (let i = 0; i < lines.length; i++) {
    const h = /^##\s+(.*)$/.exec(lines[i]);
    if (h) { name = h[1].trim(); continue; }
    const f = /^(`{3,}|~{3,})/.exec(lines[i]);
    if (!f || name === null) continue;
    const body: string[] = [];
    for (i++; i < lines.length && !lines[i].startsWith(f[1]); i++) body.push(lines[i]);
    out.push({ name, text: body.join("\n") });
    name = null; // so o 1o bloco de cada secao
  }
  return out;
}

function loadSnippets(): void {
  try { snippets = parse(fs.readFileSync(FILE, "utf8")); } catch { snippets = []; }
}

function addSnippet(name: string, text: string): void {
  let fence = "```";
  while (text.includes(fence)) fence += "`";
  const cur = fs.existsSync(FILE) ? fs.readFileSync(FILE, "utf8") : "";
  const sep = !cur || cur.endsWith("\n\n") ? "" : cur.endsWith("\n") ? "\n" : "\n\n";
  fs.appendFileSync(FILE, `${sep}## ${name.replace(/\s+/g, " ")}\n\n${fence}\n${text.replace(/\r/g, "")}\n${fence}\n`);
  loadSnippets();
  render();
  duck.flash(`Snippet "${name}" salvo`);
}

// ---------------------------------------------------------------- clipboard

function saveClips(): void {
  if (persist) { try { fs.writeFileSync(CLIP_FILE, JSON.stringify(clips)); } catch (e) { duck.log("clipboard.json", e); } }
}

function addClip(text: string): void {
  if (!text.trim() || looksSecret(text)) return;
  clips = [{ id: ++clipSeq, text, t: Date.now() }, ...clips.filter((c) => c.text !== text)].slice(0, maxItems);
  saveClips();
  render();
}

// ---------------------------------------------------------------- visoes

const firstLine = (s: string) => {
  const lines = s.split("\n").filter((l) => l.trim());
  return (lines[0] ?? "").trim().slice(0, 80) + (lines.length > 1 ? `  (+${lines.length - 1} linha${lines.length > 2 ? "s" : ""})` : "");
};

function ago(t: number): string {
  const m = (Date.now() - t) / 60000;
  return m < 1 ? "agora" : m < 60 ? `há ${Math.floor(m)}min` : `há ${Math.floor(m / 60)}h`;
}

function render(): void {
  const items: Item[] = snippets.map((s, i) => ({
    id: `s:${i}`, title: s.name, subtitle: firstLine(s.text), icon: "",
    actions: [
      { id: "paste", title: "Colar", primary: true },
      { id: "run", title: "Colar e executar" },
      { id: "edit", title: "Editar arquivo" },
    ],
  }));
  items.push({
    id: "new", title: "Novo snippet", subtitle: FILE, icon: "", color: "accent",
    actions: [{ id: "new", title: "Novo snippet", primary: true }, { id: "edit", title: "Editar arquivo" }],
  });
  duck.view("snippets", [{ title: `Snippets (${snippets.length})`, items }]);

  const citems: Item[] = clips.map((c) => ({
    id: `c:${c.id}`, title: firstLine(c.text), subtitle: ago(c.t), icon: "",
    actions: [
      { id: "paste", title: "Colar", primary: true },
      { id: "save", title: "Salvar como snippet" },
      { id: "clear", title: "Limpar histórico" },
    ],
  }));
  duck.view("clipboard", [{
    title: persist ? "Copiados (guardado em disco)" : "Copiados (só nesta sessão)",
    items: citems.length ? citems : [{ id: "none", title: "Nada copiado ainda", subtitle: "Prováveis senhas e tokens não entram", color: "muted" }],
  }]);
}

// ---------------------------------------------------------------- protocolo

function textOf(item: string): string | null {
  const [k, n] = item.split(":");
  if (k === "s") return snippets[Number(n)]?.text ?? null;
  if (k === "c") return clips.find((c) => c.id === Number(n))?.text ?? null;
  return null;
}

run({
  init(m) {
    const s = (m.settings ?? {}) as { persist?: boolean; max_items?: number };
    persist = s.persist === true;
    maxItems = Math.max(1, Number(s.max_items) || 30);
    fs.mkdirSync(path.dirname(FILE), { recursive: true });
    if (!fs.existsSync(FILE)) fs.writeFileSync(FILE, EXAMPLE);
    if (persist) {
      try { clips = (JSON.parse(fs.readFileSync(CLIP_FILE, "utf8")) as Clip[]).filter((c) => !looksSecret(c.text)).slice(0, maxItems); } catch { clips = []; }
      clipSeq = Math.max(0, ...clips.map((c) => c.id));
    }
    duck.register([
      { id: "snippets", title: "Snippets", icon: "" },
      { id: "clipboard", title: "Clipboard", icon: "" },
    ]);
    loadSnippets();
    render();
    fs.watch(path.dirname(FILE), (_e, name) => {
      if (String(name ?? "") !== path.basename(FILE)) return;
      clearTimeout(timer);
      timer = setTimeout(() => { loadSnippets(); render(); }, 150);
    });
  },
  viewOpen() { loadSnippets(); render(); },
  clipboard(m) { addClip(m.text ?? ""); },
  action(m) {
    if (m.action === "refresh") { loadSnippets(); return render(); }
    if (m.action === "edit") return duck.openUrl(FILE);
    if (m.action === "new") { pendingText = null; return duck.prompt("name", "Nome do snippet"); }
    if (m.action === "clear") return duck.choose("clear", "Limpar o histórico do clipboard?", [{ id: "yes", title: "Limpar", hint: `${clips.length} entradas` }]);
    const text = textOf(m.item);
    if (text === null) return duck.flash("Item não encontrado");
    if (m.action === "paste") return duck.paste(text);
    if (m.action === "run") return duck.paste(text, true);
    if (m.action === "save") { pendingText = text; return duck.prompt("name", "Nome do snippet"); }
  },
  input(m) {
    const value = m.value ?? "";
    if (m.id === "clear") {
      if (value !== "yes") return;
      clips = [];
      if (persist) { try { fs.rmSync(CLIP_FILE, { force: true }); } catch { /* ok */ } }
      return render();
    }
    if (m.id === "name") {
      pendingName = value.trim();
      if (!pendingName) return;
      if (pendingText !== null) { addSnippet(pendingName, pendingText); pendingText = null; return; }
      return duck.prompt("text", `Conteúdo de "${pendingName}"`);
    }
    if (m.id === "text" && value.trim()) addSnippet(pendingName, value);
  },
});
