//! Reglas puras de las cuentas: un solo sitio para ellas.

use std::fmt;

/// Por qué no vale un nombre. La interfaz lo redacta.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NameProblem {
    /// Menos de 3 o más de 16 caracteres.
    Length,
    /// Algo que no es letra, número o guion bajo.
    Characters,
}

impl fmt::Display for NameProblem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            NameProblem::Length => "el nombre debe tener entre 3 y 16 caracteres",
            NameProblem::Characters => "solo letras, números y guion bajo",
        })
    }
}

/// Las reglas de Mojang para un nombre de jugador: el servidor rechaza lo que se salga de
/// ahí, y es mejor decirlo mientras se escribe que al entrar a jugar.
pub fn validate_name(name: &str) -> Result<&str, NameProblem> {
    let n = name.trim();
    let len = n.chars().count();
    if !(3..=16).contains(&len) {
        return Err(NameProblem::Length);
    }
    if !n.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return Err(NameProblem::Characters);
    }
    Ok(n)
}

/// UUID de una cuenta sin conexión: `UUID.nameUUIDFromBytes("OfflinePlayer:<nombre>")` de
/// Java, el mismo que calculan el servidor, el motor y 1.x. **Tiene que coincidir**: es con
/// el que el mundo guarda el inventario y la posición. Por eso cambiar de nombre es ser
/// otro jugador. 32 hex sin guiones.
pub fn offline_uuid(name: &str) -> String {
    let mut d: [u8; 16] = md5(format!("OfflinePlayer:{name}").as_bytes());
    d[6] = (d[6] & 0x0f) | 0x30; // versión 3
    d[8] = (d[8] & 0x3f) | 0x80; // variante IETF
    d.iter().map(|b| format!("{b:02x}")).collect()
}

/// MD5 (lo exige Java para este UUID; no protege nada).
fn md5(input: &[u8]) -> [u8; 16] {
    use md5::{Digest, Md5};
    Md5::digest(input).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn md5_conocido() {
        let hex = |b: [u8; 16]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
        assert_eq!(hex(md5(b"")), "d41d8cd98f00b204e9800998ecf8427e");
        assert_eq!(hex(md5(b"The quick brown fox jumps over the lazy dog")), "9e107d9d372bb6826bd81d3542a419d6");
    }

    #[test]
    fn el_uuid_es_el_mismo_que_calcula_minecraft() {
        // Mismo valor que comprueban 1.x (auth.rs) y dino-minecraft.
        assert_eq!(offline_uuid("Notch"), "b50ad385829d3141a2167e7d7539ba7f");
        let u = offline_uuid("Kevs");
        assert_eq!(&u[12..13], "3");
        assert!(matches!(&u[16..17], "8" | "9" | "a" | "b"));
    }

    #[test]
    fn el_nombre_se_valida_como_lo_valida_mojang() {
        assert_eq!(validate_name("  Steve_99  "), Ok("Steve_99"));
        assert_eq!(validate_name("ab"), Err(NameProblem::Length));
        assert_eq!(validate_name("abcdefghijklmnopq"), Err(NameProblem::Length));
        assert_eq!(validate_name("José"), Err(NameProblem::Characters));
        assert_eq!(validate_name("con espacio"), Err(NameProblem::Characters));
    }
}
