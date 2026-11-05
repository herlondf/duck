//! duckw.exe: a janela, sem console no Windows. Ver src/lib.rs.
#![cfg_attr(windows, windows_subsystem = "windows")]
fn main() { duck::entry(true) }
