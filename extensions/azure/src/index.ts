// Extensao Azure DevOps do Duck: PRs com revisao pedida a voce, seus PRs,
// builds recentes e o que esta publicado em cada ambiente (releases), em uma
// ou varias organizacoes. Avisa PR novo para revisar, comentario novo nos seus
// PRs, build que falhou e deploy que terminou. So leitura e comentario: nada
// de aprovar, completar, abandonar ou publicar daqui.
//
// Config (~/.duck.lua):
//   extensions = { azure = { orgs = {
//     { name = 'empresa', url = 'https://dev.azure.com/empresa', project = 'proj', secret = 'azure/empresa',
//       releases = {
//         -- Release classica: o ultimo deploy de cada ambiente ("rotulo=Ambiente" troca o nome mostrado).
//         { name = 'API', definition = 'api-cd', stages = { 'develop=Dev', 'tst=Tst', 'master=Prod' } },
//         -- Pipeline YAML (ou CI): o ultimo build de cada branch.
//         { name = 'Portal', pipeline = 'portal-ci', branches = { 'develop', 'tst', 'master' } },
//       } },
//   } } }
// Token (PAT com Code Read, Build Read e Release Read): echo <pat> | duck secret set azure/<name>
// Paleta -> "Azure: definicoes de release" lista as definicoes e ambientes para copiar.

import { duck, run, Item, Section, Action } from "./duck";

type RelCfg = { name?: string; project?: string; definition?: string | number; stages?: string[]; pipeline?: string | number; branches?: string[] };
type OrgCfg = { name: string; url: string; project: string; secret?: string; releases?: RelCfg[]; release_url?: string; release_notify?: boolean };
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

// Releases: um cartao por projeto configurado, uma linha por ambiente/branch.
type Stage = {
  label: string; status: string; when?: string; who?: string; release?: string; version?: string; branch?: string;
  url?: string; history?: { defId: number; envId: number } | { buildDef: number; branch: string }; error?: string;
};
type RelView = { name: string; stages: Stage[] };
const releases = new Map<string, RelView[]>();     // org -> projetos
const relDefs = new Map<string, { id: number; envs: Map<string, number>; names: string[] } | null>();   // "org|proj|def" -> definicao
const buildDefs = new Map<string, number | null>();  // "org|proj|pipeline" -> id
const lastDeploy = new Map<string, string>();      // "org|proj|label" -> "id:status" (para avisar)

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
// URL do Release Management: dev.azure.com/x -> vsrm.dev.azure.com/x; x.visualstudio.com -> x.vsrm.visualstudio.com.
function vsrm(o: Org): string {
  if (o.release_url) return o.release_url.replace(/\/$/, "");
  const u = root(o);
  if (/\/\/dev\.azure\.com\//i.test(u)) return u.replace(/\/\/dev\.azure\.com\//i, "//vsrm.dev.azure.com/");
  return u.replace(/\/\/([^./]+)\.visualstudio\.com/i, "//$1.vsrm.visualstudio.com");
}
async function rm(o: Org, path: string): Promise<any> { return call({ ...o, url: vsrm(o) }, path); }
// "tst=Tst" -> rotulo tst, ambiente Tst; "Tst" -> os dois iguais.
const pair = (x: string): [string, string] => { const i = x.indexOf("="); return i < 0 ? [x.trim(), x.trim()] : [x.slice(0, i).trim(), x.slice(i + 1).trim()]; };
const STATUS: Record<string, [string, string, string]> = {
  succeeded: ["ok", "done", "check"], failed: ["falhou", "blocked", "x"], partiallySucceeded: ["parcial", "working", "warning"],
  canceled: ["cancelado", "muted", "x"], inProgress: ["publicando", "working", "clock"], notDeployed: ["sem deploy", "muted", "clock"],
  pending: ["aguardando aprovacao", "working", "clock"], undefined: ["sem deploy", "muted", "clock"],
  rodando: ["rodando", "working", "clock"], nobuild: ["sem build", "muted", "clock"],
};
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
    if (o.releases?.length) {
      try { await pollReleases(o); } catch (e) { duck.log("releases", o.name, e); }
    }
  }
  first = false;
  render();
}

async function relDef(o: Org, proj: string, def: string | number): Promise<{ id: number; envs: Map<string, number>; names: string[] } | null> {
  const k = `${o.name}|${proj}|${def}`;
  if (relDefs.has(k)) return relDefs.get(k)!;
  const p = encodeURIComponent(proj);
  let d: any = null;
  if (typeof def === "number" || /^\d+$/.test(String(def))) d = await rm(o, `/${p}/_apis/release/definitions/${def}?${V}`);
  else {
    const r = await rm(o, `/${p}/_apis/release/definitions?searchText=${encodeURIComponent(String(def))}&isExactNameMatch=true&$expand=environments&${V}`);
    d = (r.value ?? [])[0] ?? null;
  }
  const v = d ? { id: d.id as number, envs: new Map<string, number>((d.environments ?? []).map((e: any) => [String(e.name).trim().toLowerCase(), e.id as number])), names: (d.environments ?? []).map((e: any) => String(e.name).trim()) } : null;
  relDefs.set(k, v);
  return v;
}

async function buildDef(o: Org, proj: string, name: string | number): Promise<number | null> {
  if (typeof name === "number" || /^\d+$/.test(String(name))) return Number(name);
  const k = `${o.name}|${proj}|${name}`;
  if (buildDefs.has(k)) return buildDefs.get(k)!;
  const r = await call(o, `/${encodeURIComponent(proj)}/_apis/build/definitions?name=${encodeURIComponent(String(name))}&${V}`);
  const id = (r.value ?? [])[0]?.id ?? null;
  buildDefs.set(k, id);
  return id;
}

function deployStage(label: string, dep: any, defId: number, envId: number): Stage {
  if (!dep) return { label, status: "notDeployed", history: { defId, envId } };
  const pending = /pending|queued|approval/i.test(String(dep.operationStatus ?? "")) && dep.deploymentStatus !== "succeeded" && dep.deploymentStatus !== "failed";
  const art = (dep.release?.artifacts ?? []).find((a: any) => a.isPrimary) ?? (dep.release?.artifacts ?? [])[0];
  return {
    label, status: pending ? "pending" : String(dep.deploymentStatus ?? "notDeployed"),
    when: dep.completedOn ?? dep.startedOn ?? dep.queuedOn, who: dep.requestedFor?.displayName,
    release: dep.release?.name, version: art?.definitionReference?.version?.name, branch: art?.definitionReference?.branch?.name ? branch(art.definitionReference.branch.name) : undefined,
    url: dep.release?._links?.web?.href, history: { defId, envId },
  };
}

async function pollReleases(o: Org): Promise<void> {
  const out: RelView[] = [];
  for (const c of o.releases ?? []) {
    const proj = c.project ?? o.project;
    const p = encodeURIComponent(proj);
    const name = c.name ?? String(c.definition ?? c.pipeline ?? "?");
    const stages: Stage[] = [];
    try {
      if (c.definition !== undefined) {
        const d = await relDef(o, proj, c.definition);
        if (!d) { out.push({ name, stages: [{ label: "?", status: "notDeployed", error: `definicao de release "${c.definition}" nao encontrada` }] }); continue; }
        const wanted = (c.stages?.length ? c.stages : d.names).map(pair);
        const got = await Promise.all(wanted.map(async ([label, env]) => {
          const envId = d.envs.get(env.toLowerCase());
          if (envId === undefined) return { label, status: "notDeployed", error: `ambiente "${env}" nao existe (tem: ${d.names.join(", ")})` } as Stage;
          const r = await rm(o, `/${p}/_apis/release/deployments?definitionId=${d.id}&definitionEnvironmentId=${envId}&$top=1&queryOrder=descending&${V}`);
          return deployStage(label, (r.value ?? [])[0], d.id, envId);
        }));
        stages.push(...got);
      } else if (c.pipeline !== undefined) {
        const id = await buildDef(o, proj, c.pipeline);
        if (!id) { out.push({ name, stages: [{ label: "?", status: "notDeployed", error: `pipeline "${c.pipeline}" nao encontrado` }] }); continue; }
        const got = await Promise.all((c.branches ?? ["main"]).map(pair).map(async ([label, br]) => {
          const ref = br.startsWith("refs/") ? br : `refs/heads/${br}`;
          const r = await call(o, `/${p}/_apis/build/builds?definitions=${id}&branchName=${encodeURIComponent(ref)}&$top=1&queryOrder=queueTimeDescending&${V}`);
          const b: Build | undefined = (r.value ?? [])[0];
          if (!b) return { label, status: "nobuild", history: { buildDef: id, branch: ref } } as Stage;
          return {
            label, status: b.status !== "completed" ? "rodando" : (b.result ?? "notDeployed"), when: b.finishTime ?? b.queueTime,
            version: b.buildNumber, branch: branch(b.sourceBranch), url: b._links.web.href, history: { buildDef: id, branch: ref },
            who: (b as any).requestedFor?.displayName,
          } as Stage;
        }));
        stages.push(...got);
      }
    } catch (e) {
      stages.push({ label: "?", status: "notDeployed", error: String((e as Error).message) });
    }
    // Aviso: deploy (ou build da branch) que terminou desde a ultima volta.
    for (const st of stages) {
      if (st.error || !st.release && !st.version) continue;
      const k = `${o.name}|${name}|${st.label}`;
      const sig = `${st.release ?? st.version}:${st.status}`;
      const before = lastDeploy.get(k);
      lastDeploy.set(k, sig);
      const final = ["succeeded", "failed", "partiallySucceeded", "canceled"].includes(st.status);
      if (before !== undefined && before !== sig && final && o.release_notify !== false) {
        const [lbl, color] = STATUS[st.status] ?? [st.status, "muted"];
        duck.notify({
          title: `Azure · ${name} ${st.label}: ${lbl}`, body: [st.release, st.version, st.branch, st.who].filter(Boolean).join(" · "),
          item: `r|${o.name}|${name}|${st.label}`, actions: [{ id: "web", title: "Abrir" }], color,
        });
      }
    }
    out.push({ name, stages });
  }
  releases.set(o.name, out);
}

function stageItem(o: Org, rel: RelView, st: Stage): Item {
  const [label, color, icon] = STATUS[st.status] ?? [st.status, "muted", "clock"];
  const what = [st.release, st.version && st.branch ? `${st.version} · ${st.branch}` : st.version].filter(Boolean).join(" · ");
  return {
    id: `r|${o.name}|${rel.name}|${st.label}`, title: `${st.label}${what ? `  ${what}` : ""}`, icon: st.error ? "warning" : icon,
    subtitle: st.error ?? [st.when ? ago(st.when) : "", st.who ?? ""].filter(Boolean).join(" · "),
    color: st.error ? "blocked" : color, badge: st.error ? "erro" : label,
    actions: st.url ? [{ id: "web", title: "Abrir", primary: true }, { id: "history", title: "Historico" }] : st.history ? [{ id: "history", title: "Historico", primary: true }] : [],
  };
}

async function showHistory(o: Org, relName: string, st: Stage): Promise<void> {
  const cfg = (o.releases ?? []).find((c) => (c.name ?? String(c.definition ?? c.pipeline)) === relName);
  const p = encodeURIComponent(cfg?.project ?? o.project);
  const h = st.history;
  if (!h) return;
  let lines: string[] = [];
  if ("defId" in h) {
    const r = await rm(o, `/${p}/_apis/release/deployments?definitionId=${h.defId}&definitionEnvironmentId=${h.envId}&$top=15&queryOrder=descending&${V}`);
    lines = (r.value ?? []).map((d: any) => {
      const s = deployStage(st.label, d, h.defId, h.envId);
      return `${(STATUS[s.status] ?? [s.status])[0].padEnd(10)} ${s.release ?? ""}  ${s.version ?? ""} ${s.branch ? `(${s.branch})` : ""}  ${s.when ? ago(s.when) : ""}  ${s.who ?? ""}`;
    });
  } else {
    const r = await call(o, `/${p}/_apis/build/builds?definitions=${h.buildDef}&branchName=${encodeURIComponent(h.branch)}&$top=15&queryOrder=queueTimeDescending&${V}`);
    lines = (r.value ?? []).map((b: Build) => `${(STATUS[b.status !== "completed" ? "rodando" : b.result ?? ""] ?? [b.result ?? b.status])[0].padEnd(10)} ${b.buildNumber}  ${ago(b.finishTime ?? b.queueTime)}`);
  }
  duck.detail({
    title: `${relName} · ${st.label}`, subtitle: "ultimos deploys (mais novo primeiro)", item: `r|${o.name}|${relName}|${st.label}`,
    body: lines.join("\n") || "(nenhum)", actions: st.url ? [{ id: "web", title: "Abrir o ultimo" }] : [],
  });
}

// Lista as definicoes de release e os ambientes, com um trecho pronto para a config.
async function listDefinitions(): Promise<void> {
  const out: string[] = [];
  for (const o of orgs) {
    try {
      const r = await rm(o, `/${encodeURIComponent(o.project)}/_apis/release/definitions?$expand=environments&$top=500&${V}`);
      const defs = (r.value ?? []).sort((a: any, b: any) => String(a.name).localeCompare(String(b.name)));
      out.push(`── ${o.name} · ${o.project} (${defs.length} definicoes) ──`, "");
      for (const d of defs) out.push(`${d.name}`, `    ambientes: ${(d.environments ?? []).map((e: any) => String(e.name).trim()).join(", ")}`);
      const ex = defs[0];
      if (ex) {
        const envs = (ex.environments ?? []).map((e: any) => `'${String(e.name).trim()}'`).slice(0, 3).join(", ");
        out.push("", "Exemplo para o ~/.duck.lua (dentro da org):", `  releases = {`, `    { name = '${ex.name}', definition = '${ex.name}', stages = { ${envs} } },`, `  },`, "");
      }
    } catch (e) { out.push(`${o.name}: ${(e as Error).message}`); }
  }
  duck.detail({ title: "Azure · definicoes de release", subtitle: "copie o nome e os ambientes que quer acompanhar", body: out.join("\n") });
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
  let relFailed = 0;
  for (const o of orgs) {
    const p = multi ? `${o.name} · ` : "";
    const rv = review.get(o.name) ?? [];
    pending += rv.filter((x) => (x.reviewers.find((r) => r.id === o.me?.id)?.vote ?? 0) === 0).length;
    secs.push({ title: `${p}Revisao pedida (${rv.length})`, items: rv.map((x) => prItem(o, x, true)) });
    secs.push({ title: `${p}Meus PRs (${mine.get(o.name)?.length ?? 0})`, items: (mine.get(o.name) ?? []).map((x) => prItem(o, x, false)) });
    for (const rel of releases.get(o.name) ?? []) {
      relFailed += rel.stages.filter((s) => s.status === "failed").length;
      secs.push({ title: `${p}Release · ${rel.name}`, items: rel.stages.map((s) => stageItem(o, rel, s)) });
    }
    secs.push({ title: `${p}Builds recentes`, items: (builds.get(o.name) ?? []).map((b) => buildItem(o, b)) });
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  duck.view("prs", secs, { error: err });
  duck.status("prs", ` ${pending}`, { color: pending ? "accent" : "muted", view: "prs" });
  duck.status("rel", relFailed ? ` ${relFailed} deploy falhou` : "", { color: "blocked", view: "prs" });
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
  if (kind === "r") {
    const rel = (releases.get(name) ?? []).find((x) => x.name === a);
    const st = rel?.stages.find((x) => x.label === b);
    if (!st) return;
    if (action === "web" && st.url) duck.openUrl(st.url);
    else await showHistory(o, a, st);
    return;
  }
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
    duck.register([{ id: "prs", title: "Azure DevOps · PRs, releases e builds", icon: "azure" }],
      [{ id: "prs", title: "Azure DevOps: PRs, releases e builds" }, { id: "release-defs", title: "Azure: definicoes de release (para configurar)" }]);
    if (!settings.orgs?.length) { duck.view("prs", [], { error: "Configure extensions.azure.orgs no ~/.duck.lua" }); return; }
    orgs = await loadOrgs();
    await poll();
    setInterval(() => void poll(), Math.max(60, settings.poll_seconds ?? 120) * 1000);
  },
  async viewOpen() { render(); },
  async command(m) {
    if (m.command === "release-defs") { await listDefinitions(); return; }
    duck.showView(m.command);
  },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = waiting.get(m.id);
    waiting.delete(m.id);
    if (fn && m.value.trim()) await fn(m.value.trim());
  },
});
