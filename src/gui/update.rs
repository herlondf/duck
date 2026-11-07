//! Aviso de versao nova e o item "Atualizar o Duck" da paleta. A rede e a
//! instalacao rodam em threads; o resultado volta pela fila de acoes.

use super::*;
use crate::update;

impl Gui {
    /// Uma consulta ao GitHub logo depois de abrir e depois a cada 6 h (o cache
    /// limita a uma por dia). Desliga com `update_check = false`.
    pub(super) fn update_watch(&self) {
        if !self.cfg.update_check { return; }
        let (q, proxy) = (self.queue.clone(), self.proxy.clone());
        std::thread::spawn(move || loop {
            std::thread::sleep(Duration::from_secs(20));
            if let Some(v) = update::check_cached(Duration::from_secs(24 * 3600)) {
                q.lock().unwrap().push(("update_available".into(), vec![json!(v)]));
                let _ = proxy.send_event(UserEv::Tick);
            }
            std::thread::sleep(Duration::from_secs(6 * 3600));
        });
    }

    pub(super) fn update_builtin(&mut self, name: &str, args: &[Value]) -> bool {
        let s = |i: usize| args.get(i).and_then(|v| v.as_str()).unwrap_or("").to_string();
        match name {
            "update_available" => {
                let v = s(0);
                self.update_ver = Some(v.clone());
                if self.shown_notices.insert(format!("update {v}")) { self.flash(&update::how_to(&v)); }
            }
            "duck_update" => {
                if self.updating { self.flash("Atualizando o Duck…"); return true; }
                if let update::Kind::Source(_) = update::kind() {
                    let v = self.update_ver.clone().unwrap_or_else(|| "nova".into());
                    self.flash(&update::how_to(&v));
                    return true;
                }
                self.updating = true;
                self.flash("Procurando versão nova do Duck…");
                self.peers_bg("duck_update_done", vec![], || {
                    let latest = update::latest()?;
                    if !update::newer(&latest, update::current()) { return Ok(json!({"uptodate": update::current()})); }
                    update::install(&latest)?;
                    Ok(json!({"installed": latest}))
                });
            }
            "duck_update_done" => {
                self.updating = false;
                let r = args.first().cloned().unwrap_or(Value::Null);
                if let Some(e) = r["error"].as_str() { self.flash(&format!("Atualizar o Duck: {e}")); return true; }
                if let Some(v) = r["uptodate"].as_str() { self.update_ver = None; self.flash(&format!("Duck {v} já é a última versão")); return true; }
                let v = r["installed"].as_str().unwrap_or("").to_string();
                self.update_ver = None;
                // No Windows o instalador ja reiniciou o server; aqui, reinicia.
                if !cfg!(windows) { let _ = self.call(json!({"cmd": "restart-server"})); }
                self.flash(&format!("Duck {v} instalado e server reiniciado (os shells continuam). Feche e abra a janela para usar a versão nova."));
            }
            _ => return false,
        }
        true
    }
}
