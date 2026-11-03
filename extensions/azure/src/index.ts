// Extensao Azure DevOps do Duck: PRs com revisao pedida a voce, seus PRs e
// builds recentes, em uma ou varias organizacoes. Avisa PR novo para revisar,
// comentario novo nos seus PRs e build que falhou. So leitura e comentario:
// nada de aprovar, completar ou abandonar daqui.
//
// Config (~/.duck.lua):
//   extensions = { azure = { orgs = {
//     { name = 'empresa', url = 'https://dev.azure.com/empresa', project = 'proj', secret = 'azure/empresa' },
//   } } }
// Token (PAT com Code Read e Build Read): echo <pat> | duck secret set azure/<name>

import { duck, run, Item, Section, Action } from "./duck";

type OrgCfg = { name: string; url: string; project: string; secret?: string };
type Settings = { orgs?: OrgCfg[]; poll_seconds?: number };
type Org = OrgCfg & { auth: string; me?: { id: string; name: string } };
type PR = {
  pullRequestId: number; title: string; status: string; isDraft: boolean; creationDate: string; mergeStatus?: string;
  createdBy: { displayName: string; id: string }; sourceRefName: string; targetRefName: string; description?: string;
  repository: { id: string; name: string }; reviewers: { id: string; displayName: string; vote: number; isRequired?: boolean }[];
};
type Build = {
  id: number; buildNumber: string; status: string; result?: string; queueTime: string; finishTime?: string;
  definition: { name: string }; sourceBranch: string; _links: { web: { href: string } };
};

let settings: Settings = {};
let orgs: Org[] = [];
const review = new Map<string, PR[]>();
const mine = new Map<string, PR[]>();
const builds = new Map<string, Build[]>();
const errors = new Map<string, string>();
const seenReview = new Set<string>();
const comments = new Map<string, number>();   // "org|pr" -> quantos comentarios de outras pessoas
const failed = new Set<string>();
let first = true;

const V = "api-version=7.1";
function root(o: Org) { return o.url.replace(/\/$/, ""); }

async function call(o: Org, path: string, init: RequestInit = {}): Promise<any> {
  const headers: Record<string, string> = { Authorization: o.auth, Accept: "application/json" };
  if (init.body) headers["Content-Type"] = "application/json";
  let r: Response;
  try { r = await fetch(root(o) + path, { ...init, headers, redirect: "manual" }); }
  catch { throw new Error(`sem acesso a ${root(o)}`); }
  // PAT vencido: o Azure responde 203/302 com a pagina de login em vez de 401.
  if (r.status === 203 || (r.status >= 300 && r.status < 400)) throw new Error("PAT vencido ou sem permissao (gere outro e grave com duck secret set)");
  const text = await r.text();
  if (!r.ok) {
    let msg = text.slice(0, 160);
    try { msg = JSON.parse(text).message ?? msg; } catch { /* html */ }
    throw new Error(`${r.status} ${msg}`);
  }
  return text ? JSON.parse(text) : null;
}

function ago(iso: string): string {
  const s = Math.max(0, (Date.now() - Date.parse(iso)) / 1000);
  if (s < 3600) return `ha ${Math.max(1, Math.floor(s / 60))}min`;
  if (s < 86400) return `ha ${Math.floor(s / 3600)}h`;
  return `ha ${Math.floor(s / 86400)}d`;
}
const branch = (r: string) => r.replace(/^refs\/heads\//, "");
const VOTE: Record<string, string> = { "10": "aprovado", "5": "aprovado c/ sugestoes", "0": "sem voto", "-5": "aguardando autor", "-10": "rejeitado" };
const prUrl = (o: Org, p: PR) => `${root(o)}/${encodeURIComponent(o.project)}/_git/${encodeURIComponent(p.repository.name)}/pullrequest/${p.pullRequestId}`;

async function loadOrgs(): Promise<Org[]> {
  const out: Org[] = [];
  for (const c of settings.orgs ?? []) {
    const pat = await duck.secret(c.secret ?? `azure/${c.name}`);
    if (!pat) { errors.set(c.name, `sem token: echo <pat> | duck secret set ${c.secret ?? "azure/" + c.name}`); continue; }
    const o: Org = { ...c, auth: "Basic " + Buffer.from(":" + pat).toString("base64") };
    try {
      const cd = await call(o, `/_apis/connectionData?api-version=7.1-preview`);
      o.me = { id: cd.authenticatedUser.id, name: cd.authenticatedUser.providerDisplayName };
    } catch (e) { errors.set(c.name, String((e as Error).message)); }
    out.push(o);
  }
  return out;
}

async function threadsFromOthers(o: Org, p: PR): Promise<{ count: number; last?: { author: string; text: string } }> {
  const t = await call(o, `/${encodeURIComponent(o.project)}/_apis/git/repositories/${p.repository.id}/pullRequests/${p.pullRequestId}/threads?${V}`);
  const list = (t.value ?? []).flatMap((th: any) => th.comments ?? []).filter((c: any) => c.commentType !== "system" && c.author?.id !== o.me?.id);
  list.sort((a: any, b: any) => Date.parse(a.publishedDate) - Date.parse(b.publishedDate));
  const last = list[list.length - 1];
  return { count: list.length, last: last ? { author: last.author.displayName, text: String(last.content ?? "") } : undefined };
}

async function poll(): Promise<void> {
  for (const o of orgs) {
    if (!o.me) continue;
    const proj = encodeURIComponent(o.project);
    try {
      const [rv, my, bl] = await Promise.all([
        call(o, `/${proj}/_apis/git/pullrequests?searchCriteria.reviewerId=${o.me.id}&searchCriteria.status=active&$top=50&${V}`),
        call(o, `/${proj}/_apis/git/pullrequests?searchCriteria.creatorId=${o.me.id}&searchCriteria.status=active&$top=50&${V}`),
        call(o, `/${proj}/_apis/build/builds?requestedFor=${o.me.id}&$top=12&queryOrder=queueTimeDescending&${V}`),
      ]);
      const reviewList: PR[] = (rv.value ?? []).filter((p: PR) => p.createdBy.id !== o.me!.id);
      review.set(o.name, reviewList); mine.set(o.name, my.value ?? []); builds.set(o.name, bl.value ?? []);
      errors.delete(o.name);
      // Avisos: PR novo para revisar, comentario novo nos meus, build que falhou.
      for (const p of reviewList) {
        const k = `${o.name}|${p.pullRequestId}`;
        if (!seenReview.has(k) && !first) duck.notify({ title: `Azure · revisao pedida: !${p.pullRequestId}`, body: `${p.title} (${p.createdBy.displayName})`, item: `p|${o.name}|${p.repository.id}|${p.pullRequestId}`, actions: [{ id: "open", title: "Abrir" }, { id: "web", title: "Navegador" }], color: "accent" });
        seenReview.add(k);
      }
      for (const p of my.value ?? []) {
        const k = `${o.name}|${p.pullRequestId}`;
        try {
          const t = await threadsFromOthers(o, p);
          const before = comments.get(k);
          if (before !== undefined && t.count > before && t.last) duck.notify({ title: `Azure · comentario no !${p.pullRequestId} (${t.last.author})`, body: t.last.text.split("\n")[0].slice(0, 140), item: `p|${o.name}|${p.repository.id}|${p.pullRequestId}`, actions: [{ id: "open", title: "Abrir" }, { id: "web", title: "Navegador" }], color: "accent" });
          comments.set(k, t.count);
        } catch (e) { duck.log("threads", k, e); }
      }
      for (const b of bl.value ?? []) {
        const k = `${o.name}|${b.id}`;
        if (b.result === "failed" && !failed.has(k)) {
          if (!first) duck.notify({ title: `Azure · build falhou: ${b.definition.name}`, body: `${b.buildNumber} · ${branch(b.sourceBranch)}`, item: `b|${o.name}|${b.id}`, actions: [{ id: "web", title: "Abrir no navegador" }], color: "blocked" });
          failed.add(k);
        }
      }
    } catch (e) { errors.set(o.name, String((e as Error).message)); duck.log("poll", o.name, e); }
  }
  first = false;
  render();
}

function prItem(o: Org, p: PR, reviewing: boolean): Item {
  const myVote = p.reviewers.find((r) => r.id === o.me?.id)?.vote;
  const approved = p.reviewers.filter((r) => r.vote >= 5).length;
  const badge = p.isDraft ? "rascunho" : p.mergeStatus === "conflicts" ? "conflito" : reviewing ? (VOTE[String(myVote ?? 0)] ?? "") : `${approved}/${p.reviewers.length} aprov.`;
  const actions: Action[] = [{ id: "open", title: "Abrir", primary: true }, { id: "comment", title: "Comentar" }, { id: "web", title: "Navegador" }];
  return {
    id: `p|${o.name}|${p.repository.id}|${p.pullRequestId}`, title: `!${p.pullRequestId}  ${p.title}`,
    subtitle: `${p.repository.name} · ${branch(p.sourceRefName)} → ${branch(p.targetRefName)}${reviewing ? ` · ${p.createdBy.displayName}` : ""} · ${ago(p.creationDate)}`,
    icon: p.isDraft ? "pr-draft" : "pr", color: p.mergeStatus === "conflicts" ? "blocked" : p.isDraft ? "muted" : "accent", badge, actions,
  };
}

function buildItem(o: Org, b: Build): Item {
  const res = b.status !== "completed" ? "rodando" : (b.result ?? "");
  const color = res === "succeeded" ? "done" : res === "failed" ? "blocked" : res === "rodando" ? "working" : "muted";
  const label: Record<string, string> = { succeeded: "ok", failed: "falhou", canceled: "cancelado", partiallySucceeded: "parcial", rodando: "rodando" };
  return {
    id: `b|${o.name}|${b.id}`, title: `${b.definition.name}  ${b.buildNumber}`, icon: res === "failed" ? "x" : res === "rodando" ? "clock" : "check",
    subtitle: `${branch(b.sourceBranch)} · ${ago(b.finishTime ?? b.queueTime)}`, color, badge: label[res] ?? res,
    actions: [{ id: "web", title: "Abrir", primary: true }],
  };
}

function render(): void {
  const multi = orgs.length > 1;
  const secs: Section[] = [];
  let pending = 0;
  for (const o of orgs) {
    const p = multi ? `${o.name} · ` : "";
    const rv = review.get(o.name) ?? [];
    pending += rv.filter((x) => (x.reviewers.find((r) => r.id === o.me?.id)?.vote ?? 0) === 0).length;
    secs.push({ title: `${p}Revisao pedida (${rv.length})`, items: rv.map((x) => prItem(o, x, true)) });
    secs.push({ title: `${p}Meus PRs (${mine.get(o.name)?.length ?? 0})`, items: (mine.get(o.name) ?? []).map((x) => prItem(o, x, false)) });
    secs.push({ title: `${p}Builds recentes`, items: (builds.get(o.name) ?? []).map((b) => buildItem(o, b)) });
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("prs", secs, { error: err });
  duck.status("prs", ` ${pending}`, { color: pending ? "accent" : "muted", view: "prs" });
}

async function showPR(o: Org, repoId: string, id: number): Promise<void> {
  const proj = encodeURIComponent(o.project);
  const [p, th]: [PR, any] = await Promise.all([
    call(o, `/${proj}/_apis/git/repositories/${repoId}/pullRequests/${id}?${V}`),
    call(o, `/${proj}/_apis/git/repositories/${repoId}/pullRequests/${id}/threads?${V}`),
  ]);
  const cs = (th.value ?? []).flatMap((t: any) => (t.comments ?? []).map((c: any) => ({ ...c, file: t.threadContext?.filePath })))
    .filter((c: any) => c.commentType !== "system")
    .sort((a: any, b: any) => Date.parse(a.publishedDate) - Date.parse(b.publishedDate)).slice(-15);
  const body = [
    (p.description ?? "").replace(/\r/g, "").trim() || "(sem descricao)",
    "",
    "── Revisores ──",
    ...p.reviewers.map((r) => `${r.displayName}${r.isRequired ? " (obrigatorio)" : ""}: ${VOTE[String(r.vote)] ?? r.vote}`),
    "",
    `── Comentarios (${cs.length}) ──`,
    ...cs.flatMap((c: any) => ["", `# ${c.author?.displayName} · ${ago(c.publishedDate)}${c.file ? ` · ${c.file}` : ""}`, ...String(c.content ?? "").replace(/\r/g, "").split("\n")]),
  ].join("\n");
  duck.detail({
    title: `PR !${id} · ${p.title}`, item: `p|${o.name}|${repoId}|${id}`,
    subtitle: `${p.repository.name} · ${branch(p.sourceRefName)} → ${branch(p.targetRefName)} · por ${p.createdBy.displayName} ${ago(p.creationDate)}${p.isDraft ? " · rascunho" : ""}${p.mergeStatus === "conflicts" ? " · com conflito" : ""}`,
    body, actions: [{ id: "comment", title: "Comentar" }, { id: "web", title: "Navegador" }],
  });
}

const waiting = new Map<string, (v: string) => Promise<void>>();

async function onAction(action: string, item: string): Promise<void> {
  if (action === "refresh") { await poll(); return; }
  const [kind, name, a, b] = item.split("|");
  const o = orgs.find((x) => x.name === name);
  if (!o) return;
  if (kind === "b") {
    const bd = (builds.get(name) ?? []).find((x) => String(x.id) === a);
    duck.openUrl(bd?._links.web.href ?? `${root(o)}/${encodeURIComponent(o.project)}/_build/results?buildId=${a}`);
    return;
  }
  const id = Number(b);
  const all = [...(review.get(name) ?? []), ...(mine.get(name) ?? [])];
  const pr = all.find((x) => x.pullRequestId === id);
  switch (action) {
    case "open": await showPR(o, a, id); break;
    case "web": duck.openUrl(pr ? prUrl(o, pr) : `${root(o)}/${encodeURIComponent(o.project)}/_git/${a}/pullrequest/${id}`); break;
    case "comment":
      waiting.set(item, async (text) => {
        await call(o, `/${encodeURIComponent(o.project)}/_apis/git/repositories/${a}/pullRequests/${id}/threads?${V}`,
          { method: "POST", body: JSON.stringify({ comments: [{ parentCommentId: 0, content: text, commentType: 1 }], status: 1 }) });
        duck.flash(`Comentario enviado no !${id}`);
        await showPR(o, a, id);
      });
      duck.prompt(item, `Comentario no PR !${id}`);
      break;
  }
}

run({
  async init(m) {
    settings = (m.settings ?? {}) as Settings;
    duck.register([{ id: "prs", title: "Azure DevOps · PRs e builds", icon: "azure" }], [{ id: "prs", title: "Azure DevOps: PRs e builds" }]);
    if (!settings.orgs?.length) { duck.view("prs", [], { error: "Configure extensions.azure.orgs no ~/.duck.lua" }); return; }
    orgs = await loadOrgs();
    await poll();
    setInterval(() => void poll(), Math.max(60, settings.poll_seconds ?? 120) * 1000);
  },
  async viewOpen() { render(); },
  async command(m) { duck.showView(m.command); },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = waiting.get(m.id);
    waiting.delete(m.id);
    if (fn && m.value.trim()) await fn(m.value.trim());
  },
});
