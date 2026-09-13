//! La biblioteca: qué modpacks tiene el usuario y con cuál juega.
//!
//! Antes esto era un único `pack.json`, un pack y ya. El motor nunca tuvo ese límite
//! —sus instancias viven cada una en su carpeta y se lanzan por nombre—, así que pasar
//! a varias es cosa de esta capa y de nadie más.
//!
//! El archivo lo escribimos solo nosotros y guarda **lo que el usuario ha instalado**,
//! que puede ser el trabajo de meses en forma de mundos. Eso marca dos decisiones: un
//! `selected` que apunte a la nada no puede hacer que la biblioteca parezca vacía, y
//! dos packs distintos no pueden acabar compartiendo carpeta.

use serde::{Deserialize, Serialize};

/// Un modpack de la biblioteca.
///
/// `id` es **local**: nombra la carpeta de la instancia y el `.mrpack` en disco. Suele
/// coincidir con el del catálogo pero no tiene por qué, y por eso lo que identifica al
/// pack de cara al catálogo —y lo que sirve para buscarle actualizaciones— es `code`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Instalado {
    pub id: String,
    #[serde(default)]
    pub code: Option<String>,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub server: Option<String>,
    pub mrpack: String,
    /// RAM máxima elegida a mano para este pack, en MB. `None` es «la que toque en esta
    /// máquina»: un pack que nadie ha tocado sigue la recomendación aunque el equipo
    /// cambie de RAM.
    ///
    /// Va aquí y no solo en el `instance.cfg` del motor porque ese archivo se reescribe
    /// entero en cada Jugar: guardada solo ahí se perdería a la primera partida.
    #[serde(default)]
    pub max_mem: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Biblioteca {
    #[serde(rename = "formatVersion")]
    pub format_version: u32,
    /// Con cuál se juega. `None` solo cuando no hay ninguno.
    #[serde(default)]
    pub selected: Option<String>,
    #[serde(default)]
    pub packs: Vec<Instalado>,
}

impl Default for Biblioteca {
    fn default() -> Self {
        Self { format_version: 1, selected: None, packs: Vec::new() }
    }
}

/// Lee el archivo.
///
/// Devuelve error en vez de una biblioteca vacía si el JSON está roto: son cosas
/// distintas, y confundirlas llevaría a sobrescribir la lista buena en el siguiente
/// guardado. A diferencia del catálogo, aquí **no** se rechaza un `formatVersion`
/// futuro: el archivo es local y lo escribimos nosotros, así que leer los campos que
/// entendemos es mejor que dejar al usuario sin sus packs.
pub fn parse(json: &str) -> Result<Biblioteca, serde_json::Error> {
    serde_json::from_str(json)
}

pub fn a_json(b: &Biblioteca) -> String {
    serde_json::to_string_pretty(b).unwrap_or_else(|_| "{}".into())
}

/// Convierte el `pack.json` de un solo pack en una biblioteca de uno.
///
/// Quien ya tenga el launcher instalado tiene ese archivo y su instancia montada;
/// actualizar no puede dejarle la biblioteca en blanco con el pack ahí en el disco.
pub fn migrar(v: &serde_json::Value) -> Biblioteca {
    let mut b = Biblioteca::default();
    if let Ok(p) = serde_json::from_value::<Instalado>(v.clone()) {
        if !p.id.is_empty() {
            b.selected = Some(p.id.clone());
            b.packs.push(p);
        }
    }
    b
}

impl Biblioteca {
    pub fn buscar(&self, id: &str) -> Option<&Instalado> {
        self.packs.iter().find(|p| p.id == id)
    }

    /// El pack con el que se juega.
    ///
    /// Si `selected` señala a uno que ya no está, vale el primero: quedarse sin nada
    /// seleccionado teniendo packs instalados dejaría el botón de Jugar muerto sin que
    /// se vea por qué.
    pub fn seleccionado(&self) -> Option<&Instalado> {
        self.selected
            .as_deref()
            .and_then(|id| self.buscar(id))
            .or_else(|| self.packs.first())
    }

    pub fn seleccionar(&mut self, id: &str) -> bool {
        if self.buscar(id).is_some() {
            self.selected = Some(id.to_string());
            return true;
        }
        false
    }

    /// Mete el pack, o lo actualiza si ya estaba.
    ///
    /// Conserva el sitio en la lista: reinstalar o actualizar no debería reordenar la
    /// biblioteca delante de quien la está mirando. No cambia la selección; eso lo
    /// decide quien llama, porque instalar y actualizar no quieren lo mismo.
    pub fn upsert(&mut self, p: Instalado) {
        match self.packs.iter_mut().find(|x| x.id == p.id) {
            Some(hueco) => *hueco = p,
            None => self.packs.push(p),
        }
    }

    /// Saca el pack de la lista y devuelve lo que había.
    pub fn quitar(&mut self, id: &str) -> Option<Instalado> {
        let i = self.packs.iter().position(|p| p.id == id)?;
        let fuera = self.packs.remove(i);
        if self.selected.as_deref() == Some(id) {
            // El de al lado, que es el que ocupa ahora ese sitio; si era el último, el
            // anterior. Saltar siempre al primero movería la selección más de lo que
            // hace falta.
            self.selected = self.packs.get(i).or_else(|| self.packs.last()).map(|p| p.id.clone());
        }
        Some(fuera)
    }

    /// Qué carpeta le toca a un pack que entra.
    ///
    /// Dos packs distintos con el mismo id se instalarían uno encima del otro, y ahí
    /// están los mundos del primero. Solo se considera conflicto cuando **los dos traen
    /// código y no es el mismo**: es el único caso en que sabemos de verdad que son
    /// packs diferentes. Un `.mrpack` importado a mano sin código se sigue instalando
    /// sobre el suyo, que es lo que se espera al reimportarlo.
    pub fn id_libre(&self, id: &str, code: Option<&str>) -> String {
        // Un pack ya conocido vuelve a su carpeta aunque el catálogo le haya cambiado
        // el id: la instancia montada y sus mundos están donde están.
        if let Some(c) = code.filter(|c| !c.is_empty()) {
            if let Some(p) = self.packs.iter().find(|p| p.code.as_deref() == Some(c)) {
                return p.id.clone();
            }
        }
        let choca = self.packs.iter().any(|p| {
            p.id == id && matches!((p.code.as_deref(), code), (Some(a), Some(b)) if a != b)
        });
        if !choca {
            return id.to_string();
        }
        (2..)
            .map(|n| format!("{id}-{n}"))
            .find(|c| !self.packs.iter().any(|p| p.id == *c))
            .unwrap_or_else(|| id.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pack(id: &str, code: Option<&str>) -> Instalado {
        Instalado {
            id: id.into(),
            code: code.map(String::from),
            name: id.into(),
            version: "1.0.0".into(),
            server: None,
            mrpack: format!("/packs/{id}.mrpack"),
            max_mem: None,
        }
    }

    fn con(ids: &[(&str, Option<&str>)]) -> Biblioteca {
        let mut b = Biblioteca::default();
        for (id, code) in ids {
            b.packs.push(pack(id, *code));
        }
        b.selected = b.packs.first().map(|p| p.id.clone());
        b
    }

    #[test]
    fn migra_el_pack_unico_de_antes() {
        // Quien ya tenía el launcher tiene su instancia en el disco: actualizar no
        // puede dejarle la biblioteca vacía.
        let viejo = serde_json::json!({
            "id": "dinosmp", "code": "DINOSMP", "name": "Dino SMP",
            "version": "1.0.0", "server": "mc.ejemplo.com", "mrpack": "/packs/dinosmp.mrpack"
        });
        let b = migrar(&viejo);
        assert_eq!(b.packs.len(), 1);
        assert_eq!(b.seleccionado().unwrap().name, "Dino SMP");
        assert_eq!(b.packs[0].server.as_deref(), Some("mc.ejemplo.com"));
    }

    #[test]
    fn migra_un_pack_json_sin_codigo() {
        // El de esta máquina, copiado tal cual: se importó a mano antes de que
        // existiera el catálogo, así que no tiene `code`. Si esto fallara, actualizar
        // el launcher le dejaría la biblioteca vacía con la instancia montada.
        let real = serde_json::json!({
            "id": "fabulously-optimized-14-0-0-alpha-4-for-26-2",
            "mrpack": "C:\\Users\\pici\\Downloads\\Fabulously Optimized 14.0.0-alpha.4 for 26.2.mrpack",
            "name": "Fabulously Optimized 14.0.0-alpha.4 for 26.2",
            "server": "mc.universocraft.com",
            "version": "1.0.0"
        });
        let b = migrar(&real);
        let p = b.seleccionado().expect("debería quedar seleccionado");
        assert_eq!(p.id, "fabulously-optimized-14-0-0-alpha-4-for-26-2");
        assert!(p.code.is_none());
        assert_eq!(p.server.as_deref(), Some("mc.universocraft.com"));
    }

    #[test]
    fn sin_pack_json_previo_la_biblioteca_queda_vacia_sin_romperse() {
        assert!(migrar(&serde_json::json!(null)).packs.is_empty());
        assert!(migrar(&serde_json::json!({})).packs.is_empty());
    }

    #[test]
    fn un_seleccionado_que_ya_no_existe_no_vacia_la_biblioteca() {
        // Si esto devolviera None, el botón de Jugar quedaría muerto con packs
        // instalados y sin nada que explicara por qué.
        let mut b = con(&[("a", None), ("b", None)]);
        b.selected = Some("fantasma".into());
        assert_eq!(b.seleccionado().unwrap().id, "a");
    }

    #[test]
    fn actualizar_un_pack_no_lo_duplica_ni_lo_mueve_de_sitio() {
        let mut b = con(&[("a", None), ("b", None), ("c", None)]);
        let mut nueva = pack("b", None);
        nueva.version = "2.0.0".into();
        b.upsert(nueva);
        assert_eq!(b.packs.len(), 3);
        assert_eq!(b.packs[1].id, "b", "no debería reordenarse delante del usuario");
        assert_eq!(b.packs[1].version, "2.0.0");
    }

    #[test]
    fn quitar_el_seleccionado_pasa_al_de_al_lado() {
        let mut b = con(&[("a", None), ("b", None), ("c", None)]);
        b.seleccionar("b");
        b.quitar("b");
        assert_eq!(b.seleccionado().unwrap().id, "c");

        // Y si era el último, al anterior.
        let mut b = con(&[("a", None), ("b", None)]);
        b.seleccionar("b");
        b.quitar("b");
        assert_eq!(b.seleccionado().unwrap().id, "a");

        // Vaciarla del todo deja la selección en nada, no colgando.
        let mut b = con(&[("a", None)]);
        b.quitar("a");
        assert!(b.selected.is_none() && b.seleccionado().is_none());
    }

    #[test]
    fn quitar_algo_que_no_esta_no_toca_la_seleccion() {
        let mut b = con(&[("a", None), ("b", None)]);
        assert!(b.quitar("nada").is_none());
        assert_eq!(b.seleccionado().unwrap().id, "a");
        assert_eq!(b.packs.len(), 2);
    }

    #[test]
    fn dos_packs_distintos_nunca_comparten_carpeta() {
        // Compartirla sería instalar uno encima del otro, y ahí están los mundos.
        let b = con(&[("smp", Some("DINOSMP"))]);
        assert_eq!(b.id_libre("smp", Some("OTRO")), "smp-2");
    }

    #[test]
    fn el_mismo_pack_vuelve_a_su_carpeta_aunque_le_cambien_el_id() {
        // Su instancia y sus mundos están donde están; el catálogo no los mueve.
        let b = con(&[("smp", Some("DINOSMP"))]);
        assert_eq!(b.id_libre("dino-smp-nuevo", Some("DINOSMP")), "smp");
    }

    #[test]
    fn reimportar_un_mrpack_a_mano_se_instala_sobre_el_suyo() {
        // Sin código no hay forma de saber que son distintos, y crear una copia dejaría
        // una instancia huérfana cada vez que el admin prueba su propio pack.
        let b = con(&[("dinosmp", None)]);
        assert_eq!(b.id_libre("dinosmp", None), "dinosmp");
    }

    #[test]
    fn los_sufijos_no_se_pisan_entre_ellos() {
        let b = con(&[("smp", Some("A")), ("smp-2", Some("B"))]);
        assert_eq!(b.id_libre("smp", Some("C")), "smp-3");
    }

    #[test]
    fn un_json_roto_no_se_confunde_con_una_biblioteca_vacia() {
        // Tratarlo como vacía haría que el siguiente guardado se llevara la lista buena.
        assert!(parse("{ esto no es json").is_err());
        assert!(parse(r#"{ "formatVersion": 1 }"#).unwrap().packs.is_empty());
    }

    #[test]
    fn lo_que_se_guarda_se_vuelve_a_leer_igual() {
        let mut b = con(&[("a", Some("AAA")), ("b", None)]);
        b.seleccionar("b");
        let leida = parse(&a_json(&b)).unwrap();
        assert_eq!(leida.selected.as_deref(), Some("b"));
        assert_eq!(leida.packs, b.packs);
    }

    #[test]
    fn un_packs_json_de_antes_de_la_ram_por_pack_se_sigue_leyendo() {
        // Los que ya tengan biblioteca la escribieron sin este campo.
        let j = r#"{ "formatVersion": 1, "selected": "a", "packs": [
            { "id": "a", "name": "A", "version": "1", "mrpack": "/p/a.mrpack" } ] }"#;
        assert!(parse(j).unwrap().packs[0].max_mem.is_none());
    }

    #[test]
    fn actualizar_un_pack_conserva_la_ram_que_eligio_el_jugador() {
        // La actualización llega con los datos del catálogo, que no sabe nada de lo
        // que cada cual ajustó en su máquina.
        let mut b = con(&[("a", Some("AAA"))]);
        b.packs[0].max_mem = Some(6144);
        let antes = b.buscar("a").cloned().unwrap();
        b.upsert(Instalado { version: "2.0.0".into(), ..antes });
        assert_eq!(b.packs[0].max_mem, Some(6144));
        assert_eq!(b.packs[0].version, "2.0.0");
    }

    #[test]
    fn un_formato_futuro_no_borra_los_packs_que_si_entendemos() {
        // Al revés que el catálogo: este archivo es nuestro y local, y quedarse sin
        // biblioteca es peor que leer de menos.
        let j = r#"{ "formatVersion": 9, "selected": "a", "packs": [
            { "id": "a", "name": "A", "mrpack": "/p/a.mrpack", "loquesea": 1 } ] }"#;
        let b = parse(j).unwrap();
        assert_eq!(b.seleccionado().unwrap().id, "a");
    }
}
