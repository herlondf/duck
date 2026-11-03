// Extensao GitHub do Duck: notificacoes, issues/PRs e repositorios, em uma ou
// varias contas. Os tokens vem do `gh` (gh auth login), que ja guarda varias
// contas: nada de token em arquivo.
//
// Config opcional (~/.duck.lua):
//   extensions = { github = { accounts = { 'conta1', 'conta2' }, poll_seconds = 60 } }

import { execFile } from "node:child_process";
import { promisify } from "node:util";
import * as os from "node:os";
import * as path from "node:path";
import { duck, run, Item, Section, Action } from "./duck";

const exec = promisify(execFile);
const API = "https://api.github.com";

type Settings = { accounts?: string[]; poll_seconds?: number };
type Account = { login: string; token: string };

type Notification = {
  id: string; unread: boolean; reason: string; updated_at: string;
  subject: { title: string; url: string | null; type: string };
  repository: { full_name: string; html_url: string };
};
type Issue = {
  number: number; title: string; html_url: string; state: string; comments: number; updated_at: string;
  user: { login: string }; repository_url: string; pull_request?: unknown; draft?: boolean;
  labels: { name: string }[]; body: string | null; created_at: string;
};
type Repo = { full_name: string; html_url: string; description: string | null; language: string | null; stargazers_count: number; pushed_at: string; private: boolean; open_issues_count: number };

let settings: Settings = {};
let accounts: Account[] = [];
const notes = new Map<string, Notification[]>();     // login -> notificacoes nao lidas
const lastMod = new Map<string, string>();           // login -> Last-Modified (If-Modified-Since)
const work = new Map<string, { review: Issue[]; mine: Issue[]; assigned: Issue[] }>();
const repos = new Map<string, Repo[]>();
const seen = new Set<string>();                      // notificacoes ja avisadas
let firstPoll = true;
let workLoaded = 0, reposLoaded = 0;

// ---------------------------------------------------------------- utilidades

function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 60) return "agora";
  if (s < 3600) return `ha ${Math.floor(s / 60)}min`;
  if (s < 86400) return `ha ${Math.floor(s / 3600)}h`;
  return `ha ${Math.floor(s / 86400)}d`;
}

const REASON: Record<string, string> = {
  assign: "atribuida a voce", author: "voce criou", comment: "comentario", ci_activity: "CI",
  invitation: "convite", manual: "voce assina", mention: "mencionou voce", review_requested: "revisao pedida",
  security_alert: "alerta de seguranca", state_change: "mudou de estado", subscribed: "voce acompanha",
  team_mention: "mencionou seu time", approval_requested: "aprovacao pedida",
};

async function gh(...args: string[]): Promise<string> {
  const { stdout } = await exec("gh", args, { windowsHide: true, maxBuffer: 1 << 20 });
  return stdout.trim();
}

async function loadAccounts(): Promise<Account[]> {
  let logins = settings.accounts ?? [];
  if (!logins.length) {
    const st = JSON.parse(await gh("auth", "status", "--json", "hosts"));
    logins = (st.hosts?.["github.com"] ?? []).filter((h: any) => h.state === "success").map((h: any) => h.login);
  }
  const out: Account[] = [];
  for (const login of logins) {
    try { out.push({ login, token: await gh("auth", "token", "--user", login, "--hostname", "github.com") }); }
    catch (e) { duck.log("sem token para", login, e); }
  }
  return out;
}

async function api(acc: Account, p: string, init: RequestInit & { ifModified?: string } = {}): Promise<{ status: number; data: any; headers: Headers }> {
  const headers: Record<string, string> = {
    Authorization: `Bearer ${acc.token}`, Accept: "application/vnd.github+json",
    "X-GitHub-Api-Version": "2022-11-28", "User-Agent": "duck-github-extension",
  };
  if (init.ifModified) headers["If-Modified-Since"] = init.ifModified;
  if (init.body) headers["Content-Type"] = "application/json";
  const r = await fetch(p.startsWith("http") ? p : API + p, { ...init, headers });
  if (r.status === 304) return { status: 304, data: null, headers: r.headers };
  const text = await r.text();
  const data = text ? JSON.parse(text) : null;
  if (!r.ok) throw new Error(`${r.status} ${data?.message ?? ""} (${p})`);
  return { status: r.status, data, headers: r.headers };
}

const acct = (login: string) => accounts.find((a) => a.login === login)!;
const multi = () => accounts.length > 1;
const repoOf = (i: Issue) => i.repository_url.replace(`${API}/repos/`, "");

// ---------------------------------------------------------------- notificacoes

async function pollNotifications(): Promise<void> {
  for (const acc of accounts) {
    try {
      const r = await api(acc, "/notifications?per_page=50", { ifModified: lastMod.get(acc.login) });
      if (r.status === 304) continue;
      const lm = r.headers.get("last-modified");
      if (lm) lastMod.set(acc.login, lm);
      const list = (r.data as Notification[]).filter((n) => n.unread);
      // Avisa as que chegaram desde a ultima leitura (nao na primeira).
      const fresh = list.filter((n) => !seen.has(n.id + n.updated_at));
      list.forEach((n) => seen.add(n.id + n.updated_at));
      if (!firstPoll) {
        for (const n of fresh.slice(0, 3)) {
          duck.notify({
            title: `GitHub · ${n.repository.full_name}`, body: `${n.subject.title} (${REASON[n.reason] ?? n.reason})`,
            item: `n|${acc.login}|${n.id}`, actions: [{ id: "open", title: "Abrir" }, { id: "read", title: "Marcar lida" }],
          });
        }
        if (fresh.length > 3) duck.notify({ title: "GitHub", body: `+${fresh.length - 3} notificacoes novas`, item: "", actions: [{ id: "open-inbox", title: "Ver" }] });
      }
      notes.set(acc.login, list);
    } catch (e) { duck.log("notificacoes", acc.login, e); }
  }
  firstPoll = false;
  renderInbox();
}

function noteIcon(type: string): string {
  return ({ Issue: "issue", PullRequest: "pr", Release: "tag", Discussion: "comment", CheckSuite: "check", Commit: "branch" } as Record<string, string>)[type] ?? "bell";
}

function renderInbox(): void {
  const sections: Section[] = [];
  let total = 0;
  for (const acc of accounts) {
    const list = notes.get(acc.login) ?? [];
    total += list.length;
    sections.push({
      title: `${multi() ? "@" + acc.login + " · " : ""}Nao lidas (${list.length})`,
      items: list.map((n): Item => ({
        id: `n|${acc.login}|${n.id}`, title: n.subject.title, unread: true,
        subtitle: `${n.repository.full_name} · ${REASON[n.reason] ?? n.reason} · ${ago(n.updated_at)}`,
        icon: noteIcon(n.subject.type), color: n.subject.type === "PullRequest" ? "accent" : "working",
        actions: [{ id: "open", title: "Abrir", primary: true }, { id: "read", title: "Lida" }, { id: "web", title: "Navegador" }],
      })),
    });
  }
  duck.view("inbox", sections);
  duck.status("inbox", ` ${total}`, { color: total ? "accent" : "muted", view: "inbox" });
}

// ---------------------------------------------------------------- issues e PRs

async function search(acc: Account, q: string): Promise<Issue[]> {
  const r = await api(acc, `/search/issues?per_page=30&sort=updated&q=${encodeURIComponent(q)}`);
  return r.data.items as Issue[];
}

async function loadWork(): Promise<void> {
  duck.view("work", [], { loading: true });
  for (const acc of accounts) {
    try {
      const [review, mine, assigned] = await Promise.all([
        search(acc, "is:open is:pr review-requested:@me archived:false"),
        search(acc, "is:open is:pr author:@me archived:false"),
        search(acc, "is:open is:issue assignee:@me archived:false"),
      ]);
      work.set(acc.login, { review, mine, assigned });
    } catch (e) { duck.log("issues/PRs", acc.login, e); duck.view("work", [], { error: String((e as Error).message) }); }
  }
  workLoaded = Date.now();
  renderWork();
}

function issueItem(login: string, i: Issue): Item {
  const pr = !!i.pull_request;
  const repo = repoOf(i);
  const actions: Action[] = [{ id: "open", title: "Abrir", primary: true }];
  if (pr) actions.push({ id: "diff", title: "Diff" });
  actions.push({ id: "web", title: "Navegador" });
  return {
    id: `i|${login}|${repo}#${i.number}`, title: `#${i.number} ${i.title}`,
    subtitle: `${repo} · ${ago(i.updated_at)}${i.comments ? ` · ${i.comments} coment.` : ""}`,
    icon: pr ? (i.draft ? "pr-draft" : "pr") : "issue", color: pr ? "accent" : "done",
    badge: i.draft ? "rascunho" : (i.labels[0]?.name ?? ""), actions,
  };
}

function renderWork(): void {
  const sections: Section[] = [];
  for (const acc of accounts) {
    const w = work.get(acc.login);
    if (!w) continue;
    const p = multi() ? `@${acc.login} · ` : "";
    sections.push({ title: `${p}Revisao pedida (${w.review.length})`, items: w.review.map((i) => issueItem(acc.login, i)) });
    sections.push({ title: `${p}Meus PRs abertos (${w.mine.length})`, items: w.mine.map((i) => issueItem(acc.login, i)) });
    sections.push({ title: `${p}Issues atribuidas (${w.assigned.length})`, items: w.assigned.map((i) => issueItem(acc.login, i)) });
  }
  duck.view("work", sections);
}

// ---------------------------------------------------------------- repositorios

async function loadRepos(): Promise<void> {
  duck.view("repos", [], { loading: true });
  for (const acc of accounts) {
    try {
      const r = await api(acc, "/user/repos?per_page=40&sort=pushed&affiliation=owner,collaborator,organization_member");
      repos.set(acc.login, r.data as Repo[]);
    } catch (e) { duck.log("repos", acc.login, e); }
  }
  reposLoaded = Date.now();
  const sections: Section[] = accounts.map((acc) => ({
    title: `${multi() ? "@" + acc.login + " · " : ""}Repositorios (${repos.get(acc.login)?.length ?? 0})`,
    items: (repos.get(acc.login) ?? []).map((r): Item => ({
      id: `r|${acc.login}|${r.full_name}`, title: r.full_name,
      subtitle: [r.description, r.language, r.stargazers_count ? `★ ${r.stargazers_count}` : "", `push ${ago(r.pushed_at)}`].filter(Boolean).join(" · "),
      icon: "repo", color: r.private ? "working" : "muted", badge: r.private ? "privado" : "",
      actions: [{ id: "open", title: "Abrir", primary: true }, { id: "clone", title: "Clonar" }, { id: "web", title: "Navegador" }],
    })),
  }));
  duck.view("repos", sections);
}

// ---------------------------------------------------------------- detalhes

function clean(md: string | null): string {
  return (md ?? "").replace(/<!--[\s\S]*?-->/g, "").replace(/!\[[^\]]*\]\([^)]*\)/g, "[imagem]")
    .replace(/<img[^>]*>/g, "[imagem]").replace(/\r/g, "").trim();
}

async function showIssue(login: string, repo: string, num: number, fromNote?: string): Promise<void> {
  const acc = acct(login);
  const [{ data: i }, { data: comments }] = await Promise.all([
    api(acc, `/repos/${repo}/issues/${num}`),
    api(acc, `/repos/${repo}/issues/${num}/comments?per_page=100`),
  ]);
  const pr = !!i.pull_request;
  let extra = "";
  if (pr) {
    const { data: p } = await api(acc, `/repos/${repo}/pulls/${num}`);
    extra = ` · ${p.merged ? "mesclado" : p.state === "open" ? (p.draft ? "rascunho" : "aberto") : "fechado"} · +${p.additions} −${p.deletions} em ${p.changed_files} arquivos · ${p.head.ref} → ${p.base.ref}`;
  }
  const labels = i.labels.map((l: any) => l.name).join(", ");
  const last = (comments as any[]).slice(-10);
  const body = [
    clean(i.body) || "(sem descricao)",
    "",
    `── Comentarios (${i.comments}${i.comments > last.length ? `, ultimos ${last.length}` : ""}) ──`,
    ...last.flatMap((c: any) => ["", `# @${c.user.login} · ${ago(c.created_at)}`, ...clean(c.body).split("\n")]),
  ].join("\n");
  const item = `i|${login}|${repo}#${num}`;
  const actions: (Action & { item?: string })[] = [{ id: "comment", title: "Comentar" }];
  actions.push({ id: i.state === "open" ? "close" : "reopen", title: i.state === "open" ? "Fechar" : "Reabrir" });
  if (pr) actions.push({ id: "diff", title: "Ver diff" });
  actions.push({ id: "web", title: "Navegador" });
  if (fromNote) actions.push({ id: "read", title: "Marcar lida", item: fromNote });
  duck.detail({
    title: `${pr ? "PR" : "Issue"} #${num} · ${i.title}`, item,
    subtitle: `${repo} · ${i.state === "open" ? "aberta" : "fechada"} por @${i.user.login} ${ago(i.created_at)}${labels ? " · " + labels : ""}${extra}`,
    body, actions,
  });
}

async function showRepo(login: string, full: string): Promise<void> {
  const acc = acct(login);
  const r = repos.get(login)?.find((x) => x.full_name === full);
  const open = await search(acc, `repo:${full} is:open`);
  const lines = open.map((i) => `${i.pull_request ? "PR   " : "Issue"} #${i.number}  ${i.title}  (${ago(i.updated_at)}, @${i.user.login})`);
  duck.detail({
    title: full, item: `r|${login}|${full}`,
    subtitle: [r?.description, r?.language, r ? `★ ${r.stargazers_count}` : "", r ? `push ${ago(r.pushed_at)}` : ""].filter(Boolean).join(" · "),
    body: [`# Abertos (${open.length})`, "", ...(lines.length ? lines : ["nada aberto"])].join("\n"),
    actions: [{ id: "web", title: "Navegador" }, { id: "clone", title: "Clonar" }, { id: "issues-web", title: "Issues no navegador" }],
  });
}

// ---------------------------------------------------------------- acoes

const pending = new Map<string, (v: string) => Promise<void>>();

function parseIssue(rest: string): { repo: string; num: number } {
  const [repo, n] = rest.split("#");
  return { repo, num: Number(n) };
}

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await Promise.all([pollNotifications(), loadWork(), loadRepos()]); return; }
  if (action === "open-inbox") { duck.showView("inbox"); return; }
  const [kind, login, rest] = item.split("|");
  const acc = acct(login);
  if (!acc) return;
  if (kind === "n") {
    const n = (notes.get(login) ?? []).find((x) => x.id === rest);
    if (action === "read") {
      await api(acc, `/notifications/threads/${rest}`, { method: "PATCH" });
      notes.set(login, (notes.get(login) ?? []).filter((x) => x.id !== rest));
      renderInbox();
      duck.flash("Notificacao marcada como lida");
      return;
    }
    if (!n) return;
    const subject = n.subject.url ? (await api(acc, n.subject.url)).data : null;
    if (action === "web" || !subject?.number) { duck.openUrl(subject?.html_url ?? n.repository.html_url); return; }
    await showIssue(login, n.repository.full_name, subject.number, item);
    return;
  }
  if (kind === "i") {
    const { repo, num } = parseIssue(rest);
    switch (action) {
      case "open": await showIssue(login, repo, num); break;
      case "web": duck.openUrl(`https://github.com/${repo}/issues/${num}`); break;
      case "diff": duck.openPane(["pwsh", "-NoLogo", "-NoExit", "-Command", `$env:GH_TOKEN = (gh auth token --user ${login}); gh pr diff ${num} -R ${repo}`], { where: "float" }); break;
      case "comment":
        pending.set(item, async (text) => {
          await api(acc, `/repos/${repo}/issues/${num}/comments`, { method: "POST", body: JSON.stringify({ body: text }) });
          duck.flash("Comentario enviado");
          await showIssue(login, repo, num);
        });
        duck.prompt(item, `Comentario em ${repo}#${num}`);
        break;
      case "close": case "reopen":
        await api(acc, `/repos/${repo}/issues/${num}`, { method: "PATCH", body: JSON.stringify({ state: action === "close" ? "closed" : "open" }) });
        duck.flash(action === "close" ? `#${num} fechada` : `#${num} reaberta`);
        await Promise.all([showIssue(login, repo, num), loadWork()]);
        break;
    }
    return;
  }
  if (kind === "r") {
    switch (action) {
      case "open": await showRepo(login, rest); break;
      case "web": duck.openUrl(`https://github.com/${rest}`); break;
      case "issues-web": duck.openUrl(`https://github.com/${rest}/issues`); break;
      case "clone":
        pending.set(item, async (dir) => {
          duck.openPane(["pwsh", "-NoLogo", "-NoExit", "-Command", `$env:GH_TOKEN = (gh auth token --user ${login}); gh repo clone ${rest}`], { cwd: dir, title: rest.split("/")[1] });
        });
        duck.prompt(item, `Clonar ${rest} na pasta`, path.join(os.homedir(), "projetos"));
        break;
    }
  }
}

// ---------------------------------------------------------------- inicio

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register(
      [
        { id: "inbox", title: "GitHub · Notificacoes", icon: "bell" },
        { id: "work", title: "GitHub · Issues e PRs", icon: "github" },
        { id: "repos", title: "GitHub · Repositorios", icon: "repo" },
      ],
      [
        { id: "inbox", title: "GitHub: notificacoes" },
        { id: "work", title: "GitHub: issues e PRs" },
        { id: "repos", title: "GitHub: repositorios" },
        { id: "goto", title: "GitHub: abrir issue/PR (dono/repo#123)" },
      ],
    );
    accounts = await loadAccounts();
    if (!accounts.length) {
      duck.view("inbox", [], { error: "Nenhuma conta do GitHub: rode `gh auth login`" });
      return;
    }
    await pollNotifications();
    const every = Math.max(30, settings.poll_seconds ?? 60) * 1000;
    setInterval(() => void pollNotifications(), every);
    // Issues/PRs e repos: na hora de abrir a visao e a cada 5 min.
    setInterval(() => { if (workLoaded) void loadWork(); }, 5 * 60_000);
  },
  async viewOpen(m) {
    if (m.view === "work" && Date.now() - workLoaded > 60_000) await loadWork();
    if (m.view === "repos" && Date.now() - reposLoaded > 5 * 60_000) await loadRepos();
    if (m.view === "inbox") renderInbox();
  },
  async command(m) {
    if (m.command === "goto") { duck.prompt("goto", "Issue ou PR (dono/repo#123)"); return; }
    duck.showView(m.command);
  },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    if (m.id === "goto") {
      const mm = /^([\w.-]+\/[\w.-]+)#(\d+)$/.exec(m.value.trim());
      if (!mm) { duck.flash("Formato: dono/repo#123"); return; }
      await showIssue(accounts[0].login, mm[1], Number(mm[2]));
      return;
    }
    const fn = pending.get(m.id);
    pending.delete(m.id);
    if (fn && m.value.trim()) await fn(m.value.trim());
  },
});
