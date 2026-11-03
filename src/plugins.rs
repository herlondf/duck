//! Plugins WebAssembly: `~/.duck/plugins/*.wasm` (ou `.wat`), em qualquer
//! linguagem que compile para wasm32 (Rust, Zig, C, Go/TinyGo, AssemblyScript).
//!
//! Isolados: nao veem disco, rede nem processos; so falam com o Duck pela
//! ABI abaixo. Cada chamada tem um limite de "combustivel" (instrucoes), entao
//! um plugin em loop infinito falha sozinho em vez de travar a janela.
//!
//! ABI (tudo em JSON UTF-8 na memoria do plugin):
//!   exports  memory
//!            duck_alloc(len: i32) -> i32          reserva len bytes, devolve o ponteiro
//!            duck_on_event(ptr: i32, len: i32)    recebe um evento ({"event": "...", ...})
//!   imports  duck.request(ptr: i32, len: i32)     {"cmd": ...} vai para o server (mesma API
//!                                                  da CLI); {"action": "...", "args": [...]}
//!                                                  roda uma acao da janela (palette, split...)
//!            duck.log(ptr: i32, len: i32)         escreve no server.log
//! Eventos: load, pane-created, pane-closed, agent-state-changed, attention,
//! bell, command-finished, approval.

use wasmi::{Caller, Config, Engine, Extern, Instance, Linker, Module, Store};

const FUEL: u64 = 5_000_000;

pub struct Plugin {
    pub name: String,
    store: Store<Vec<String>>,
    instance: Instance,
}

fn read_mem(caller: &Caller<'_, Vec<String>>, ptr: i32, len: i32) -> Option<String> {
    let mem = caller.get_export("memory").and_then(Extern::into_memory)?;
    let mut buf = vec![0u8; len.clamp(0, 1 << 20) as usize];
    mem.read(caller, ptr as usize, &mut buf).ok()?;
    String::from_utf8(buf).ok()
}

impl Plugin {
    pub fn load(name: &str, bytes: &[u8]) -> Result<Plugin, String> {
        let mut cfg = Config::default();
        cfg.consume_fuel(true);
        let engine = Engine::new(&cfg);
        let module = Module::new(&engine, bytes).map_err(|e| e.to_string())?;
        let mut store = Store::new(&engine, Vec::<String>::new());
        let mut linker = Linker::<Vec<String>>::new(&engine);
        linker.func_wrap("duck", "request", |mut caller: Caller<'_, Vec<String>>, ptr: i32, len: i32| {
            if let Some(s) = read_mem(&caller, ptr, len) { caller.data_mut().push(s); }
        }).map_err(|e| e.to_string())?;
        let plugin = name.to_string();
        linker.func_wrap("duck", "log", move |caller: Caller<'_, Vec<String>>, ptr: i32, len: i32| {
            if let Some(s) = read_mem(&caller, ptr, len) { crate::proto::log(&format!("plugin {plugin}: {s}")); }
        }).map_err(|e| e.to_string())?;
        store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let instance = linker.instantiate_and_start(&mut store, &module).map_err(|e| e.to_string())?;
        Ok(Plugin { name: name.into(), store, instance })
    }

    /// Entrega um evento e devolve os pedidos que o plugin fez durante ele.
    pub fn on_event(&mut self, json: &str) -> Result<Vec<String>, String> {
        self.store.set_fuel(FUEL).map_err(|e| e.to_string())?;
        let alloc = self.instance.get_typed_func::<i32, i32>(&self.store, "duck_alloc").map_err(|e| e.to_string())?;
        let handler = self.instance.get_typed_func::<(i32, i32), ()>(&self.store, "duck_on_event").map_err(|e| e.to_string())?;
        let mem = self.instance.get_memory(&self.store, "memory").ok_or("plugin sem memory exportada")?;
        let bytes = json.as_bytes();
        let ptr = alloc.call(&mut self.store, bytes.len() as i32).map_err(|e| e.to_string())?;
        mem.write(&mut self.store, ptr as usize, bytes).map_err(|e| e.to_string())?;
        handler.call(&mut self.store, (ptr, bytes.len() as i32)).map_err(|e| e.to_string())?;
        Ok(std::mem::take(self.store.data_mut()))
    }
}

/// Carrega todos os plugins da pasta; os que falham voltam como erro (a
/// janela mostra), sem impedir os outros.
pub fn load_all() -> (Vec<Plugin>, Vec<String>) {
    let mut ok = Vec::new();
    let mut errs = Vec::new();
    for e in std::fs::read_dir(crate::config::plugins_dir()).into_iter().flatten().flatten() {
        let p = e.path();
        if !p.extension().is_some_and(|x| x == "wasm" || x == "wat") { continue; }
        let name = p.file_stem().unwrap().to_string_lossy().to_string();
        match std::fs::read(&p).map_err(|e| e.to_string()).and_then(|b| Plugin::load(&name, &b)) {
            Ok(pl) => ok.push(pl),
            Err(e) => errs.push(format!("plugin {name}: {e}")),
        }
    }
    (ok, errs)
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEMO: &str = r#"(module
      (import "duck" "request" (func $req (param i32 i32)))
      (memory (export "memory") 1)
      (data (i32.const 0) "{\22cmd\22:\22rename-tab\22,\22title\22:\22wasm-ok\22}")
      (global $heap (mut i32) (i32.const 1024))
      (func (export "duck_alloc") (param $n i32) (result i32) (local $p i32)
        global.get $heap local.set $p
        global.get $heap local.get $n i32.add global.set $heap
        local.get $p)
      (func (export "duck_on_event") (param i32 i32)
        i32.const 0 i32.const 38 call $req))"#;

    #[test]
    fn plugin_requests() {
        let mut p = Plugin::load("demo", DEMO.as_bytes()).unwrap();
        let reqs = p.on_event(r#"{"event":"load"}"#).unwrap();
        assert_eq!(reqs, vec![r#"{"cmd":"rename-tab","title":"wasm-ok"}"#.to_string()]);
    }

    #[test]
    fn infinite_loop_runs_out_of_fuel() {
        let wat = r#"(module (memory (export "memory") 1)
          (func (export "duck_alloc") (param i32) (result i32) i32.const 0)
          (func (export "duck_on_event") (param i32 i32) (loop br 0)))"#;
        let mut p = Plugin::load("loop", wat.as_bytes()).unwrap();
        assert!(p.on_event("{}").is_err());
    }
}
