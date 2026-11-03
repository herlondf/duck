// Extensao Git do Duck: mostra o repositorio do pane ativo (Windows ou WSL):
// branch, ahead/behind, mudancas, branches locais, worktrees e stashes.
// Precisa de "permissions": ["context"] para saber a pasta do pane.

import { execFile } from "node:child_process";
import { duck, run, Item, Section, ContextMsg } from "./duck";

type Repo = {
  root: string; head: string; oid: string; upstream: string; ahead: number; behind: number;
  files: { path: string; xy: string }[];
  branches: { name: string; when: string; track: string; subject: string }[];
  worktrees: { path: string; branch: string }[];
  stashes: { ref: string; when: string; msg: string }[];
};

const WIN = process.platform === "win32";
let ctx: { cwd: string; distro: string } = { cwd: "", distro: "" };
let repo: Repo | null = null;
let error = "";
let viewOpen = false;
let gen = 0;
let distros: string[] | null = null;

// ---------------------------------------------------------------- processo

function decode(b: Buffer): string {
  const s = (b[0] === 0xff && b[1] === 0xfe) || (b.length > 1 && b[1] === 0) ? b.toString("utf16le") : b.toString("utf8");
  return s.replace(/^﻿/, "").replace(/\0/g, "");
}

function exec(cmd: string, args: string[], timeout = 15000, raw = false): Promise<string> {
  return new Promise((res, rej) =>
    execFile(cmd, args, { windowsHide: true, timeout, encoding: "buffer", maxBuffer: 16 << 20 }, (e, out, err) => {
      if (e) rej(new Error((decode(err).trim() || e.message).split("\n")[0]));
      else res(raw ? out.toString("utf8") : decode(out));
    }));
}

async function wslDistros(): Promise<string[]> {
  if (distros) return distros;
  if (!WIN) return (distros = []);
  try { distros = (await exec("wsl.exe", ["-l", "-q"], 8000)).split(/\r?\n/).map((s) => s.trim()).filter(Boolean); }
  catch { distros = []; }
  return distros;
}

/** Linha de comando do git para a pasta do contexto (Windows: git -C; WSL: wsl -d --cd). */
function gitArgv(args: string[], c = ctx): string[] {
  return c.distro ? ["wsl.exe", "-d", c.distro, "--cd", c.cwd, "--exec", "git", ...args] : ["git", "-C", c.cwd, ...args];
}

function git(args: string[], timeout?: number, raw = false): Promise<string> {
  const a = gitArgv(args);
  return exec(a[0], a.slice(1), timeout, raw);
}

/** Roda num pane e segura a saida ate o Enter (o pane fecha quando o comando acaba). */
function paneCmd(args: string[]): string[] {
  if (ctx.distro) {
    return ["wsl.exe", "-d", ctx.distro, "--cd", ctx.cwd, "--exec", "sh", "-c", 'git "$@"; printf "\\n[Enter fecha] "; read _', "sh", ...args];
  }
  if (!WIN) return ["sh", "-c", 'git "$@"; printf "\\n[Enter fecha] "; read _', "sh", "-C", ctx.cwd, ...args];
  const q = (s: string) => `'${s.replace(/'/g, "''")}'`;
  return ["powershell.exe", "-NoProfile", "-Command", `& git -C ${q(ctx.cwd)} ${args.map(q).join(" ")}; Write-Host; Read-Host '[Enter fecha]'`];
}

// ---------------------------------------------------------------- leitura

async function load(): Promise<void> {
  const my = ++gen;
  if (!ctx.cwd) { repo = null; render(); return; }
  try {
    const [st, br, wt, sh] = await Promise.all([
      git(["status", "--porcelain=v2", "--branch", "-z"], 15000, true),
      git(["for-each-ref", "--sort=-committerdate", "--count=30", "--format=%(refname:short)%09%(committerdate:relative)%09%(upstream:track)%09%(subject)", "refs/heads"]),
      git(["worktree", "list", "--porcelain"]),
      git(["stash", "list", "--format=%gd%x09%cr%x09%gs"]),
    ]);
    const root = await git(["rev-parse", "--show-toplevel"]);
    if (my !== gen) return;
    repo = parse(st, br, wt, sh, root.trim());
    error = "";
  } catch (e) {
    if (my !== gen) return;
    repo = null;
    const msg = (e as Error).message;
    error = /not a git repository|não é um repositório/i.test(msg) ? ""
      : /execvpe\(git\)|git: not found/.test(msg) ? `git não está instalado na distro ${ctx.distro}` : msg;
  }
  render();
}

function parse(st: string, br: string, wt: string, sh: string, root: string): Repo {
  const r: Repo = { root, head: "", oid: "", upstream: "", ahead: 0, behind: 0, files: [], branches: [], worktrees: [], stashes: [] };
  const ent = st.split("\0");
  for (let i = 0; i < ent.length; i++) {
    const e = ent[i];
    if (!e) continue;
    const f = e.split(" ");
    if (e.startsWith("# branch.head ")) r.head = e.slice(14);
    else if (e.startsWith("# branch.oid ")) r.oid = e.slice(13);
    else if (e.startsWith("# branch.upstream ")) r.upstream = e.slice(18);
    else if (e.startsWith("# branch.ab ")) { r.ahead = Number(f[2]); r.behind = -Number(f[3]); }
    else if (f[0] === "1") r.files.push({ xy: f[1], path: f.slice(8).join(" ") });
    else if (f[0] === "2") { r.files.push({ xy: f[1], path: f.slice(9).join(" ") }); i++; } // proxima entrada e o nome antigo
    else if (f[0] === "u") r.files.push({ xy: "UU", path: f.slice(10).join(" ") });
    else if (f[0] === "?") r.files.push({ xy: "??", path: e.slice(2) });
  }
  for (const l of br.split(/\r?\n/).filter(Boolean)) {
    const [name, when, track, subject] = l.split("\t");
    r.branches.push({ name, when, track, subject });
  }
  let cur: { path: string; branch: string } | null = null;
  for (const l of wt.split(/\r?\n/)) {
    if (l.startsWith("worktree ")) r.worktrees.push((cur = { path: l.slice(9), branch: "" }));
    else if (cur && l.startsWith("branch ")) cur.branch = l.slice(7).replace(/^refs\/heads\//, "");
    else if (cur && l === "detached") cur.branch = "(detached)";
  }
  for (const l of sh.split(/\r?\n/).filter(Boolean)) {
    const [ref, when, msg] = l.split("\t");
    r.stashes.push({ ref, when, msg });
  }
  return r;
}

// ---------------------------------------------------------------- visao

const base = (p: string) => p.replace(/[\\/]+$/, "").split(/[\\/]/).pop() || p;
const same = (a: string, b: string) => a.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase() === b.replace(/\\/g, "/").replace(/\/$/, "").toLowerCase();

function fileColor(xy: string): string {
  if (xy === "??") return "muted";
  if (xy.includes("U")) return "blocked";
  return xy[0] !== "." ? "done" : "working"; // staged x so na arvore
}

function render(): void {
  if (!repo) {
    duck.view("git", [], { title: "Git", error: error || (ctx.cwd ? `${ctx.cwd} não é um repositório git` : "Sem pasta no pane ativo") });
    duck.status("git", "");
    return;
  }
  const r = repo;
  const n = r.files.length;
  const sync = [r.ahead ? `↑${r.ahead}` : "", r.behind ? `↓${r.behind}` : ""].filter(Boolean).join(" ");
  const head: Item = {
    id: "repo", icon: "repo", title: `${base(r.root)} · ${r.head}`,
    subtitle: [r.upstream ? `${r.upstream}${sync ? " " + sync : " em dia"}` : "sem upstream", n ? `${n} arquivo${n > 1 ? "s" : ""} mudado${n > 1 ? "s" : ""}` : "sem mudanças", ctx.distro ? `WSL ${ctx.distro}` : ""].filter(Boolean).join(" · "),
    color: r.behind ? "working" : "accent",
    actions: [{ id: "fetch", title: "Fetch", primary: true }, { id: "pull", title: "Pull" }, { id: "new-branch", title: "Nova branch" }],
  };
  const secs: Section[] = [
    { items: [head] },
    {
      title: `Mudanças (${n})`, items: r.files.map((f) => ({
        id: `f|${f.path}`, title: f.path, badge: f.xy.replace(/\./g, " ").trim() || f.xy, icon: "", color: fileColor(f.xy),
        actions: [{ id: "diff", title: "Diff", primary: true }],
      })),
    },
    {
      title: `Branches locais (${r.branches.length})`, items: r.branches.map((b) => ({
        id: `b|${b.name}`, title: b.name, icon: "branch", subtitle: [b.when, b.track, b.subject].filter(Boolean).join(" · "),
        color: b.name === r.head ? "accent" : "fg", badge: b.name === r.head ? "atual" : "",
        actions: b.name === r.head ? [] : [{ id: "switch", title: "Trocar", primary: true }],
      })),
    },
  ];
  if (r.worktrees.length > 1) {
    secs.push({
      title: `Worktrees (${r.worktrees.length})`, items: r.worktrees.map((w) => ({
        id: `w|${w.path}`, title: base(w.path), subtitle: `${w.branch} · ${w.path}`, icon: "",
        color: same(w.path, r.root) ? "accent" : "fg", actions: [{ id: "open-worktree", title: "Abrir numa aba", primary: true }],
      })),
    });
  }
  if (r.stashes.length) {
    secs.push({
      title: `Stashes (${r.stashes.length})`, items: r.stashes.map((s) => ({
        id: `s|${s.ref}`, title: s.msg, subtitle: `${s.ref} · ${s.when}`, icon: "",
        actions: [{ id: "stash-apply", title: "Apply", primary: true }, { id: "stash-pop", title: "Pop" }, { id: "stash-drop", title: "Drop" }],
      })),
    });
  }
  duck.view("git", secs, { title: `Git · ${base(r.root)}`, error });
  duck.status("git", ` ${r.head}${n ? ` ±${n}` : ""}${sync ? ` ${sync}` : ""}`, { color: n ? "working" : "accent", view: "git" });
}

// ---------------------------------------------------------------- acoes

const pending = new Map<string, (v: string) => Promise<void>>();

async function runGit(args: string[], ok: string): Promise<void> {
  try { await git(args, 120000); duck.flash(ok); }
  catch (e) { duck.flash(`git ${args[0]}: ${(e as Error).message}`); }
  await load();
}

function confirm(id: string, title: string, option: string, hint: string, fn: () => Promise<void>): void {
  pending.set(id, async (v) => { if (v === "yes") await fn(); });
  duck.choose(id, title, [{ id: "yes", title: option, hint }]);
}

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await load(); return; }
  if (!repo) return;
  const arg = item.slice(2);
  const n = repo.files.length;
  switch (action) {
    case "fetch": duck.flash("git fetch..."); await runGit(["fetch", "--all", "--prune"], "Fetch concluído"); break;
    case "pull": duck.openPane(paneCmd(["pull"]), { where: "float", title: `git pull · ${base(repo.root)}` }); break;
    case "new-branch":
      pending.set("new-branch", async (name) => { if (name) await runGit(["switch", "-c", name], `Branch ${name} criada`); });
      duck.prompt("new-branch", `Nova branch a partir de ${repo.head}`);
      break;
    case "switch":
      if (!n) { await runGit(["switch", arg], `Agora em ${arg}`); break; }
      confirm(`switch|${arg}`, `Trocar para ${arg} com ${n} ${n > 1 ? "mudanças locais" : "mudança local"}?`, "Trocar mesmo assim",
        "as mudanças vão junto; o git recusa se houver conflito", () => runGit(["switch", arg], `Agora em ${arg}`));
      break;
    case "diff": {
      const f = repo.files.find((x) => x.path === arg);
      const args = f?.xy === "??" ? ["--no-pager", "diff", "--color=always", "--no-index", "--", "/dev/null", arg]
        : ["--no-pager", "diff", "--color=always", ...(/^0+$/.test(repo.oid) || repo.oid === "(initial)" ? [] : ["HEAD"]), "--", arg];
      duck.openPane(paneCmd(args), { where: "float", title: `diff · ${base(arg)}` });
      break;
    }
    case "open-worktree":
      if (ctx.distro) duck.openPane(["wsl.exe", "-d", ctx.distro, "--cd", arg], { where: "tab", title: base(arg) });
      else duck.openPane([], { where: "tab", cwd: arg, title: base(arg) });
      break;
    case "stash-apply":
    case "stash-pop":
    case "stash-drop": {
      const verb = action.slice(6);
      const s = repo.stashes.find((x) => x.ref === arg);
      const hint = verb === "drop" ? "apaga o stash sem aplicar" : verb === "pop" ? "aplica e remove da lista" : "aplica e mantém na lista";
      confirm(`${action}|${arg}`, `stash ${verb} ${arg}${s ? ` (${s.msg})` : ""}?`, `stash ${verb}`, hint,
        () => runGit(["stash", verb, arg], `stash ${verb} ${arg}: ok`));
      break;
    }
  }
}

// ---------------------------------------------------------------- contexto

async function onContext(m: ContextMsg): Promise<void> {
  const cwd = (m.cwd ?? "").trim();
  let distro = "";
  if (WIN && cwd.startsWith("/") && m.domain) {
    const d = m.domain.replace(/^wsl:/i, "");
    distro = (await wslDistros()).find((x) => x.toLowerCase() === d.toLowerCase()) ?? "";
  }
  if (cwd === ctx.cwd && distro === ctx.distro) return;
  ctx = { cwd, distro };
  await load();
}

let timer: NodeJS.Timeout | undefined;

run({
  init() {
    duck.register([{ id: "git", title: "Git", icon: "branch" }], [{ id: "git", title: "Git: abrir" }, { id: "fetch", title: "Git: fetch" }, { id: "new-branch", title: "Git: nova branch" }]);
    render();
  },
  context: onContext,
  async viewOpen() {
    viewOpen = true;
    clearInterval(timer);
    timer = setInterval(() => { if (viewOpen) void load(); }, 20000);
    await load();
  },
  viewClose() { viewOpen = false; clearInterval(timer); },
  async command(m) { if (m.command === "git") duck.showView("git"); else await onAction(m.command, "repo"); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn) await fn(m.value.trim());
  },
});
