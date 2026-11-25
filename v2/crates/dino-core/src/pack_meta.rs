//! Lo que quien publica un pack puede añadirle además de los archivos: la actividad de
//! Discord, las notas de parche y el tema del launcher.
//!
//! Todo es **opcional y está apagado por omisión**: un pack que no dice nada se comporta
//! exactamente como antes (sin presencia, sin notas, con el tema base). Vive en el
//! catálogo (un archivo remoto, así que no es de fiar) y por eso cada pieza pasa por su
//! `sanitized()` antes de usarse: colores solo en hexadecimal, enlaces solo `https`,
//! textos acotados. La interfaz pinta con `textContent`, pero esto es la primera red.

use serde::{Deserialize, Serialize};

/// Lo opcional de un pack publicado. Se guarda tal cual en el catálogo (aplanado dentro de
/// `PackRemoto`) y en `packs/<id>.extras.json` como copia para cuando no hay red.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PackExtras {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub presence: Option<Presence>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<PatchNotes>,
}

impl PackExtras {
    pub fn is_empty(&self) -> bool {
        self.presence.is_none() && self.theme.is_none() && self.notes.is_none()
    }

    /// Los campos que el editor puede llenar con un `data:` (archivo elegido en el equipo)
    /// y que al publicar se suben al repo y se cambian por su enlace.
    pub fn media_slots_mut(&mut self) -> Vec<(MediaKind, &mut String)> {
        let mut out: Vec<(MediaKind, &mut String)> = Vec::new();
        if let Some(p) = self.presence.as_mut() {
            if let Some(i) = p.image.as_mut() {
                out.push((MediaKind::Image, i));
            }
        }
        if let Some(n) = self.notes.as_mut() {
            for img in n.images.iter_mut() {
                out.push((MediaKind::Image, &mut img.url));
            }
        }
        if let Some(t) = self.theme.as_mut() {
            if let Some(b) = t.decor.banner.as_mut() {
                out.push((MediaKind::Image, b));
            }
            let s = &mut t.sounds;
            for slot in [&mut s.click, &mut s.hover, &mut s.done, &mut s.play] {
                if let Some(v) = slot.as_mut() {
                    out.push((MediaKind::Audio, v));
                }
            }
        }
        out
    }

    /// La copia segura: lo que no pasa el saneado se descarta, no se arregla a medias.
    pub fn sanitized(&self) -> PackExtras {
        PackExtras {
            presence: self.presence.as_ref().map(Presence::sanitized).filter(|p| p.enabled),
            theme: self.theme.as_ref().map(Theme::sanitized).filter(|t| t.enabled),
            notes: self.notes.as_ref().map(PatchNotes::sanitized).filter(|n| !n.is_empty()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Audio,
}

// ---------------------------------------------------------------- texto y enlaces

fn clip(s: &str, max: usize) -> String {
    s.trim().chars().filter(|c| !c.is_control() || *c == '\n').take(max).collect::<String>().trim().to_string()
}

fn clip_opt(s: &Option<String>, max: usize) -> Option<String> {
    s.as_deref().map(|s| clip(s, max)).filter(|s| !s.is_empty())
}

/// Un enlace `https` sin espacios ni comillas, o nada.
pub fn safe_url(s: &str) -> Option<String> {
    let s = s.trim();
    let ok = s.len() <= 512
        && s.starts_with("https://")
        && s.len() > "https://".len()
        && !s.chars().any(|c| c.is_whitespace() || c.is_control() || matches!(c, '"' | '\'' | '<' | '>' | '\\'));
    ok.then(|| s.to_string())
}

fn safe_url_opt(s: &Option<String>) -> Option<String> {
    s.as_deref().and_then(safe_url)
}

/// `#RGB` o `#RRGGBB`, en minúsculas y siempre de seis cifras; cualquier otra cosa, nada.
/// Es lo único que llega a una variable CSS: así un color no puede traer otra declaración.
pub fn safe_color(s: &str) -> Option<String> {
    let h = s.trim().strip_prefix('#')?;
    if !h.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    match h.len() {
        6 => Some(format!("#{}", h.to_ascii_lowercase())),
        3 => Some(format!("#{}", h.chars().flat_map(|c| [c, c]).collect::<String>().to_ascii_lowercase())),
        _ => None,
    }
}

/// ¿Es un `data:` de imagen o audio? Lo que el editor manda para que se suba al publicar.
pub fn is_data_uri(s: &str) -> bool {
    s.starts_with("data:image/") || s.starts_with("data:audio/")
}

// ---------------------------------------------------------------- Discord

/// La actividad de Discord de un pack.
///
/// `details` y `state` aceptan `{pack}`, `{server}`, `{version}`, `{minecraft}` y
/// `{player}`; ver [`Presence::render`].
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Presence {
    /// Apagada por omisión: si el pack no la activa, el launcher no manda nada a Discord.
    #[serde(default)]
    pub enabled: bool,
    /// Aplicación de Discord con otro nombre y logos (p. ej. la del servidor). Sin esto se
    /// usa la de Dino.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub client_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub details: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<String>,
    /// Logo grande: un `https://…` o la clave de un asset de la aplicación de Discord.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_text: Option<String>,
    /// Botón para que otros se unan. Sin `url`, apunta a la página de invitación del pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub join: Option<JoinButton>,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct JoinButton {
    pub label: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

/// Los datos con los que se rellenan las plantillas de la actividad.
#[derive(Debug, Clone, Default)]
pub struct PresenceVars<'a> {
    pub pack: &'a str,
    pub server: Option<&'a str>,
    pub version: &'a str,
    pub minecraft: &'a str,
    pub player: &'a str,
}

/// La actividad ya resuelta, lista para mandar a Discord.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Activity {
    pub details: Option<String>,
    pub state: Option<String>,
    pub image: Option<String>,
    pub image_text: Option<String>,
    pub button: Option<(String, String)>,
}

impl Presence {
    pub fn sanitized(&self) -> Presence {
        Presence {
            enabled: self.enabled,
            client_id: self.client_id.as_deref().map(str::trim).filter(|c| is_snowflake(c)).map(String::from),
            details: clip_opt(&self.details, 128),
            state: clip_opt(&self.state, 128),
            image: self.image.as_deref().and_then(|i| {
                safe_url(i).or_else(|| {
                    let k = i.trim();
                    (!k.is_empty()
                        && k.len() <= 64
                        && k.chars().all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_' || c == '-'))
                    .then(|| k.to_string())
                })
            }),
            image_text: clip_opt(&self.image_text, 128),
            join: self.join.as_ref().and_then(|j| {
                let label = clip(&j.label, 32);
                (!label.is_empty()).then(|| JoinButton { label, url: safe_url_opt(&j.url) })
            }),
        }
    }

    /// Resuelve plantillas y botón. `invite_base` es la página de invitación (el código del
    /// pack se le pega detrás). Un texto que queda vacío o de un solo carácter se omite:
    /// Discord rechaza la actividad entera si alguno mide menos de 2.
    pub fn render(&self, v: &PresenceVars, code: Option<&str>, invite_base: &str) -> Activity {
        let fill = |t: &Option<String>| -> Option<String> {
            let t = t.as_deref()?;
            let out = t
                .replace("{pack}", v.pack)
                .replace("{server}", v.server.unwrap_or(""))
                .replace("{version}", v.version)
                .replace("{minecraft}", v.minecraft)
                .replace("{player}", v.player);
            let out = clip(&out, 128);
            (out.chars().count() >= 2).then_some(out)
        };
        let button = self.join.as_ref().and_then(|j| {
            let url = j.url.clone().or_else(|| code.map(|c| format!("{invite_base}{c}")))?;
            safe_url(&url).map(|u| (j.label.clone(), u))
        });
        Activity {
            details: fill(&self.details),
            state: fill(&self.state),
            image: self.image.clone(),
            image_text: fill(&self.image_text),
            button,
        }
    }
}

/// Un id de aplicación de Discord: 17 a 20 cifras.
pub fn is_snowflake(s: &str) -> bool {
    (17..=20).contains(&s.len()) && s.chars().all(|c| c.is_ascii_digit())
}

// ---------------------------------------------------------------- notas de parche

/// Las novedades de una versión: texto estructurado e imágenes, para leerlas dentro del
/// launcher sin salir a un enlace.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct PatchNotes {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sections: Vec<NoteSection>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<NoteImage>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteSection {
    #[serde(default)]
    pub kind: NoteKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default)]
    pub items: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NoteKind {
    /// Cosas nuevas.
    Feature,
    /// Cambios y ajustes.
    #[default]
    Change,
    /// Correcciones.
    Fix,
    /// Aviso suelto.
    Note,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NoteImage {
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caption: Option<String>,
}

pub const MAX_SECTIONS: usize = 12;
pub const MAX_ITEMS: usize = 40;
pub const MAX_IMAGES: usize = 8;

impl PatchNotes {
    pub fn is_empty(&self) -> bool {
        self.title.is_none() && self.summary.is_none() && self.sections.is_empty() && self.images.is_empty()
    }

    pub fn sanitized(&self) -> PatchNotes {
        PatchNotes {
            title: clip_opt(&self.title, 120),
            summary: clip_opt(&self.summary, 600),
            sections: self
                .sections
                .iter()
                .take(MAX_SECTIONS)
                .map(|s| NoteSection {
                    kind: s.kind,
                    title: clip_opt(&s.title, 80),
                    items: s.items.iter().map(|i| clip(i, 400)).filter(|i| !i.is_empty()).take(MAX_ITEMS).collect(),
                })
                .filter(|s| !s.items.is_empty())
                .collect(),
            images: self
                .images
                .iter()
                .filter_map(|i| safe_url(&i.url).map(|url| NoteImage { url, caption: clip_opt(&i.caption, 160) }))
                .take(MAX_IMAGES)
                .collect(),
        }
    }
}

// ---------------------------------------------------------------- tema

/// El aspecto del launcher mientras ese pack está elegido.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Theme {
    /// Apagado por omisión: sin esto el launcher no cambia nada.
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub colors: ThemeColors,
    #[serde(default)]
    pub sounds: ThemeSounds,
    #[serde(default)]
    pub decor: ThemeDecor,
}

/// Todos opcionales y en hexadecimal: lo que falte se queda con el tema base.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThemeColors {
    /// Color principal (botones, acentos).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub primary: Option<String>,
    /// Variante más viva del principal (resaltes).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accent: Option<String>,
    /// Variante oscura del principal (fondos de botón).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deep: Option<String>,
    /// Fondo de la ventana.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ink: Option<String>,
    /// Color de las superficies de vidrio (paneles, tarjetas).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub surface: Option<String>,
    /// Bordes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub border: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

/// Qué suena en cada momento: el nombre de un sonido de fábrica (`soft`, `blip`, `chime`,
/// `void`) o un `https://…` a un archivo de audio. Sin nada, ese momento queda en silencio.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThemeSounds {
    /// Volumen propio del pack, de 0 a 1. El jugador tiene su propio control encima.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume: Option<f32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub click: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hover: Option<String>,
    /// Terminó una instalación o actualización.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub done: Option<String>,
    /// Se pulsó Jugar.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub play: Option<String>,
}

pub const SOUND_PRESETS: [&str; 4] = ["soft", "blip", "chime", "void"];

/// Adornos que no tocan la disposición: una imagen de cabecera, una etiqueta y un marco.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ThemeDecor {
    /// Imagen que se pone de fondo en la ficha del pack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub banner: Option<String>,
    /// Etiqueta corta junto al nombre («Servidor oficial»).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub badge: Option<String>,
    /// Marco de la ficha: `none`, `glow`, `double` o `pixel`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub frame: Option<String>,
    /// Frase bajo el nombre.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tagline: Option<String>,
}

pub const FRAMES: [&str; 4] = ["none", "glow", "double", "pixel"];

fn sound(s: &Option<String>) -> Option<String> {
    let s = s.as_deref()?.trim();
    if SOUND_PRESETS.contains(&s) {
        return Some(s.to_string());
    }
    safe_url(s)
}

impl Theme {
    pub fn sanitized(&self) -> Theme {
        let c = |o: &Option<String>| o.as_deref().and_then(safe_color);
        Theme {
            enabled: self.enabled,
            colors: ThemeColors {
                primary: c(&self.colors.primary),
                accent: c(&self.colors.accent),
                deep: c(&self.colors.deep),
                ink: c(&self.colors.ink),
                surface: c(&self.colors.surface),
                border: c(&self.colors.border),
                text: c(&self.colors.text),
            },
            sounds: ThemeSounds {
                volume: self.sounds.volume.map(|v| if v.is_finite() { v.clamp(0.0, 1.0) } else { 0.0 }),
                click: sound(&self.sounds.click),
                hover: sound(&self.sounds.hover),
                done: sound(&self.sounds.done),
                play: sound(&self.sounds.play),
            },
            decor: ThemeDecor {
                banner: safe_url_opt(&self.decor.banner),
                badge: clip_opt(&self.decor.badge, 28),
                frame: self.decor.frame.as_deref().map(str::trim).filter(|f| FRAMES.contains(f)).map(String::from),
                tagline: clip_opt(&self.decor.tagline, 90),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn un_pack_sin_extras_no_activa_nada() {
        let e: PackExtras = serde_json::from_str("{}").unwrap();
        assert!(e.is_empty());
        assert!(e.sanitized().is_empty());
        // y no deja rastro al guardarse
        assert_eq!(serde_json::to_string(&e).unwrap(), "{}");
    }

    #[test]
    fn lo_apagado_se_descarta_aunque_traiga_datos() {
        let e: PackExtras = serde_json::from_str(
            r##"{"presence":{"enabled":false,"details":"hola"},"theme":{"enabled":false,"colors":{"primary":"#fff"}}}"##,
        )
        .unwrap();
        assert!(e.sanitized().is_empty(), "lo desactivado no se fuerza al jugador");
    }

    #[test]
    fn un_color_solo_es_un_color() {
        assert_eq!(safe_color("#A0f"), Some("#aa00ff".into()));
        assert_eq!(safe_color(" #73BF69 "), Some("#73bf69".into()));
        assert_eq!(safe_color("red"), None);
        assert_eq!(safe_color("#fff; background:url(x)"), None);
        assert_eq!(safe_color("#12345"), None);
    }

    #[test]
    fn solo_https_y_sin_comillas() {
        assert!(safe_url("https://x.io/a.png").is_some());
        assert!(safe_url("http://x.io/a.png").is_none());
        assert!(safe_url("javascript:alert(1)").is_none());
        assert!(safe_url("https://x.io/a\" onerror=\"x").is_none());
        assert!(safe_url("https://").is_none());
    }

    #[test]
    fn el_tema_saneado_no_deja_pasar_basura() {
        let t = Theme {
            enabled: true,
            colors: ThemeColors { primary: Some("#7a3cff".into()), ink: Some("url(x)".into()), ..Default::default() },
            sounds: ThemeSounds {
                volume: Some(9.0),
                click: Some("blip".into()),
                hover: Some("http://x/y.ogg".into()),
                done: Some("https://x.io/done.ogg".into()),
                play: Some("../../etc".into()),
            },
            decor: ThemeDecor {
                banner: Some("data:image/png;base64,AAAA".into()),
                badge: Some("Oficial".into()),
                frame: Some("neon".into()),
                tagline: None,
            },
        }
        .sanitized();
        assert_eq!(t.colors.primary.as_deref(), Some("#7a3cff"));
        assert_eq!(t.colors.ink, None);
        assert_eq!(t.sounds.volume, Some(1.0));
        assert_eq!(t.sounds.click.as_deref(), Some("blip"));
        assert_eq!((t.sounds.hover, t.sounds.play), (None, None));
        assert!(t.sounds.done.is_some());
        assert_eq!((t.decor.banner, t.decor.frame), (None, None));
        assert_eq!(t.decor.badge.as_deref(), Some("Oficial"));
    }

    #[test]
    fn las_plantillas_de_discord_se_rellenan_y_lo_vacio_se_omite() {
        let p = Presence {
            enabled: true,
            details: Some("Jugando {pack}".into()),
            state: Some("{server}".into()),
            image_text: Some("x".into()),
            join: Some(JoinButton { label: "Unirse".into(), url: None }),
            ..Default::default()
        };
        let v = PresenceVars { pack: "Voidasio", server: None, version: "3.1", minecraft: "1.21.1", player: "Dino" };
        let a = p.render(&v, Some("VOIDASIO"), "https://dino.example/invitar/#");
        assert_eq!(a.details.as_deref(), Some("Jugando Voidasio"));
        assert_eq!(a.state, None, "sin servidor la plantilla queda vacía y no se manda");
        assert_eq!(a.image_text, None, "Discord rechaza textos de un carácter");
        assert_eq!(a.button, Some(("Unirse".into(), "https://dino.example/invitar/#VOIDASIO".into())));
        // un pack local no tiene código: sin url propia, no hay botón
        let sin = p.render(&v, None, "https://dino.example/invitar/#");
        assert_eq!(sin.button, None);
    }

    #[test]
    fn los_ids_de_discord_son_numeros_largos() {
        assert!(is_snowflake("1234567890123456789"));
        assert!(!is_snowflake("12345"));
        assert!(!is_snowflake("12345678901234567x"));
        let p = Presence { enabled: true, client_id: Some("abc".into()), ..Default::default() };
        assert_eq!(p.sanitized().client_id, None);
    }

    #[test]
    fn las_notas_se_acotan_y_solo_dejan_imagenes_https() {
        let n = PatchNotes {
            title: Some("  3.16  ".into()),
            summary: None,
            sections: vec![
                NoteSection { kind: NoteKind::Fix, title: None, items: vec!["  ".into(), "Arreglado X".into()] },
                NoteSection { kind: NoteKind::Note, title: None, items: vec![] },
            ],
            images: vec![
                NoteImage { url: "https://x.io/a.png".into(), caption: Some("a".into()) },
                NoteImage { url: "file:///c:/secreto.png".into(), caption: None },
            ],
        }
        .sanitized();
        assert_eq!(n.title.as_deref(), Some("3.16"));
        assert_eq!(n.sections.len(), 1, "una sección sin nada que contar no se enseña");
        assert_eq!(n.sections[0].items, vec!["Arreglado X"]);
        assert_eq!(n.images.len(), 1);
    }

    #[test]
    fn las_notas_hacen_ida_y_vuelta_por_json() {
        let j = r#"{"title":"Nuevo","sections":[{"kind":"feature","items":["a"]}],"images":[{"url":"https://x.io/i.png"}]}"#;
        let n: PatchNotes = serde_json::from_str(j).unwrap();
        assert_eq!(n.sections[0].kind, NoteKind::Feature);
        let back: PatchNotes = serde_json::from_str(&serde_json::to_string(&n).unwrap()).unwrap();
        assert_eq!(n, back);
    }
}
