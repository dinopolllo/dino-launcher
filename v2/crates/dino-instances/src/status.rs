//! Estado de un servidor de Minecraft: versión, jugadores y latencia.
//!
//! Implementa el *server list ping*, el mismo que usa el juego para pintar la
//! lista de servidores. Incluye la resolución SRV, sin la cual una dirección como
//! `mcpvp.club` no responde: su registro A apunta al sitio web, no al servidor.

use serde::Serialize;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

/// Versión de protocolo que decimos hablar. En un *ping* de estado da igual cuál
/// sea: el servidor responde igual y nos dice la suya.
const PROTOCOL: i32 = 767;
const TIMEOUT: Duration = Duration::from_secs(6);

#[derive(Debug, Serialize)]
pub struct ServerStatus {
    pub online: bool,
    pub version: Option<String>,
    pub players: Option<u32>,
    pub max_players: Option<u32>,
    pub ping_ms: Option<u64>,
    pub motd: Option<String>,
}

impl ServerStatus {
    pub fn offline() -> Self {
        Self { online: false, version: None, players: None, max_players: None, ping_ms: None, motd: None }
    }
}

fn varint(mut n: i32) -> Vec<u8> {
    let mut out = Vec::new();
    loop {
        let mut b = (n & 0x7F) as u8;
        n = ((n as u32) >> 7) as i32;
        if n != 0 {
            b |= 0x80;
        }
        out.push(b);
        if n == 0 {
            return out;
        }
    }
}

async fn read_varint(s: &mut TcpStream) -> std::io::Result<i32> {
    let (mut n, mut shift) = (0i32, 0u32);
    loop {
        let b = s.read_u8().await?;
        n |= ((b & 0x7F) as i32) << shift;
        if b & 0x80 == 0 {
            return Ok(n);
        }
        shift += 7;
        if shift > 35 {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidData, "varint desbordado"));
        }
    }
}

/// Resuelve `_minecraft._tcp.<host>`. Los servidores grandes lo usan para mover el
/// puerto y para que el dominio web viva en otra IP.
async fn resolve_srv(host: &str) -> Option<(String, u16)> {
    use hickory_resolver::config::{NameServerConfigGroup, ResolverConfig};
    use hickory_resolver::TokioAsyncResolver;
    let (conf, mut opts) = hickory_resolver::system_conf::read_system_conf().ok()?;
    // Windows anuncia `fec0:0:0:ffff::1..3` (DNS «site-local», obsoletos) en adaptadores sin
    // DNS IPv6 propio, como ZeroTier o el loopback. Nadie contesta ahí y cada consulta se
    // comía el plazo entero: 5 s de espera para un ping de 40 ms.
    let servers: Vec<_> = conf.name_servers().iter().filter(|n| usable_dns(n.socket_addr.ip())).cloned().collect();
    let conf = ResolverConfig::from_parts(
        conf.domain().cloned(),
        conf.search().to_vec(),
        NameServerConfigGroup::from(servers),
    );
    opts.timeout = Duration::from_millis(1500);
    opts.attempts = 1;
    let resolver = TokioAsyncResolver::tokio(conf, opts);
    let lookup = resolver.srv_lookup(format!("_minecraft._tcp.{host}.")).await.ok()?;
    let rec = lookup.iter().next()?;
    Some((rec.target().to_utf8().trim_end_matches('.').to_string(), rec.port()))
}

fn usable_dns(ip: std::net::IpAddr) -> bool {
    match ip {
        std::net::IpAddr::V6(v6) => {
            let seg = v6.segments()[0];
            seg & 0xffc0 != 0xfec0 && seg & 0xffc0 != 0xfe80 // site-local y link-local
        }
        std::net::IpAddr::V4(_) => true,
    }
}

/// Limpia los códigos de color (`§a`) y aplana el MOTD, que puede venir como
/// texto plano o como árbol de componentes JSON.
fn flatten_motd(v: &serde_json::Value) -> String {
    // El limpiado va **una sola vez**, al final: hacerlo en cada nivel se come
    // los espacios de los bordes y pega las palabras entre componentes.
    strip_colors(&flatten_raw(v))
}

fn flatten_raw(v: &serde_json::Value) -> String {
    let mut out = String::new();
    match v {
        serde_json::Value::String(s) => out.push_str(s),
        serde_json::Value::Object(o) => {
            if let Some(serde_json::Value::String(t)) = o.get("text") {
                out.push_str(t);
            }
            if let Some(serde_json::Value::Array(extra)) = o.get("extra") {
                for e in extra {
                    out.push_str(&flatten_raw(e));
                }
            }
        }
        serde_json::Value::Array(a) => {
            for e in a {
                out.push_str(&flatten_raw(e));
            }
        }
        _ => {}
    }
    out
}

fn strip_colors(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '§' {
            chars.next(); // el código va pegado al símbolo
        } else {
            out.push(c);
        }
    }
    out.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Separa `host:puerto`. Sin puerto explícito, se intenta SRV y luego el 25565.
fn split_addr(addr: &str) -> (String, Option<u16>) {
    match addr.rsplit_once(':') {
        Some((h, p)) => match p.parse() {
            Ok(port) => (h.to_string(), Some(port)),
            Err(_) => (addr.to_string(), None),
        },
        None => (addr.to_string(), None),
    }
}

pub async fn ping(addr: &str) -> ServerStatus {
    match tokio::time::timeout(TIMEOUT, ping_inner(addr)).await {
        Ok(Some(s)) => s,
        _ => ServerStatus::offline(),
    }
}

async fn ping_inner(addr: &str) -> Option<ServerStatus> {
    let (host, explicit_port) = split_addr(addr);

    // Un puerto explícito manda; si no, SRV; si tampoco, el de siempre.
    let (target, port) = match explicit_port {
        Some(p) => (host.clone(), p),
        None => resolve_srv(&host).await.unwrap_or((host.clone(), 25565)),
    };

    let started = Instant::now();
    let mut s = TcpStream::connect((target.as_str(), port)).await.ok()?;
    let ping_ms = started.elapsed().as_millis() as u64;

    // Handshake: el `host` que se envía es el que el jugador escribió, no el
    // resuelto — los proxies enrutan por ese nombre.
    let mut hs = vec![0x00];
    hs.extend(varint(PROTOCOL));
    hs.extend(varint(host.len() as i32));
    hs.extend(host.as_bytes());
    hs.extend(port.to_be_bytes());
    hs.extend(varint(1)); // 1 = pedir estado
    let mut packet = varint(hs.len() as i32);
    packet.extend(hs);
    s.write_all(&packet).await.ok()?;
    s.write_all(&[0x01, 0x00]).await.ok()?; // petición de estado, vacía

    let _len = read_varint(&mut s).await.ok()?;
    let _id = read_varint(&mut s).await.ok()?;
    let json_len = read_varint(&mut s).await.ok()?;
    if !(0..=2_000_000).contains(&json_len) {
        return None;
    }
    let mut buf = vec![0u8; json_len as usize];
    s.read_exact(&mut buf).await.ok()?;

    let v: serde_json::Value = serde_json::from_slice(&buf).ok()?;
    Some(ServerStatus {
        online: true,
        version: v["version"]["name"].as_str().map(strip_colors),
        players: v["players"]["online"].as_u64().map(|n| n as u32),
        max_players: v["players"]["max"].as_u64().map(|n| n as u32),
        ping_ms: Some(ping_ms),
        motd: Some(flatten_motd(&v["description"])).filter(|s| !s.is_empty()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn los_dns_site_local_se_descartan() {
        assert!(!usable_dns("fec0:0:0:ffff::1".parse().unwrap()));
        assert!(!usable_dns("fe80::1".parse().unwrap()));
        assert!(usable_dns("2806:1000:ffff:4::e".parse().unwrap()));
        assert!(usable_dns("192.168.1.254".parse().unwrap()));
    }

    #[test]
    fn varint_codifica_como_el_protocolo() {
        assert_eq!(varint(0), vec![0x00]);
        assert_eq!(varint(1), vec![0x01]);
        assert_eq!(varint(127), vec![0x7F]);
        assert_eq!(varint(128), vec![0x80, 0x01]);
        assert_eq!(varint(767), vec![0xFF, 0x05]);
    }

    #[test]
    fn el_puerto_explicito_manda_sobre_srv() {
        assert_eq!(split_addr("mc.ejemplo.com:25566"), ("mc.ejemplo.com".into(), Some(25566)));
        assert_eq!(split_addr("mcpvp.club"), ("mcpvp.club".into(), None));
        // IPv6 sin corchetes no es una dirección válida de Minecraft: no la partimos mal.
        assert_eq!(split_addr("mc.ejemplo.com:abc"), ("mc.ejemplo.com:abc".into(), None));
    }

    /// Toca la red: `cargo test -- --ignored ping_real`.
    #[tokio::test]
    #[ignore]
    async fn ping_real_resolviendo_srv() {
        // mcpvp.club solo responde vía SRV: su registro A es el sitio web.
        let s = ping("mcpvp.club").await;
        println!("  {:?}", s);
        assert!(s.online, "no respondió; ¿SRV roto?");
        assert!(s.players.unwrap_or(0) > 0);
        assert!(s.version.is_some());
    }

    #[test]
    fn el_motd_pierde_colores_y_estructura() {
        // Los componentes se concatenan tal cual: los espacios vienen en el texto.
        let v = serde_json::json!({
            "text": "§aDonut ",
            "extra": [{ "text": "§fSMP " }, { "text": "1.21.2+" }]
        });
        assert_eq!(flatten_motd(&v), "Donut SMP 1.21.2+");
        assert_eq!(flatten_motd(&serde_json::json!("§cHola\n  mundo")), "Hola mundo");
    }
}
