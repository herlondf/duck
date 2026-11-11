//! Aviso de versao nova, atualizacao automatica e o item "Atualizar o Duck"
//! da paleta. A rede e a instalacao rodam em threads; o resultado volta pela
//! fila de acoes.
//!
//! Automatica (instalacao do release, `update_auto = true`, o padrao): baixa,
//! confere e instala sem reiniciar; o server so reinicia quando nenhum agente
//! esta trabalhando/pedindo aprovacao e nenhum comando esta rodando. Ocupado:
//! tenta de novo a cada `RETRY`.

use super::*;
use crate::update;

/// Espera entre tentativas quando ha agente trabalhando ou comando rodando.
const RETRY: Duration = Duration::from_secs(120);

impl Gui {
    /// Uma consulta ao GitHub logo depois de abrir e depois a cada 6 h (o cache
    /// limita a uma por dia). Desliga com `update_check = false`; com
    /// `update_auto = false` so avisa.
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
                match update::auto_mode(self.cfg.update_check, self.cfg.update_auto, &update::kind()) {
                    update::Auto::Install if update::should_auto_install(&v, update::current(), update::installed_marker().as_deref()) => self.update_auto(v),
                    // Ja instalada por outra janela ou pelo `duck update`: falta reabrir.
                    update::Auto::Install => { self.update_ver = None; self.notice_once(&format!("update {v}"), &format!("Duck {v} instalado; feche e abra a janela para usar a versão nova")); }
                    _ => self.notice_once(&format!("update {v}"), &update::how_to(&v)),
                }
            }
            // Timers da atualizacao automatica (ver `update_later`).
            "update_auto" => {
                self.update_retry = false;
                let v = s(0);
                if update::auto_mode(self.cfg.update_check, self.cfg.update_auto, &update::kind()) == update::Auto::Install
                    && update::should_auto_install(&v, update::current(), update::installed_marker().as_deref()) { self.update_auto(v); }
            }
            "update_restart" => { self.update_retry = false; self.update_restart(s(0)); }
            // Arg "release": trocar a instalacao pelo codigo pela do release.
            "duck_update" => {
                if self.updating { self.flash("Atualizando o Duck…"); return true; }
                let mode = if s(0) == "release" { update::Mode::Release } else { update::Mode::Auto };
                let kind = update::kind();
                if let (update::Mode::Auto, update::Kind::Source(_)) = (mode, &kind) {
                    let v = self.update_ver.clone().unwrap_or_else(|| "nova".into());
                    self.flash(&update::how_to(&v));
                    return true;
                }
                self.updating = true;
                self.flash(if mode == update::Mode::Release { "Trocando pela instalação do release…" } else { "Procurando versão nova do Duck…" });
                self.peers_bg("duck_update_done", vec![], move || {
                    let latest = update::latest()?;
                    match update::plan(mode, &kind, Some(&latest), update::current(), update::CAN_SWITCH) {
                        update::Plan::UpToDate => Ok(json!({"uptodate": update::current()})),
                        update::Plan::Install { switch } => {
                            update::install(&latest)?;
                            #[cfg(not(windows))]
                            let warn = update::shadow_warning();
                            #[cfg(windows)]
                            let warn: Option<String> = None;
                            Ok(json!({"installed": latest, "switch": switch, "warn": warn}))
                        }
                        update::Plan::Refuse(m) => Err(m),
                        update::Plan::Pull(_) => Err("use duck update --source num terminal".into()),
                    }
                });
            }
            "duck_update_done" => {
                self.updating = false;
                let auto = args.len() > 1 && args[0] == "auto";
                let r = args.last().cloned().unwrap_or(Value::Null);
                if auto {
                    // Falhou (rede, checksum): a proxima consulta tenta de novo.
                    if let Some(e) = r["error"].as_str() { self.flash(&format!("Atualização automática do Duck falhou: {e}")); return true; }
                    self.update_ver = None;
                    self.update_restart(r["installed"].as_str().unwrap_or("").to_string());
                    return true;
                }
                if let Some(e) = r["error"].as_str() { self.flash(&format!("Atualizar o Duck: {e}")); return true; }
                if let Some(v) = r["uptodate"].as_str() { self.update_ver = None; self.flash(&format!("Duck {v} já é a última versão")); return true; }
                let v = r["installed"].as_str().unwrap_or("").to_string();
                self.update_ver = None;
                // No Windows o instalador ja reiniciou o server; aqui, reinicia.
                if !cfg!(windows) { let _ = self.call(json!({"cmd": "restart-server"})); }
                let switch = if r["switch"].as_bool() == Some(true) { " Agora é instalação do release: as próximas atualizações são por aqui." } else { "" };
                let warn = r["warn"].as_str().map(|w| format!(" {w}")).unwrap_or_default();
                self.flash(&format!("Duck {v} instalado e server reiniciado (os shells continuam). Feche e abra a janela para usar a versão nova.{switch}{warn}"));
            }
            _ => return false,
        }
        true
    }

    fn notice_once(&mut self, key: &str, msg: &str) {
        if self.shown_notices.insert(key.to_string()) { self.flash(msg); }
    }

    /// Por que nao reiniciar o server agora (None: quieto).
    fn update_busy(&self) -> Option<String> {
        match self.call(json!({"cmd": "list"})) {
            Ok(l) => update::busy_reason(&l),
            Err(_) => Some("server sem resposta".into()),
        }
    }

    /// Tenta `what` de novo daqui a `RETRY` (um timer por vez).
    fn update_later(&mut self, what: &str, v: String) {
        if self.update_retry { return; }
        self.update_retry = true;
        let (q, proxy, what) = (self.queue.clone(), self.proxy.clone(), what.to_string());
        std::thread::spawn(move || {
            std::thread::sleep(RETRY);
            q.lock().unwrap().push((what, vec![json!(v)]));
            let _ = proxy.send_event(UserEv::Tick);
        });
    }

    /// Baixa, confere e instala `v` em segundo plano, sem reiniciar o server.
    /// So comeca num momento quieto: a instalacao nao mexe nos shells, mas
    /// assim o reinicio logo depois tambem nao pega ninguem no meio.
    fn update_auto(&mut self, v: String) {
        if self.updating { return; }
        if let Some(why) = self.update_busy() {
            self.notice_once(&format!("update {v}"), &format!("Duck {v} disponível: atualiza sozinho quando ficar quieto ({why})"));
            return self.update_later("update_auto", v);
        }
        self.updating = true;
        self.peers_bg("duck_update_done", vec![json!("auto")], move || {
            update::install_no_restart(&v)?;
            Ok(json!({"installed": v}))
        });
    }

    /// Reinicia o server com o binario novo quando ficar quieto; os shells continuam.
    fn update_restart(&mut self, v: String) {
        if let Some(why) = self.update_busy() {
            self.notice_once(&format!("update-restart {v}"), &format!("Duck {v} baixado; o server reinicia quando ficar quieto ({why})"));
            return self.update_later("update_restart", v);
        }
        let _ = self.call(json!({"cmd": "restart-server"}));
        self.flash(&format!("Duck atualizado para v{} (os shells continuam). Feche e abra a janela para a interface nova.", v.trim_start_matches('v')));
    }
}
