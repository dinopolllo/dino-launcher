//! Perfiles por jugador: lo que cada cuenta elige y no debe pisar a las demás.
//!
//! `profiles.json` guarda, por UUID de cuenta, sus preferencias. Todo es `Option`: **`None`
//! es «no lo ha tocado»**, y entonces manda lo de abajo. El orden de resolución, de más
//! cercano al jugador a más lejano:
//!
//! 1. lo que ese jugador eligió para **esa instancia** (`Profile::instances`);
//! 2. lo que ese jugador eligió para **todas** (`Profile::window`);
//! 3. lo que dice la propia instancia (`instance.json`, que es donde 1.x y la migración
//!    dejaron la memoria y el Java);
//! 4. el valor automático (RAM según la máquina, Java de Mojang, ventana del juego).
//!
//! Un jugador nuevo tiene un perfil vacío, así que juega con 3 y 4: nadie hereda lo de otro.

use crate::schema::{InstanceManifest, JavaChoice, Memory};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const PROFILES_SCHEMA: u32 = 1;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProfilesFile {
    pub schema: u32,
    /// Por UUID de cuenta.
    #[serde(default)]
    pub profiles: BTreeMap<String, Profile>,
}

impl Default for ProfilesFile {
    fn default() -> Self {
        Self { schema: PROFILES_SCHEMA, profiles: BTreeMap::new() }
    }
}

impl ProfilesFile {
    /// El perfil de una cuenta; vacío si nunca lo tocó.
    pub fn of(&self, uuid: &str) -> Profile {
        self.profiles.get(uuid).cloned().unwrap_or_default()
    }

    /// Cambia el perfil de una cuenta. Si queda vacío se borra: no se acumulan perfiles
    /// fantasma de cuentas que ya no existen o que nunca eligieron nada.
    pub fn update(&mut self, uuid: &str, f: impl FnOnce(&mut Profile)) {
        let mut p = self.of(uuid);
        f(&mut p);
        if p.is_default() {
            self.profiles.remove(uuid);
        } else {
            self.profiles.insert(uuid.to_string(), p);
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Profile {
    /// Preferencias del propio launcher para este jugador.
    #[serde(default, skip_serializing_if = "LauncherPrefs::is_default")]
    pub launcher: LauncherPrefs,
    /// Ventana del juego para todas sus instancias.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowPref>,
    /// Lo que eligió para una instancia concreta (por id de instancia).
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub instances: BTreeMap<String, InstanceOverrides>,
}

impl Profile {
    pub fn is_default(&self) -> bool {
        self.launcher.is_default()
            && self.window.is_none()
            && self.instances.values().all(InstanceOverrides::is_default)
    }
}

/// Preferencias del launcher. `None` = la de fábrica ([`LauncherPrefs::resolved`]).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct LauncherPrefs {
    /// La cabeza de la cuenta mira al cursor.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub head_tracking: Option<bool>,
    /// Sonidos de la interfaz (los del tema del pack y los de fábrica).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_sounds: Option<bool>,
    /// Volumen de esos sonidos, de 0 a 1.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ui_volume: Option<f32>,
    /// Aplicar el tema que traiga el pack elegido.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack_themes: Option<bool>,
    /// Contar en Discord a qué se juega.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub discord_presence: Option<bool>,
}

impl LauncherPrefs {
    pub fn is_default(&self) -> bool {
        *self == LauncherPrefs::default()
    }

    pub fn resolved(&self) -> ResolvedPrefs {
        ResolvedPrefs {
            head_tracking: self.head_tracking.unwrap_or(true),
            ui_sounds: self.ui_sounds.unwrap_or(true),
            ui_volume: self.ui_volume.filter(|v| v.is_finite()).map_or(0.6, |v| v.clamp(0.0, 1.0)),
            pack_themes: self.pack_themes.unwrap_or(true),
            discord_presence: self.discord_presence.unwrap_or(true),
        }
    }
}

/// Lo que de verdad vale para un jugador: sus elecciones o, si no ha elegido, las de fábrica.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedPrefs {
    pub head_tracking: bool,
    pub ui_sounds: bool,
    pub ui_volume: f32,
    pub pack_themes: bool,
    pub discord_presence: bool,
}

impl Default for ResolvedPrefs {
    fn default() -> Self {
        LauncherPrefs::default().resolved()
    }
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct InstanceOverrides {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Memory>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub jvm_args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub java: Option<JavaChoice>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window: Option<WindowPref>,
}

impl InstanceOverrides {
    pub fn is_default(&self) -> bool {
        *self == InstanceOverrides::default()
    }
}

/// Cómo se abre la ventana del juego.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum WindowPref {
    /// La que elija Minecraft.
    Default,
    Fullscreen,
    Size {
        width: u32,
        height: u32,
    },
}

impl WindowPref {
    /// Tamaños que Minecraft aguanta; fuera de ahí, se vuelve a la ventana por omisión.
    pub fn sanitized(self) -> WindowPref {
        match self {
            WindowPref::Size { width, height } if (320..=16384).contains(&width) && (240..=16384).contains(&height) => {
                self
            }
            WindowPref::Size { .. } => WindowPref::Default,
            other => other,
        }
    }

    /// Los argumentos de juego que lo piden; ninguno para la ventana por omisión.
    pub fn game_args(self) -> Vec<String> {
        match self.sanitized() {
            WindowPref::Fullscreen => vec!["--fullscreen".into()],
            WindowPref::Size { width, height } => {
                vec!["--width".into(), width.to_string(), "--height".into(), height.to_string()]
            }
            WindowPref::Default => vec![],
        }
    }
}

/// Lo que se usa al lanzar una instancia, ya resuelto.
#[derive(Debug, Clone, PartialEq)]
pub struct Effective {
    pub memory: Memory,
    pub jvm_args: Vec<String>,
    pub java: JavaChoice,
    pub window: WindowPref,
}

/// Dónde se decidió cada valor (para decírselo al jugador en Ajustes).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// Lo eligió este jugador para esta instancia.
    Profile,
    /// Viene de la propia instancia (pack, migración) o es el automático.
    Pack,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Resolved {
    pub effective: Effective,
    pub memory_from: Source,
    pub jvm_args_from: Source,
    pub java_from: Source,
    pub window_from: Source,
}

fn pick<T>(own: Option<T>, base: T) -> (T, Source) {
    match own {
        Some(v) => (v, Source::Profile),
        None => (base, Source::Pack),
    }
}

/// Aplica el orden de resolución de este módulo.
pub fn resolve(profile: Option<&Profile>, instance_id: &str, manifest: &InstanceManifest) -> Resolved {
    let none = Profile::default();
    let p = profile.unwrap_or(&none);
    let o = p.instances.get(instance_id).cloned().unwrap_or_default();
    let (memory, memory_from) = pick(o.memory, manifest.memory);
    let (jvm_args, jvm_args_from) = pick(o.jvm_args, manifest.jvm_args.clone());
    let (java, java_from) = pick(o.java, manifest.java.clone());
    let (window, window_from) = match (o.window, p.window) {
        (Some(w), _) => (w.sanitized(), Source::Profile),
        (None, Some(w)) => (w.sanitized(), Source::Profile),
        (None, None) => (WindowPref::Default, Source::Pack),
    };
    Resolved {
        effective: Effective { memory, jvm_args, java, window },
        memory_from,
        jvm_args_from,
        java_from,
        window_from,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::{InstallState, LoaderSpec};

    fn manifest(memory: Memory) -> InstanceManifest {
        InstanceManifest {
            schema: 1,
            id: "vs".into(),
            minecraft: "1.21.1".into(),
            loader: LoaderSpec::Vanilla,
            state: InstallState::Ready,
            memory,
            jvm_args: vec!["-Xss2M".into()],
            java: JavaChoice::Auto,
            managed_files: vec![],
        }
    }

    #[test]
    fn un_jugador_nuevo_juega_con_lo_de_la_instancia() {
        let r = resolve(None, "vs", &manifest(Memory::Fixed { max_mb: 6144 }));
        assert_eq!(r.effective.memory, Memory::Fixed { max_mb: 6144 });
        assert_eq!(r.effective.jvm_args, vec!["-Xss2M".to_string()]);
        assert_eq!(r.effective.window, WindowPref::Default);
        assert_eq!(r.memory_from, Source::Pack);
        assert_eq!(LauncherPrefs::default().resolved(), ResolvedPrefs::default());
        assert!(ResolvedPrefs::default().head_tracking);
    }

    #[test]
    fn cada_jugador_conserva_lo_suyo_al_alternar_cuentas() {
        let mut f = ProfilesFile::default();
        f.update("ana", |p| {
            p.instances.entry("vs".into()).or_default().memory = Some(Memory::Fixed { max_mb: 8192 });
        });
        f.update("beto", |p| p.window = Some(WindowPref::Fullscreen));
        let m = manifest(Memory::Auto);
        let ana = resolve(Some(&f.of("ana")), "vs", &m);
        let beto = resolve(Some(&f.of("beto")), "vs", &m);
        let nuevo = resolve(Some(&f.of("carla")), "vs", &m);
        assert_eq!(ana.effective.memory, Memory::Fixed { max_mb: 8192 });
        assert_eq!(ana.memory_from, Source::Profile);
        assert_eq!(beto.effective.memory, Memory::Auto, "lo de Ana no se le pega a Beto");
        assert_eq!(beto.effective.window, WindowPref::Fullscreen);
        assert_eq!(nuevo.effective.window, WindowPref::Default);
        // y lo de Ana en otra instancia no existe
        assert_eq!(resolve(Some(&f.of("ana")), "otra", &m).effective.memory, Memory::Auto);
    }

    #[test]
    fn elegir_automatico_gana_a_lo_que_trae_la_instancia() {
        let mut f = ProfilesFile::default();
        f.update("ana", |p| p.instances.entry("vs".into()).or_default().memory = Some(Memory::Auto));
        let r = resolve(Some(&f.of("ana")), "vs", &manifest(Memory::Fixed { max_mb: 2048 }));
        assert_eq!(r.effective.memory, Memory::Auto);
    }

    #[test]
    fn la_ventana_de_la_instancia_manda_sobre_la_del_jugador() {
        let mut p = Profile { window: Some(WindowPref::Fullscreen), ..Default::default() };
        p.instances.insert(
            "vs".into(),
            InstanceOverrides { window: Some(WindowPref::Size { width: 1280, height: 720 }), ..Default::default() },
        );
        let r = resolve(Some(&p), "vs", &manifest(Memory::Auto));
        assert_eq!(r.effective.window.game_args(), vec!["--width", "1280", "--height", "720"]);
        assert_eq!(
            resolve(Some(&p), "otra", &manifest(Memory::Auto)).effective.window.game_args(),
            vec!["--fullscreen"]
        );
    }

    #[test]
    fn un_perfil_que_vuelve_a_ser_el_de_fabrica_se_borra() {
        let mut f = ProfilesFile::default();
        f.update("ana", |p| p.launcher.head_tracking = Some(false));
        assert!(f.profiles.contains_key("ana"));
        f.update("ana", |p| p.launcher = LauncherPrefs::default());
        assert!(f.profiles.is_empty());
    }

    #[test]
    fn una_ventana_imposible_se_ignora() {
        assert_eq!(WindowPref::Size { width: 0, height: 99999 }.sanitized(), WindowPref::Default);
        assert!(WindowPref::Default.game_args().is_empty());
        assert!(WindowPref::Size { width: 1, height: 1 }.game_args().is_empty());
    }

    #[test]
    fn el_volumen_se_acota() {
        let p = LauncherPrefs { ui_volume: Some(7.0), ..Default::default() };
        assert_eq!(p.resolved().ui_volume, 1.0);
        let p = LauncherPrefs { ui_volume: Some(f32::NAN), ..Default::default() };
        assert_eq!(p.resolved().ui_volume, 0.6);
    }

    #[test]
    fn forma_en_json() {
        let j = serde_json::to_value(WindowPref::Size { width: 1280, height: 720 }).unwrap();
        assert_eq!(j["mode"], "size");
        let p: Profile = serde_json::from_str("{}").unwrap();
        assert!(p.is_default());
    }
}
