// Extensao "Notas": notas e tarefas por projeto e uma nota geral, em markdown.
//
// Projeto = raiz git do cwd do pane ativo (ou o proprio cwd). Arquivos em
// ~/.duck/notes/<slug>.md e ~/.duck/notes/geral.md (nunca dentro do repositorio).
// "- [ ]" / "- [x]" viram tarefas, "## Titulo" vira secao, o resto vira nota.

import * as fs from "node:fs";
import * as os from "node:os";
import * as path from "node:path";
import { execFile } from "node:child_process";
import { duck, run, Item, Section } from "./duck";

const DIR = path.join(os.homedir(), ".duck", "notes");
const GERAL = path.join(DIR, "geral.md");
const TASK = /^(\s*[-*]\s+\[)([ xX])(\]\s+)(.*)$/;

let project: { root: string; file: string } | null = null;
/** Linhas mostradas, por id de item ("p:12" / "g:3"), para conferir antes de reescrever. */
let shown = new Map<string, string>();
let watcher: fs.FSWatcher | null = null;
let timer: NodeJS.Timeout | undefined;
let ctxSeq = 0;

// ---------------------------------------------------------------- arquivos

function slug(p: string): string {
  let s = path.resolve(p);
  if (process.platform === "win32") s = s.toLowerCase();
  return s.replace(/[^a-zA-Z0-9]+/g, "-").replace(/^-+|-+$/g, "") || "raiz";
}

function gitRoot(cwd: string): Promise<string> {
  return new Promise((res) => {
    execFile("git", ["-C", cwd, "rev-parse", "--show-toplevel"], { timeout: 5000, windowsHide: true },
      (err, out) => res(err || !out.trim() ? cwd : path.normalize(out.trim())));
  });
}

const fileOf = (k: string) => (k === "p" ? project?.file : GERAL);
const read = (f: string) => { try { return fs.readFileSync(f, "utf8"); } catch { return ""; } };

function write(f: string, text: string): void {
  fs.mkdirSync(DIR, { recursive: true });
  fs.writeFileSync(f, text);
}

/** Tarefa entra colada na anterior; nota vira paragrafo (linha em branco antes). */
function append(f: string, block: string, paragraph: boolean): void {
  const cur = read(f);
  const nl = paragraph ? "\n\n" : "\n";
  const sep = !cur || cur.endsWith(nl) ? "" : cur.endsWith("\n") ? "\n" : nl;
  write(f, cur + sep + block + "\n");
}

/** Troca so a linha n, se ela ainda for a que estava na tela. */
function editLine(id: string, fn: (line: string) => string | null): boolean {
  const [k, n] = id.split(":");
  const f = fileOf(k);
  if (!f) return false;
  const lines = read(f).split("\n");
  const i = Number(n);
  const line = lines[i]?.replace(/\r$/, "");
  if (line === undefined || line !== shown.get(id)) { duck.flash("O arquivo mudou; recarreguei a lista."); render(); return false; }
  const cr = lines[i].endsWith("\r") ? "\r" : "";
  const out = fn(line);
  if (out === null) lines.splice(i, 1); else lines[i] = out + cr;
  write(f, lines.join("\n"));
  return true;
}

// ---------------------------------------------------------------- visao

function parse(k: string, label: string, text: string, sections: Section[]): number {
  let open = 0;
  let cur: Section = { title: label, items: [] };
  const all = [cur];
  text.split("\n").forEach((raw, i) => {
    const line = raw.replace(/\r$/, "");
    const id = `${k}:${i}`;
    const h = /^##\s+(.*)$/.exec(line);
    if (h) { cur = { title: `${label} › ${h[1].trim()}`, items: [] }; all.push(cur); return; }
    if (!line.trim() || /^#\s/.test(line)) return;
    shown.set(id, line);
    const t = TASK.exec(line);
    if (t) {
      const done = t[2] !== " ";
      if (!done) open++;
      cur.items.push({
        id, title: t[4], icon: done ? "check" : "", color: done ? "done" : undefined, badge: done ? "feito" : undefined,
        actions: [{ id: "toggle", title: done ? "Desmarcar" : "Marcar feito", primary: true }, { id: "delete", title: "Apagar" }],
      });
    } else {
      cur.items.push({ id, title: line.replace(/^\s*[-*]\s+/, ""), icon: "comment", color: "muted", actions: [{ id: "delete", title: "Apagar" }] });
    }
  });
  all[0].items.unshift({
    id: `${k}:add`, title: "Nova tarefa", subtitle: fileOf(k), icon: "", color: "accent",
    actions: [
      { id: "new-task", title: "Nova tarefa", primary: true },
      { id: "new-note", title: "Nova nota" },
      { id: "open-file", title: "Abrir no editor" },
    ],
  });
  sections.push(...all.filter((s, j) => j === 0 || s.items.length));
  return open;
}

function render(): void {
  shown = new Map();
  const sections: Section[] = [];
  let open = 0;
  if (project) open = parse("p", `Deste projeto (${path.basename(project.root) || project.root})`, read(project.file), sections);
  else sections.push({ title: "Deste projeto", items: [{ id: "none", title: "Nenhum pane ativo ainda", color: "muted" }] });
  parse("g", "Geral", read(GERAL), sections);
  duck.view("notes", sections);
  duck.status("notes", open ? ` ${open}` : "", { view: "notes", color: "accent" });
}

function scheduleRender(): void {
  clearTimeout(timer);
  timer = setTimeout(render, 150);
}

function watch(): void {
  if (watcher) return;
  fs.mkdirSync(DIR, { recursive: true });
  watcher = fs.watch(DIR, (_ev, name) => {
    const n = String(name ?? "");
    if (!n || n === "geral.md" || (project && n === path.basename(project.file))) scheduleRender();
  });
}

// ---------------------------------------------------------------- protocolo

run({
  init() {
    duck.register([{ id: "notes", title: "Notas", icon: "" }]);
    watch();
    render();
  },
  async context(m) {
    if (!m.cwd) return;
    const mine = ++ctxSeq;
    const root = await gitRoot(m.cwd);
    if (mine !== ctxSeq) return; // trocou de pane de novo enquanto o git rodava
    project = { root, file: path.join(DIR, slug(root) + ".md") };
    render();
  },
  viewOpen() { render(); },
  action(m) {
    if (m.action === "refresh") return render();
    const k = m.item.split(":")[0];
    if (m.action === "toggle") {
      if (editLine(m.item, (l) => l.replace(TASK, (_a, pre, x, post, rest) => `${pre}${x === " " ? "x" : " "}${post}${rest}`))) render();
    } else if (m.action === "delete") {
      duck.choose(`del|${m.item}`, `Apagar "${(shown.get(m.item) ?? "").slice(0, 60)}"?`, [{ id: "yes", title: "Apagar", hint: "não tem desfazer" }]);
    } else if (m.action === "new-task") {
      duck.prompt(`task|${k}`, "Nova tarefa");
    } else if (m.action === "new-note") {
      duck.prompt(`note|${k}`, "Nova nota");
    } else if (m.action === "open-file") {
      const f = fileOf(k);
      if (!f) return;
      if (!fs.existsSync(f)) write(f, k === "p" ? `# ${project?.root}\n\n` : "# Geral\n\n");
      duck.openUrl(f);
    }
  },
  input(m) {
    const [what, arg] = m.id.split("|");
    const value = (m.value ?? "").trim();
    if (!value) return;
    if (what === "del") { if (value === "yes" && editLine(arg, () => null)) render(); return; }
    const f = fileOf(arg);
    if (!f) return duck.flash("Nenhum projeto ativo");
    const one = value.replace(/\s*\n\s*/g, " ");
    append(f, what === "task" ? `- [ ] ${one}` : one, what !== "task");
    render();
  },
});
