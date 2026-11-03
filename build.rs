// Icone do Duck dentro do .exe (Explorer, atalhos, barra de tarefas).
fn main() {
    println!("cargo:rerun-if-changed=assets/duck.rc");
    println!("cargo:rerun-if-changed=assets/duck.ico");
    #[cfg(windows)]
    { let _ = embed_resource::compile("assets/duck.rc", embed_resource::NONE); }
}
