//! Configuración y biblioteca de Dino 2.0.
//!
//! Dos piezas:
//! - [`LibraryExt`]: las reglas de la biblioteca, puras y probadas. Son las de 1.x
//!   (`src-tauri/src/library.rs`), con sus mismos casos de prueba, sobre el esquema nuevo;
//! - [`Store`]: leer y guardar `config.json` y `library.json`. Cada cambio es
//!   «leer → modificar → guardar de forma atómica» bajo un cerrojo, así que dos comandos de
//!   la interfaz a la vez no se pisan el cambio el uno al otro.

use dino_core::schema::{self, Config, Library, LibraryEntry, PackSource, Paths, StoreError};
use std::sync::Mutex;

// ---------------------------------------------------------------- reglas

pub trait LibraryExt {
    fn find(&self, id: &str) -> Option<&LibraryEntry>;
    /// Con cuál se juega. Un `selected` que apunta a algo que ya no está **no vacía la
    /// biblioteca**: se cae al primero.
    fn selected_entry(&self) -> Option<&LibraryEntry>;
    /// Selecciona si existe. Devuelve si lo hizo.
    fn select(&mut self, id: &str) -> bool;
    /// Mete o actualiza **sin moverla de sitio**: reinstalar no reordena la biblioteca
    /// delante de quien la está mirando. No toca la selección.
    fn upsert(&mut self, entry: LibraryEntry);
    /// La saca y devuelve lo que había. Si era la seleccionada, pasa a la de al lado.
    fn remove(&mut self, id: &str) -> Option<LibraryEntry>;
    /// Qué carpeta le toca a un pack que entra (ver la implementación).
    fn free_id(&self, id: &str, code: Option<&str>) -> String;
}

fn code_of(e: &LibraryEntry) -> Option<&str> {
    match &e.source {
        PackSource::Catalog { code } => Some(code.as_str()),
        PackSource::File => None,
    }
}

impl LibraryExt for Library {
    fn find(&self, id: &str) -> Option<&LibraryEntry> {
        self.instances.iter().find(|e| e.id == id)
    }

    fn selected_entry(&self) -> Option<&LibraryEntry> {
        self.selected.as_deref().and_then(|id| self.find(id)).or_else(|| self.instances.first())
    }

    fn select(&mut self, id: &str) -> bool {
        if self.find(id).is_some() {
            self.selected = Some(id.to_string());
            return true;
        }
        false
    }

    fn upsert(&mut self, entry: LibraryEntry) {
        match self.instances.iter_mut().find(|e| e.id == entry.id) {
            Some(slot) => *slot = entry,
            None => self.instances.push(entry),
        }
    }

    fn remove(&mut self, id: &str) -> Option<LibraryEntry> {
        let i = self.instances.iter().position(|e| e.id == id)?;
        let out = self.instances.remove(i);
        if self.selected.as_deref() == Some(id) {
            // La de al lado (la que ocupa ahora ese sitio); si era la última, la anterior.
            self.selected = self.instances.get(i).or_else(|| self.instances.last()).map(|e| e.id.clone());
        }
        Some(out)
    }

    /// Dos packs distintos con el mismo id se instalarían uno encima del otro, y ahí están
    /// los mundos del primero. Solo es conflicto cuando **los dos traen código y no es el
    /// mismo**. Un pack ya conocido por su código vuelve a su carpeta aunque el catálogo le
    /// haya cambiado el id. Un `.mrpack` sin código se reinstala sobre el suyo.
    fn free_id(&self, id: &str, code: Option<&str>) -> String {
        if let Some(c) = code.filter(|c| !c.is_empty()) {
            if let Some(e) = self.instances.iter().find(|e| code_of(e) == Some(c)) {
                return e.id.clone();
            }
        }
        let clash =
            self.instances.iter().any(|e| e.id == id && matches!((code_of(e), code), (Some(a), Some(b)) if a != b));
        if !clash {
            return id.to_string();
        }
        (2..).map(|n| format!("{id}-{n}")).find(|c| self.find(c).is_none()).unwrap_or_else(|| id.to_string())
    }
}

// ---------------------------------------------------------------- almacén

pub struct Store {
    paths: Paths,
    lock: Mutex<()>,
}

impl Store {
    pub fn new(paths: Paths) -> Self {
        Self { paths, lock: Mutex::new(()) }
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// La configuración; la de fábrica si todavía no hay archivo. Un archivo dañado es
    /// un error, no la de fábrica: confundirlos borraría la buena al guardar.
    pub fn config(&self) -> Result<Config, StoreError> {
        Ok(schema::load(&self.paths.config())?.unwrap_or_default())
    }

    pub fn library(&self) -> Result<Library, StoreError> {
        Ok(schema::load(&self.paths.library())?.unwrap_or_default())
    }

    /// Cambia la configuración y la guarda. Devuelve la que quedó.
    pub fn update_config(&self, f: impl FnOnce(&mut Config)) -> Result<Config, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut c = self.config()?;
        f(&mut c);
        schema::save(&self.paths.config(), &c, schema::CONFIG_SCHEMA)?;
        Ok(c)
    }

    /// Cambia la biblioteca y la guarda. `f` devuelve lo que quiera el que llama.
    pub fn update_library<T>(&self, f: impl FnOnce(&mut Library) -> T) -> Result<T, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut lib = self.library()?;
        let out = f(&mut lib);
        schema::save(&self.paths.library(), &lib, schema::LIBRARY_SCHEMA)?;
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(id: &str, code: Option<&str>) -> LibraryEntry {
        LibraryEntry {
            id: id.into(),
            name: id.into(),
            source: code.map_or(PackSource::File, |c| PackSource::Catalog { code: c.into() }),
            pack_version: "1.0.0".into(),
            server: None,
            last_played: None,
        }
    }

    fn lib(ids: &[(&str, Option<&str>)]) -> Library {
        let mut l = Library::default();
        for (id, code) in ids {
            l.instances.push(e(id, *code));
        }
        l.selected = l.instances.first().map(|e| e.id.clone());
        l
    }

    // Los mismos casos que `library.rs` de 1.x.

    #[test]
    fn un_seleccionado_que_ya_no_existe_no_vacia_la_biblioteca() {
        let mut l = lib(&[("a", None), ("b", None)]);
        l.selected = Some("borrado".into());
        assert_eq!(l.selected_entry().unwrap().id, "a");
    }

    #[test]
    fn actualizar_un_pack_no_lo_duplica_ni_lo_mueve_de_sitio() {
        let mut l = lib(&[("a", None), ("b", None), ("c", None)]);
        let mut b = e("b", None);
        b.pack_version = "2.0.0".into();
        l.upsert(b);
        assert_eq!(l.instances.len(), 3);
        assert_eq!(l.instances[1].id, "b");
        assert_eq!(l.instances[1].pack_version, "2.0.0");
    }

    #[test]
    fn quitar_el_seleccionado_pasa_al_de_al_lado() {
        let mut l = lib(&[("a", None), ("b", None), ("c", None)]);
        l.select("b");
        l.remove("b");
        assert_eq!(l.selected_entry().unwrap().id, "c");
        l.remove("c");
        assert_eq!(l.selected_entry().unwrap().id, "a");
        l.remove("a");
        assert!(l.selected.is_none() && l.selected_entry().is_none());
    }

    #[test]
    fn quitar_algo_que_no_esta_no_toca_la_seleccion() {
        let mut l = lib(&[("a", None), ("b", None)]);
        assert!(l.remove("nada").is_none());
        assert_eq!(l.selected_entry().unwrap().id, "a");
        assert_eq!(l.instances.len(), 2);
    }

    #[test]
    fn seleccionar_algo_que_no_esta_no_hace_nada() {
        let mut l = lib(&[("a", None)]);
        assert!(!l.select("nada"));
        assert_eq!(l.selected.as_deref(), Some("a"));
    }

    #[test]
    fn dos_packs_distintos_nunca_comparten_carpeta() {
        let l = lib(&[("smp", Some("DINOSMP"))]);
        assert_eq!(l.free_id("smp", Some("OTRO")), "smp-2");
    }

    #[test]
    fn el_mismo_pack_vuelve_a_su_carpeta_aunque_le_cambien_el_id() {
        let l = lib(&[("smp", Some("DINOSMP"))]);
        assert_eq!(l.free_id("dino-smp-nuevo", Some("DINOSMP")), "smp");
    }

    #[test]
    fn reimportar_un_mrpack_a_mano_se_instala_sobre_el_suyo() {
        let l = lib(&[("dinosmp", None)]);
        assert_eq!(l.free_id("dinosmp", None), "dinosmp");
    }

    #[test]
    fn los_sufijos_no_se_pisan_entre_ellos() {
        let l = lib(&[("smp", Some("A")), ("smp-2", Some("B"))]);
        assert_eq!(l.free_id("smp", Some("C")), "smp-3");
    }

    // El almacén.

    fn store(nombre: &str) -> Store {
        let d = std::env::temp_dir().join(format!("dino-config-{}-{nombre}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        Store::new(Paths::new(d))
    }

    #[test]
    fn sin_archivos_se_parte_de_fabrica() {
        let s = store("vacio");
        assert_eq!(s.config().unwrap(), Config::default());
        assert!(s.library().unwrap().instances.is_empty());
    }

    #[test]
    fn un_archivo_danado_es_un_error_y_no_se_pisa() {
        let s = store("roto");
        std::fs::create_dir_all(&s.paths().root).unwrap();
        std::fs::write(s.paths().library(), b"{ esto no").unwrap();
        assert!(s.library().is_err());
        assert!(s.update_library(|l| l.instances.push(e("x", None))).is_err());
        assert_eq!(std::fs::read(s.paths().library()).unwrap(), b"{ esto no");
    }

    #[test]
    fn los_cambios_quedan_guardados() {
        let s = store("cambios");
        s.update_library(|l| {
            l.upsert(e("voidsafio", Some("VOIDSAFIO")));
            l.select("voidsafio")
        })
        .unwrap();
        s.update_config(|c| c.download_concurrency = 16).unwrap();
        let s2 = Store::new(s.paths().clone());
        assert_eq!(s2.library().unwrap().selected.as_deref(), Some("voidsafio"));
        assert_eq!(s2.config().unwrap().download_concurrency, 16);
    }

    #[test]
    fn dos_hilos_a_la_vez_no_se_pierden_cambios() {
        let s = std::sync::Arc::new(store("hilos"));
        let hs: Vec<_> = (0..8)
            .map(|n| {
                let s = s.clone();
                std::thread::spawn(move || s.update_library(|l| l.upsert(e(&format!("p{n}"), None))).unwrap())
            })
            .collect();
        for h in hs {
            h.join().unwrap();
        }
        assert_eq!(s.library().unwrap().instances.len(), 8);
    }
}
