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
//         -- Todas as releases classicas com deploy recente, uma linha por projeto. O ambiente de
//         -- cada branch sai do filtro de branch do proprio ambiente (ou do nome: Dev, Tst, Prd...).
//         -- Filtros opcionais: path (pasta), match / exclude (regex no nome), days (padrao 30).
//         { auto = true, branches = { 'develop', 'tst', 'master' }, path = 'backend' },
//         -- Uma definicao so: o ultimo deploy de cada ambiente ("rotulo=Ambiente" troca o nome
//         -- mostrado; "Prd|Prod" aceita qualquer um dos dois). branches tambem vale aqui.
//         { name = 'API', definition = 'api-cd', stages = { 'develop=Dev', 'tst=Tst', 'master=Prd|Prod' } },
//         -- Pipeline YAML (ou CI): o ultimo build de cada branch.
//         { name = 'Portal', pipeline = 'portal-ci', branches = { 'develop', 'tst', 'master' } },
//       } },
//   } } }
// Token (PAT com Code Read, Build Read e Release Read): echo <pat> | duck secret set azure/<name>
// Paleta -> "Azure: definicoes de release" lista as definicoes e ambientes para copiar.

import { duck, run, Item, Section, Action } from "./duck";

type RelCfg = {
  name?: string; project?: string; definition?: string | number; stages?: string[]; pipeline?: string | number; branches?: string[];
  auto?: boolean; path?: string; match?: string; exclude?: string; days?: number; notify?: boolean;
};
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
type RelView = { name: string; stages: Stage[]; project: string; group?: string; url?: string; notify?: boolean };
const releases = new Map<string, RelView[]>();     // org -> projetos
const relDefs = new Map<string, { id: number; envs: Map<string, number>; names: string[] } | null>();   // "org|proj|def" -> definicao
const buildDefs = new Map<string, number | null>();  // "org|proj|pipeline" -> id
const lastDeploy = new Map<string, string>();      // "org|proj|label" -> "id:status" (para avisar)
// Definicao completa (ambientes e o branch que cada um publica), relida a cada hora.
type DefEnv = { id: number; name: string; rank: number; branches: string[] };
type DefInfo = { id: number; name: string; path: string; envs: DefEnv[]; url?: string; at: number };
const defInfo = new Map<string, DefInfo>();         // "org|proj|id"
// Deploys do projeto inteiro (descoberta automatica): uma consulta por volta.
type ProjDeps = { deps: Map<string, any>; defs: Map<number, { name: string; path: string }>; tried: Set<string>; since: number; full: number; days: number };
const projDeps = new Map<string, ProjDeps>();       // "org|proj"

const V = "api-version=7.1";
function root(o: Org) { return o.url.replace(/\/$/, ""); }

async function call(o: Org, path: string, init: RequestInit = {}, hdr?: (h: Headers) => void): Promise<any> {
  const headers: Record<string, string> = { Authorization: o.auth, Accept: "application/json" };
  if (init.body) headers["Content-Type"] = "application/json";
  let r: Response;
  try { r = await fetch(root(o) + path, { ...init, headers, redirect: "manual" }); }
  catch { throw new Error(`sem acesso a ${root(o)}`); }
  // PAT vencido: o Azure responde 203/302 com a pagina de login em vez de 401.
  if (r.status === 203 || (r.status >= 300 && r.status < 400)) throw new Error("PAT vencido ou sem permissao (gere outro e grave com duck secret set)");
  const text = await r.text();
  hdr?.(r.headers);
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
async function rm(o: Org, path: string, hdr?: (h: Headers) => void): Promise<any> { return call({ ...o, url: vsrm(o) }, path, {}, hdr); }
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

// Nomes que um ambiente costuma ter para cada branch (quando o ambiente nao filtra por branch).
const GROUPS = [
  ["develop", "dev", "development", "desenv", "desenvolvimento"],
  ["tst", "test", "teste", "qa", "hml", "homolog", "homologacao"],
  ["stg", "stage", "staging"],
  ["master", "main", "prd", "prod", "production", "producao"],
];
const aliases = (x: string) => { const k = x.trim().toLowerCase(); return GROUPS.find((g) => g.includes(k)) ?? [k]; };

// Branches que o ambiente publica: filtro "sourceBranch" do gatilho de artefato.
function envBranches(e: any): string[] {
  const out: string[] = [];
  for (const c of e.conditions ?? []) {
    if (!c?.value) continue;
    try { const v = JSON.parse(c.value); if (v?.sourceBranch) out.push(branch(String(v.sourceBranch)).toLowerCase()); } catch { /* condicao de evento */ }
  }
  return out;
}

async function defFull(o: Org, proj: string, id: number): Promise<DefInfo> {
  const k = `${o.name}|${proj}|${id}`;
  const c = defInfo.get(k);
  if (c && Date.now() - c.at < 3600_000) return c;
  const d = await rm(o, `/${encodeURIComponent(proj)}/_apis/release/definitions/${id}?${V}`);
  const v: DefInfo = {
    id, name: String(d.name), path: String(d.path ?? "\\"), url: d._links?.web?.href, at: Date.now(),
    envs: (d.environments ?? []).map((e: any) => ({ id: e.id, name: String(e.name).trim(), rank: e.rank ?? 0, branches: envBranches(e) })),
  };
  defInfo.set(k, v);
  return v;
}

// Ambiente que publica o branch: pelo filtro de branch; empate (Prd e Prd - Cliente) ou
// sem filtro, pelo nome (develop -> Dev, master -> Prd/Prod...).
function envForBranch(d: DefInfo, br: string): DefEnv | undefined {
  const b = branch(br).toLowerCase();
  const al = aliases(b);
  const byName = (l: DefEnv[]) => l.find((e) => al.includes(e.name.toLowerCase()));
  const hit = d.envs.filter((e) => e.branches.includes(b)).sort((x, y) => x.rank - y.rank);
  return hit.length ? byName(hit) ?? hit[0] : byName(d.envs);
}

// "Prd|Prod" -> o primeiro que existir; sem nenhum, tenta como branch.
function envNamed(d: DefInfo, spec: string): DefEnv | undefined {
  for (const n of spec.split("|").map((x) => x.trim().toLowerCase()).filter(Boolean)) {
    const e = d.envs.find((x) => x.name.toLowerCase() === n);
    if (e) return e;
  }
  return envForBranch(d, spec.split("|")[0]);
}

// [rotulo, ambiente] de cada coluna pedida na config.
function wantedEnvs(d: DefInfo, c: RelCfg): [string, DefEnv | undefined, string][] {
  if (c.stages?.length) return c.stages.map(pair).map(([l, spec]) => [l, envNamed(d, spec), spec]);
  if (c.branches?.length) return c.branches.map(pair).map(([l, br]) => [l, envForBranch(d, br), br]);
  return [...d.envs].sort((x, y) => x.rank - y.rank).map((e) => [e.name, e, e.name]);
}

const iso = (ms: number) => new Date(ms).toISOString().replace(/\.\d+Z$/, "Z");

// Deploys de todas as definicoes do projeto: janela inteira a cada 30 min (descobre
// definicao nova), no meio so o que mudou desde a ultima volta.
async function syncDeployments(o: Org, proj: string, days: number): Promise<ProjDeps> {
  const k = `${o.name}|${proj}`;
  const now = Date.now();
  let s = projDeps.get(k);
  const full = !s || now - s.full > 30 * 60_000 || s.days < days;
  if (!s || full) s = { deps: new Map(), defs: new Map(), tried: new Set(), since: now, full: now, days };
  const when = full ? `minStartedTime=${iso(now - days * 86400_000)}` : `minModifiedTime=${iso(s.since - 5 * 60_000)}`;
  let cont: string | undefined;
  let pages = 0;
  do {
    const tok = cont;
    cont = undefined;
    const r = await rm(o, `/${encodeURIComponent(proj)}/_apis/release/deployments?${when}&latestAttemptsOnly=true&queryOrder=descending&$top=100${tok ? `&continuationToken=${tok}` : ""}&${V}`,
      (h) => { cont = h.get("x-ms-continuationtoken") ?? undefined; });
    for (const d of r?.value ?? []) {
      const def = d.releaseDefinition;
      if (!def) continue;
      const key = `${def.id}|${d.definitionEnvironmentId}`;
      const old = s.deps.get(key);
      if (!old || d.id >= old.id) s.deps.set(key, d);
      if (!s.defs.has(def.id)) s.defs.set(def.id, { name: String(def.name), path: String(def.path ?? "") });
    }
  } while (cont && ++pages < 20);
  s.since = now;
  projDeps.set(k, s);
  return s;
}

const normPath = (x: string) => ("\\" + x.replace(/\//g, "\\").replace(/^\\+/, "")).toLowerCase();

// Modo automatico: uma linha por definicao com deploy recente num dos branches pedidos.
async function pollAuto(o: Org, c: RelCfg): Promise<RelView[]> {
  const proj = c.project ?? o.project;
  const s = await syncDeployments(o, proj, Math.max(1, c.days ?? 30));
  const re = (x?: string) => { try { return x ? new RegExp(x, "i") : null; } catch { return null; } };
  const m = re(c.match), ex = re(c.exclude);
  const ids = [...s.defs.entries()]
    .filter(([, d]) => (!c.path || normPath(d.path).startsWith(normPath(c.path))) && (!m || m.test(d.name)) && !(ex && ex.test(d.name)))
    .map(([id]) => id);
  const defs: DefInfo[] = [];
  for (let i = 0; i < ids.length; i += 8) {
    defs.push(...(await Promise.all(ids.slice(i, i + 8).map((id) => defFull(o, proj, id).catch(() => null)))).filter((x): x is DefInfo => !!x));
  }
  // Ambiente sem deploy na janela: busca o ultimo dele (uma vez a cada descoberta).
  const missing = defs.flatMap((d) => wantedEnvs(d, c).filter(([, e]) => e).map(([, e]) => `${d.id}|${e!.id}`))
    .filter((k) => !s.deps.has(k) && !s.tried.has(k));
  for (let i = 0; i < missing.length; i += 8) {
    await Promise.all(missing.slice(i, i + 8).map(async (k) => {
      s.tried.add(k);
      const [def, env] = k.split("|");
      try {
        const r = await rm(o, `/${encodeURIComponent(proj)}/_apis/release/deployments?definitionId=${def}&definitionEnvironmentId=${env}&$top=1&queryOrder=descending&${V}`);
        if (r?.value?.[0]) s.deps.set(k, r.value[0]);
      } catch { /* fica sem deploy */ }
    }));
  }
  const out: RelView[] = [];
  for (const d of defs) {
    const stages = wantedEnvs(d, c).filter(([, e]) => e).map(([l, e]) => deployStage(l, s.deps.get(`${d.id}|${e!.id}`), d.id, e!.id));
    if (!stages.some((x) => x.release)) continue;
    out.push({ name: d.name, stages, project: proj, group: c.name ?? "Releases", url: d.url, notify: c.notify });
  }
  const last = (r: RelView) => Math.max(0, ...r.stages.map((x) => (x.when ? Date.parse(x.when) : 0)));
  // Falhou ou esperando aprovacao primeiro; depois o deploy mais recente.
  const bad = (r: RelView) => (r.stages.some((x) => ["failed", "partiallySucceeded", "pending", "inProgress"].includes(x.status)) ? 1 : 0);
  return out.sort((a, b) => bad(b) - bad(a) || last(b) - last(a));
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
  const views: RelView[] = [];
  for (const c of o.releases ?? []) {
    const proj = c.project ?? o.project;
    if (c.auto) {
      try { views.push(...await pollAuto(o, c)); }
      catch (e) { views.push({ name: c.name ?? "Releases", project: proj, stages: [{ label: "?", status: "notDeployed", error: String((e as Error).message) }] }); }
      continue;
    }
    const p = encodeURIComponent(proj);
    const name = c.name ?? String(c.definition ?? c.pipeline ?? "?");
    const stages: Stage[] = [];
    try {
      if (c.definition !== undefined) {
        const d0 = await relDef(o, proj, c.definition);
        if (!d0) { views.push({ name, project: proj, stages: [{ label: "?", status: "notDeployed", error: `definicao de release "${c.definition}" nao encontrada` }] }); continue; }
        const d = await defFull(o, proj, d0.id);
        const got = await Promise.all(wantedEnvs(d, c).map(async ([label, env, spec]) => {
          if (!env) return { label, status: "notDeployed", error: `ambiente para "${spec}" nao existe (tem: ${d.envs.map((e) => e.name).join(", ")})` } as Stage;
          const r = await rm(o, `/${p}/_apis/release/deployments?definitionId=${d.id}&definitionEnvironmentId=${env.id}&$top=1&queryOrder=descending&${V}`);
          return deployStage(label, (r.value ?? [])[0], d.id, env.id);
        }));
        stages.push(...got);
      } else if (c.pipeline !== undefined) {
        const id = await buildDef(o, proj, c.pipeline);
        if (!id) { views.push({ name, project: proj, stages: [{ label: "?", status: "notDeployed", error: `pipeline "${c.pipeline}" nao encontrado` }] }); continue; }
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
    views.push({ name, stages, project: proj, notify: c.notify });
  }
  // Aviso: deploy (ou build da branch) que terminou desde a ultima volta.
  for (const { name, stages, notify } of views) {
    for (const st of stages) {
      if (st.error || !st.release && !st.version) continue;
      const k = `${o.name}|${name}|${st.label}`;
      const sig = `${st.release ?? st.version}:${st.status}`;
      const before = lastDeploy.get(k);
      lastDeploy.set(k, sig);
      const final = ["succeeded", "failed", "partiallySucceeded", "canceled"].includes(st.status);
      if (before !== undefined && before !== sig && final && o.release_notify !== false && notify !== false) {
        const [lbl, color] = STATUS[st.status] ?? [st.status, "muted"];
        duck.notify({
          title: `Azure · ${name} ${st.label}: ${lbl}`, body: [st.release, st.version, st.branch, st.who].filter(Boolean).join(" · "),
          item: `r|${o.name}|${name}|${st.label}`, actions: [{ id: "web", title: "Abrir" }], color,
        });
      }
    }
  }
  releases.set(o.name, views);
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

const MARK: Record<string, string> = {
  succeeded: "✓", failed: "✗", partiallySucceeded: "!", canceled: "–", inProgress: "…", pending: "…", rodando: "…", notDeployed: "–", nobuild: "–",
};
const SEVERITY = ["failed", "partiallySucceeded", "pending", "inProgress", "canceled", "succeeded", "notDeployed"];

// Uma linha por projeto: "develop ✓ #1482 · tst ✓ #1483 · master ✗ #1484".
function relRowItem(o: Org, rel: RelView): Item {
  const worst = rel.stages.map((x) => x.status).sort((a, b) => (SEVERITY.indexOf(a) + 99) % 99 - (SEVERITY.indexOf(b) + 99) % 99)[0] ?? "notDeployed";
  const [lbl, color, icon] = STATUS[worst] ?? [worst, "muted", "clock"];
  const bad = rel.stages.filter((x) => x.status === worst).map((x) => x.label).join(", ");
  const newest = rel.stages.filter((x) => x.when).sort((a, b) => Date.parse(b.when!) - Date.parse(a.when!))[0];
  const cols = rel.stages.map((x) => `${x.label} ${MARK[x.status] ?? "?"} ${x.release ? x.release.replace(/^Release-/i, "#") : ""}`.trim()).join("  ·  ");
  return {
    id: `r|${o.name}|${rel.name}|*`, title: rel.name, icon: worst === "succeeded" ? "check" : icon,
    subtitle: `${cols}${newest?.when ? `  ·  ${ago(newest.when)}` : ""}`,
    color: worst === "succeeded" ? "muted" : color, badge: worst === "succeeded" ? "ok" : `${bad} ${lbl}`,
    actions: [{ id: "detail", title: "Detalhes", primary: true }, ...(newest?.url ? [{ id: "web", title: "Abrir o ultimo" }] : [])],
  };
}

// Detalhe do projeto: cada ambiente com os ultimos deploys.
async function showRelDetail(o: Org, rel: RelView): Promise<void> {
  const p = encodeURIComponent(rel.project);
  const parts = await Promise.all(rel.stages.map(async (st) => {
    const h = st.history;
    if (!h || !("defId" in h)) return [`── ${st.label} ──`, st.error ?? "(sem historico)"];
    const r = await rm(o, `/${p}/_apis/release/deployments?definitionId=${h.defId}&definitionEnvironmentId=${h.envId}&$top=5&queryOrder=descending&${V}`);
    const env = (r.value ?? [])[0]?.releaseEnvironment?.name;
    const lines = (r.value ?? []).map((d: any) => {
      const s = deployStage(st.label, d, h.defId, h.envId);
      return `  ${(STATUS[s.status] ?? [s.status])[0].padEnd(10)} ${s.release ?? ""}  ${s.version ?? ""}${s.branch ? ` (${s.branch})` : ""}  ${s.when ? ago(s.when) : ""}  ${s.who ?? ""}`;
    });
    return [`── ${st.label}${env ? ` → ${env}` : ""} ──`, ...(lines.length ? lines : ["  (nenhum deploy)"])];
  }));
  duck.detail({
    title: rel.name, subtitle: `${rel.project} · ultimos deploys de cada ambiente (mais novo primeiro)`, item: `r|${o.name}|${rel.name}|*`,
    body: parts.map((x) => x.join("\n")).join("\n\n"), actions: rel.url ? [{ id: "defweb", title: "Abrir a definicao" }] : [],
  });
}

async function showHistory(o: Org, relName: string, st: Stage): Promise<void> {
  const rel = (releases.get(o.name) ?? []).find((x) => x.name === relName);
  const p = encodeURIComponent(rel?.project ?? o.project);
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
        out.push("", "Exemplo para o ~/.duck.lua (dentro da org):", `  releases = {`,
          `    -- todas com deploy nos ultimos 30 dias (filtros: path, match, exclude, days)`,
          `    { auto = true, branches = { 'develop', 'tst', 'master' } },`,
          `    -- ou uma so`,
          `    { name = '${ex.name}', definition = '${ex.name}', stages = { ${envs} } },`, `  },`, "");
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
    ts: Date.parse(p.creationDate),
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

const VIEWS = [
  { id: "review", label: "Revisao", icon: "azure" },
  { id: "mine", label: "Meus PRs", icon: "pr" },
  { id: "releases", label: "Releases", icon: "tag" },
  { id: "builds", label: "Builds recentes", icon: "check" },
];

function render(): void {
  const multi = orgs.length > 1;
  // Uma visao por assunto: o painel mostra as quatro como abas no topo.
  const tabs: Record<string, { secs: Section[]; n: number }> = {
    review: { secs: [], n: 0 }, mine: { secs: [], n: 0 }, releases: { secs: [], n: 0 }, builds: { secs: [], n: 0 },
  };
  let pending = 0;
  let relFailed = 0;
  for (const o of orgs) {
    const p = multi ? `${o.name} · ` : "";
    const rv = review.get(o.name) ?? [];
    const my = mine.get(o.name) ?? [];
    const bl = builds.get(o.name) ?? [];
    pending += rv.filter((x) => (x.reviewers.find((r) => r.id === o.me?.id)?.vote ?? 0) === 0).length;
    tabs.review.n += rv.length;
    tabs.mine.n += my.length;
    tabs.builds.n += bl.length;
    tabs.review.secs.push({ title: multi ? o.name : "", items: rv.map((x) => prItem(o, x, true)) });
    tabs.mine.secs.push({ title: multi ? o.name : "", items: my.map((x) => prItem(o, x, false)) });
    tabs.builds.secs.push({ title: multi ? o.name : "", items: bl.map((b) => buildItem(o, b)) });
    const secs = tabs.releases.secs;
    const groups = new Map<string, Item[]>();
    for (const rel of releases.get(o.name) ?? []) {
      relFailed += rel.stages.filter((s) => s.status === "failed").length;
      if (rel.group) {
        if (!groups.has(rel.group)) { groups.set(rel.group, []); secs.push({ title: multi || rel.group !== "Releases" ? `${p}${rel.group}` : "", items: groups.get(rel.group)! }); }
        groups.get(rel.group)!.push(rel.stages.some((x) => x.error) ? stageItem(o, rel, rel.stages[0]) : relRowItem(o, rel));
      } else secs.push({ title: `${p}Release · ${rel.name}`, items: rel.stages.map((s) => stageItem(o, rel, s)) });
    }
    for (const sec of secs) if (sec.title && sec.items.length && [...groups.values()].includes(sec.items)) sec.title = `${sec.title} (${sec.items.length})`;
    tabs.releases.n += (releases.get(o.name) ?? []).length;
  }
  const err = [...errors.entries()].map(([n, e]) => `${n}: ${e}`).join(" | ");
  const empty = (secs: Section[]) => (secs.every((x) => !x.items.length) ? [] : secs);
  for (const v of VIEWS) {
    const t = tabs[v.id];
    duck.view(v.id, v.id === "releases" ? t.secs : empty(t.secs), { error: err, title: `Azure · ${v.label}${t.n ? ` (${t.n})` : ""}` });
  }
  duck.status("prs", ` ${pending}`, { color: pending ? "accent" : "muted", view: "review" });
  duck.status("rel", relFailed ? ` ${relFailed} deploy falhou` : "", { color: "blocked", view: "releases" });
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
    if (rel && b === "*") {
      const newest = rel.stages.filter((x) => x.when && x.url).sort((p, q) => Date.parse(q.when!) - Date.parse(p.when!))[0];
      if (action === "web" && newest?.url) duck.openUrl(newest.url);
      else if (action === "defweb" && rel.url) duck.openUrl(rel.url);
      else await showRelDetail(o, rel);
      return;
    }
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
    duck.register(VIEWS.map((v) => ({ id: v.id, title: `Azure · ${v.label}`, icon: v.icon })), [
      ...VIEWS.map((v) => ({ id: v.id, title: `Azure: ${v.label.toLowerCase()}` })),
      { id: "release-defs", title: "Azure: definicoes de release (para configurar)" },
    ]);
    if (!settings.orgs?.length) { for (const v of VIEWS) duck.view(v.id, [], { error: "Configure extensions.azure.orgs no ~/.duck.lua" }); return; }
    orgs = await loadOrgs();
    await poll();
    setInterval(() => void poll(), Math.max(60, settings.poll_seconds ?? 120) * 1000);
  },
  async viewOpen() { render(); },
  async command(m) {
    if (m.command === "release-defs") { await listDefinitions(); return; }
    duck.showView(m.command === "prs" ? "review" : m.command);   // "prs": nome antigo
  },
  async action(m) { await onAction(m.action, m.item); },
  async input(m) {
    const fn = waiting.get(m.id);
    waiting.delete(m.id);
    if (fn && m.value.trim()) await fn(m.value.trim());
  },
});
