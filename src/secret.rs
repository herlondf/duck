//! Segredos das extensoes (tokens), fora de qualquer arquivo de config.
//! Windows: Gerenciador de Credenciais (credencial generica "duck:<nome>",
//! por usuario). Linux/macOS: ~/.duck/secrets/<nome> com permissao 600.
//! ponytail: no Linux/macOS um keyring (secret-tool / Keychain) e o proximo passo.

pub fn target(name: &str) -> String { format!("duck:{name}") }

#[cfg(windows)]
mod imp {
    use windows_sys::Win32::Security::Credentials::{CredDeleteW, CredFree, CredReadW, CredWriteW, CREDENTIALW, CRED_PERSIST_LOCAL_MACHINE, CRED_TYPE_GENERIC};
    fn wide(s: &str) -> Vec<u16> { s.encode_utf16().chain([0]).collect() }

    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let t = wide(&super::target(name));
        let user = wide("duck");
        let blob = value.as_bytes();
        let mut c: CREDENTIALW = unsafe { std::mem::zeroed() };
        c.Type = CRED_TYPE_GENERIC;
        c.TargetName = t.as_ptr() as *mut u16;
        c.UserName = user.as_ptr() as *mut u16;
        c.CredentialBlobSize = blob.len() as u32;
        c.CredentialBlob = blob.as_ptr() as *mut u8;
        c.Persist = CRED_PERSIST_LOCAL_MACHINE;
        if unsafe { CredWriteW(&c, 0) } == 0 { return Err(std::io::Error::last_os_error().to_string()); }
        Ok(())
    }

    pub fn get(name: &str) -> Option<String> {
        let t = wide(&super::target(name));
        let mut p: *mut CREDENTIALW = std::ptr::null_mut();
        if unsafe { CredReadW(t.as_ptr(), CRED_TYPE_GENERIC, 0, &mut p) } == 0 { return None; }
        let v = unsafe {
            let c = &*p;
            let bytes = std::slice::from_raw_parts(c.CredentialBlob, c.CredentialBlobSize as usize);
            let s = String::from_utf8_lossy(bytes).to_string();
            CredFree(p as *const _);
            s
        };
        Some(v)
    }

    pub fn delete(name: &str) -> bool {
        let t = wide(&super::target(name));
        unsafe { CredDeleteW(t.as_ptr(), CRED_TYPE_GENERIC, 0) != 0 }
    }
}

#[cfg(not(windows))]
mod imp {
    fn path(name: &str) -> std::path::PathBuf {
        crate::config::home().join(".duck").join("secrets").join(name.replace(['/', '\\'], "__"))
    }
    pub fn set(name: &str, value: &str) -> Result<(), String> {
        let p = path(name);
        std::fs::create_dir_all(p.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(&p, value).map_err(|e| e.to_string())?;
        #[cfg(unix)]
        { use std::os::unix::fs::PermissionsExt; let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600)); }
        Ok(())
    }
    pub fn get(name: &str) -> Option<String> { std::fs::read_to_string(path(name)).ok() }
    pub fn delete(name: &str) -> bool { std::fs::remove_file(path(name)).is_ok() }
}

pub use imp::{delete, get, set};

#[cfg(test)]
mod tests {
    #[test]
    fn roundtrip() {
        let n = format!("teste/{}", std::process::id());
        super::set(&n, "valor-secreto ç").unwrap();
        assert_eq!(super::get(&n).as_deref(), Some("valor-secreto ç"));
        assert!(super::delete(&n));
        assert_eq!(super::get(&n), None);
    }
}
