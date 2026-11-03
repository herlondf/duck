// Hospedeiro das extensoes em Node do Duck: um processo so para todas, em vez
// de um Node (~60 MB) por extensao. O Duck abre:
//   node host.js '[{"name":"github","dir":"...","main":"dist/index.js"}, ...]'
// e troca JSON por linha como sempre, com o campo "ext" dizendo de qual
// extensao e a mensagem. O SDK (duck.ts) percebe o hospedeiro sozinho: a
// extensao e a mesma que roda em processo proprio.

"use strict";
const readline = require("node:readline");
const path = require("node:path");

const exts = new Map(); // nome -> { dispatch, queue }

function out(name, msg) {
  process.stdout.write(JSON.stringify({ ...msg, ext: name }) + "\n");
}

globalThis.__duckHost = {
  current: null,
  send: out,
  register(name, dispatch) {
    const e = exts.get(name);
    if (!e) return;
    e.dispatch = dispatch;
    for (const m of e.queue.splice(0)) dispatch(m);
  },
  log(name, args) { console.error(new Date().toISOString(), `[${name}]`, ...args); },
};

// Um erro solto numa extensao nao derruba as outras.
process.on("uncaughtException", (e) => console.error(new Date().toISOString(), "[host] erro nao tratado:", e));
process.on("unhandledRejection", (e) => console.error(new Date().toISOString(), "[host] promessa rejeitada:", e));

let list = [];
try { list = JSON.parse(process.argv[2] || "[]"); } catch (e) { console.error("[host] lista invalida", e); }
for (const x of list) {
  exts.set(x.name, { dispatch: null, queue: [] });
  globalThis.__duckHost.current = x.name;
  try { require(path.join(x.dir, x.main)); }
  catch (e) {
    console.error(new Date().toISOString(), `[${x.name}] nao carregou:`, e);
    out(x.name, { type: "view", id: "__error", sections: [], error: `nao carregou: ${e && e.message}` });
  }
  globalThis.__duckHost.current = null;
}

const rl = readline.createInterface({ input: process.stdin });
rl.on("line", (line) => {
  let m;
  try { m = JSON.parse(line); } catch { return; }
  const e = exts.get(m.ext);
  if (!e) return;
  if (e.dispatch) e.dispatch(m); else e.queue.push(m);
});
rl.on("close", () => process.exit(0));
