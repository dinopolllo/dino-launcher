//! Discord Rich Presence, hablando con el Discord de la propia máquina por IPC.
//!
//! No hay red ni SDK: Discord abre una tubería con nombre (`\\.\pipe\discord-ipc-N` en
//! Windows) o un socket Unix (`discord-ipc-N`) y se le habla con tramas
//! `[opcode u32 LE][longitud u32 LE][JSON]`. El apretón es un `HANDSHAKE` con el
//! `client_id`; luego `SET_ACTIVITY` pone (o, sin actividad, quita) la presencia.
//!
//! [`start`] devuelve un [`Handle`] que vive en su propio hilo: intenta conectar sin
//! bloquear a quien llama, reintenta cada rato si Discord no está abierto, y al soltarse
//! (o con [`Handle::stop`]) borra la actividad y cierra. Si el proceso muere sin llegar
//! ahí, Discord la quita solo al cerrarse la tubería.

use dino_core::pack_meta::Activity;
use serde_json::{json, Value};
use std::io::{Read, Write};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const OP_HANDSHAKE: u32 = 0;
const OP_FRAME: u32 = 1;
const OP_CLOSE: u32 = 2;
/// Cada cuánto se reintenta si Discord no estaba abierto.
const RETRY: Duration = Duration::from_secs(20);
/// Cada cuánto se reafirma la actividad: así se nota una tubería rota (Discord reiniciado).
const BEAT: Duration = Duration::from_secs(60);

// ---------------------------------------------------------------- tramas

/// Una trama lista para escribir.
pub fn encode(op: u32, payload: &Value) -> Vec<u8> {
    let body = serde_json::to_vec(payload).expect("un Value siempre serializa");
    let mut out = Vec::with_capacity(8 + body.len());
    out.extend_from_slice(&op.to_le_bytes());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

/// Lee una trama. Una longitud absurda es un error, no una reserva de memoria.
pub fn decode(r: &mut impl Read) -> std::io::Result<(u32, Value)> {
    let mut head = [0u8; 8];
    r.read_exact(&mut head)?;
    let op = u32::from_le_bytes(head[..4].try_into().expect("4 bytes"));
    let len = u32::from_le_bytes(head[4..].try_into().expect("4 bytes")) as usize;
    if len > 1 << 20 {
        return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "trama de Discord demasiado grande"));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    let v = serde_json::from_slice(&body).unwrap_or(Value::Null);
    Ok((op, v))
}

/// El JSON de `activity`. `start` son segundos Unix: Discord enseña «lleva X jugando».
pub fn activity_json(a: &Activity, start: u64) -> Value {
    let mut act = serde_json::Map::new();
    if let Some(d) = &a.details {
        act.insert("details".into(), json!(d));
    }
    if let Some(s) = &a.state {
        act.insert("state".into(), json!(s));
    }
    act.insert("timestamps".into(), json!({ "start": start }));
    if a.image.is_some() || a.image_text.is_some() {
        let mut assets = serde_json::Map::new();
        if let Some(i) = &a.image {
            assets.insert("large_image".into(), json!(i));
        }
        if let Some(t) = &a.image_text {
            assets.insert("large_text".into(), json!(t));
        }
        act.insert("assets".into(), Value::Object(assets));
    }
    if let Some((label, url)) = &a.button {
        act.insert("buttons".into(), json!([{ "label": label, "url": url }]));
    }
    Value::Object(act)
}

fn set_activity(nonce: u64, pid: u32, activity: Option<Value>) -> Value {
    json!({
        "cmd": "SET_ACTIVITY",
        "args": { "pid": pid, "activity": activity.unwrap_or(Value::Null) },
        "nonce": nonce.to_string(),
    })
}

// ---------------------------------------------------------------- conexión

/// Una conexión ya con el apretón hecho.
struct Ipc<S: Read + Write> {
    s: S,
    nonce: u64,
}

impl<S: Read + Write> Ipc<S> {
    fn handshake(mut s: S, client_id: &str) -> std::io::Result<Self> {
        s.write_all(&encode(OP_HANDSHAKE, &json!({ "v": 1, "client_id": client_id })))?;
        let (op, v) = decode(&mut s)?;
        if op == OP_CLOSE || v["evt"] != "READY" {
            let why = v["message"].as_str().unwrap_or("Discord no aceptó la aplicación").to_string();
            return Err(std::io::Error::new(std::io::ErrorKind::ConnectionRefused, why));
        }
        Ok(Self { s, nonce: 0 })
    }

    /// Pone la actividad (o la quita con `None`) y espera la respuesta de Discord, que si
    /// no se lee acaba llenando la tubería.
    fn set(&mut self, activity: Option<Value>) -> std::io::Result<()> {
        self.nonce += 1;
        let msg = set_activity(self.nonce, std::process::id(), activity);
        self.s.write_all(&encode(OP_FRAME, &msg))?;
        let (op, v) = decode(&mut self.s)?;
        if op == OP_CLOSE || v["evt"] == "ERROR" {
            let why = v["data"]["message"].as_str().or(v["message"].as_str()).unwrap_or("error de Discord");
            return Err(std::io::Error::other(why.to_string()));
        }
        Ok(())
    }
}

#[cfg(windows)]
type Stream = std::fs::File;
#[cfg(unix)]
type Stream = std::os::unix::net::UnixStream;

#[cfg(windows)]
fn open(n: u32) -> std::io::Result<Stream> {
    std::fs::OpenOptions::new().read(true).write(true).open(format!(r"\\.\pipe\discord-ipc-{n}"))
}

#[cfg(unix)]
fn open(n: u32) -> std::io::Result<Stream> {
    let mut bases: Vec<std::path::PathBuf> = ["XDG_RUNTIME_DIR", "TMPDIR", "TMP", "TEMP"]
        .iter()
        .filter_map(|k| std::env::var_os(k).map(std::path::PathBuf::from))
        .collect();
    bases.push("/tmp".into());
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "Discord no está abierto");
    for b in bases {
        // Discord de Flatpak y de Snap pone el socket en una subcarpeta.
        for sub in ["", "app/com.discordapp.Discord", "snap.discord"] {
            match std::os::unix::net::UnixStream::connect(b.join(sub).join(format!("discord-ipc-{n}"))) {
                Ok(s) => return Ok(s),
                Err(e) => last = e,
            }
        }
    }
    Err(last)
}

fn connect(client_id: &str) -> std::io::Result<Ipc<Stream>> {
    let mut last = std::io::Error::new(std::io::ErrorKind::NotFound, "Discord no está abierto");
    for n in 0..10 {
        match open(n) {
            Ok(s) => match Ipc::handshake(s, client_id) {
                Ok(i) => return Ok(i),
                Err(e) => last = e,
            },
            Err(e) => last = e,
        }
    }
    Err(last)
}

// ---------------------------------------------------------------- el hilo

/// Lo que tiene que mantener la presencia. Soltarlo la quita.
pub struct Handle {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl Handle {
    /// Quita la presencia y cierra. No espera a Discord: si no contesta, el hilo termina
    /// solo cuando pueda.
    pub fn stop(self) {
        drop(self);
    }
}

impl Drop for Handle {
    fn drop(&mut self) {
        let (m, cv) = &*self.gate;
        *m.lock().unwrap_or_else(|p| p.into_inner()) = true;
        cv.notify_all();
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// Pone `activity` en Discord y la mantiene hasta soltar el [`Handle`]. No bloquea.
pub fn start(client_id: String, activity: Activity) -> Handle {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let g = gate.clone();
    let started = now();
    let body = activity_json(&activity, started);
    let spawned = std::thread::Builder::new().name("dino-presence".into()).spawn(move || {
        let stopped = || *g.0.lock().unwrap_or_else(|p| p.into_inner());
        // Espera `d` o hasta que pidan parar. `true` = hay que parar.
        let nap = |d: Duration| {
            let (m, cv) = &*g;
            let guard = m.lock().unwrap_or_else(|p| p.into_inner());
            let (guard, _) = cv.wait_timeout_while(guard, d, |stop| !*stop).unwrap_or_else(|p| p.into_inner());
            *guard
        };
        let mut warned = false;
        while !stopped() {
            match connect(&client_id) {
                Ok(mut ipc) => {
                    warned = false;
                    tracing::info!("Discord: presencia puesta");
                    loop {
                        if let Err(e) = ipc.set(Some(body.clone())) {
                            tracing::info!(error = %e, "Discord: se perdió la conexión");
                            break;
                        }
                        if nap(BEAT) {
                            let _ = ipc.set(None);
                            tracing::info!("Discord: presencia quitada");
                            return;
                        }
                    }
                }
                Err(e) => {
                    if !warned {
                        tracing::info!(error = %e, "Discord: sin conexión, se reintenta");
                        warned = true;
                    }
                }
            }
            if nap(RETRY) {
                return;
            }
        }
    });
    if let Err(e) = spawned {
        tracing::warn!(error = %e, "Discord: no pude arrancar el hilo");
    }
    Handle { gate }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    /// Un «Discord» de mentira: lo que se escribe queda anotado y lo que se lee está
    /// preparado de antemano.
    struct Fake {
        incoming: Cursor<Vec<u8>>,
        sent: Vec<u8>,
    }
    impl Read for Fake {
        fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
            self.incoming.read(b)
        }
    }
    impl Write for Fake {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.sent.extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    fn fake(frames: &[(u32, Value)]) -> Fake {
        let bytes = frames.iter().flat_map(|(op, v)| encode(*op, v)).collect();
        Fake { incoming: Cursor::new(bytes), sent: Vec::new() }
    }

    #[test]
    fn una_trama_hace_ida_y_vuelta() {
        let v = json!({ "cmd": "x", "n": 1 });
        let (op, back) = decode(&mut Cursor::new(encode(1, &v))).unwrap();
        assert_eq!((op, back), (1, v));
    }

    #[test]
    fn una_longitud_absurda_no_reserva_memoria() {
        let mut b = Vec::new();
        b.extend_from_slice(&1u32.to_le_bytes());
        b.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&mut Cursor::new(b)).is_err());
    }

    #[test]
    fn el_apreton_manda_el_client_id_y_espera_ready() {
        let d = fake(&[(OP_FRAME, json!({ "cmd": "DISPATCH", "evt": "READY" }))]);
        let ipc = Ipc::handshake(d, "123456789012345678").unwrap();
        let (op, hello) = decode(&mut Cursor::new(ipc.s.sent.clone())).unwrap();
        assert_eq!(op, OP_HANDSHAKE);
        assert_eq!((hello["v"].as_u64(), hello["client_id"].as_str()), (Some(1), Some("123456789012345678")));
    }

    #[test]
    fn si_discord_cierra_en_el_apreton_es_un_error() {
        let d = fake(&[(OP_CLOSE, json!({ "code": 4000, "message": "Invalid Client ID" }))]);
        let e = Ipc::handshake(d, "1").err().unwrap();
        assert!(e.to_string().contains("Invalid Client ID"));
    }

    #[test]
    fn poner_y_quitar_la_actividad() {
        let d = fake(&[
            (OP_FRAME, json!({ "cmd": "DISPATCH", "evt": "READY" })),
            (OP_FRAME, json!({ "cmd": "SET_ACTIVITY", "evt": null })),
            (OP_FRAME, json!({ "cmd": "SET_ACTIVITY", "evt": null })),
        ]);
        let mut ipc = Ipc::handshake(d, "1").unwrap();
        ipc.s.sent.clear();
        ipc.set(Some(json!({ "details": "hola" }))).unwrap();
        let (_, v) = decode(&mut Cursor::new(ipc.s.sent.clone())).unwrap();
        assert_eq!(v["cmd"], "SET_ACTIVITY");
        assert_eq!(v["args"]["activity"]["details"], "hola");
        assert_eq!(v["args"]["pid"].as_u64(), Some(std::process::id() as u64));
        ipc.s.sent.clear();
        ipc.set(None).unwrap();
        let (_, v) = decode(&mut Cursor::new(ipc.s.sent.clone())).unwrap();
        assert!(v["args"]["activity"].is_null(), "sin actividad = presencia quitada");
        assert_ne!(v["nonce"], "1", "cada petición lleva su nonce");
    }

    #[test]
    fn un_error_de_discord_se_nota() {
        let d = fake(&[
            (OP_FRAME, json!({ "evt": "READY" })),
            (OP_FRAME, json!({ "evt": "ERROR", "data": { "message": "bad activity" } })),
        ]);
        let mut ipc = Ipc::handshake(d, "1").unwrap();
        assert!(ipc.set(Some(json!({}))).unwrap_err().to_string().contains("bad activity"));
    }

    #[test]
    fn la_actividad_solo_lleva_lo_que_el_pack_dijo() {
        let vacia = activity_json(&Activity::default(), 100);
        assert_eq!(vacia, json!({ "timestamps": { "start": 100 } }), "nada forzado que no se pidió");
        let a = Activity {
            details: Some("Jugando Voidasio".into()),
            state: None,
            image: Some("https://x.io/l.png".into()),
            image_text: None,
            button: Some(("Unirse".into(), "https://x.io/join".into())),
        };
        let v = activity_json(&a, 7);
        assert_eq!(v["assets"]["large_image"], "https://x.io/l.png");
        assert!(v["assets"].get("large_text").is_none());
        assert_eq!(v["buttons"][0]["label"], "Unirse");
        assert!(v.get("state").is_none());
    }

    #[test]
    fn parar_antes_de_conectar_termina_el_hilo() {
        // Un id que ninguna Discord de esta máquina va a aceptar: da igual, lo que se
        // prueba es que soltar el `Handle` no deja nada colgado ni bloquea.
        let h = start("111111111111111111".into(), Activity::default());
        let t = std::time::Instant::now();
        h.stop();
        assert!(t.elapsed() < Duration::from_secs(1));
    }
}
