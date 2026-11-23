//! Errores con código estable.
//!
//! En 1.x cada comando devolvía `Result<_, String>` y la interfaz enseñaba el texto tal
//! cual («error de red: …», «el zip está corrupto: …»). En 2.0 la interfaz recibe un
//! **código** y decide ella cómo decirlo y qué botón ofrecer; el `detail` técnico va al
//! registro y al informe de diagnóstico, no a la cara del jugador.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// Sin conexión, o un servidor que no responde.
    Offline,
    /// Una descarga falló tras sus reintentos.
    DownloadFailed,
    /// Llegó un archivo distinto del esperado.
    HashMismatch,
    /// No cabe en el disco.
    DiskFull,
    /// No se puede escribir en la carpeta de datos.
    DiskPermission,
    /// Ningún pack del catálogo tiene ese código.
    PackNotFound,
    /// El `.mrpack` está roto o trae rutas inseguras.
    PackInvalid,
    /// El pack usa un loader fuera de alcance (Forge, Quilt).
    UnsupportedLoader,
    /// No hay Java para esta versión y no se pudo descargar.
    JavaUnavailable,
    /// No hay ninguna cuenta.
    AccountRequired,
    /// La sesión de Microsoft caducó y el motor no pudo renovarla.
    SessionExpired,
    /// Mojang rechazó la sesión (cuenta sin Minecraft, sesión revocada).
    MicrosoftRejected,
    /// El juego se cerró solo antes de llegar al menú.
    GameCrashed,
    /// Ya hay una partida o una instalación en marcha.
    Busy,
    /// No se pudo instalar la actualización del launcher.
    UpdateFailed,
    /// Todo lo demás. Siempre lleva `detail` y siempre se registra.
    Internal,
}

/// Lo que la interfaz puede ofrecer como botón junto al mensaje.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    Retry,
    FreeSpace,
    ChooseFolder,
    SignIn,
    RenewSession,
    OpenLogs,
    None,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{code:?}: {detail}")]
pub struct DinoError {
    pub code: ErrorCode,
    /// ¿Tiene sentido volver a intentarlo tal cual?
    pub retryable: bool,
    pub action: Action,
    /// Detalle técnico para el registro. Nunca lleva tokens ni rutas con el nombre de
    /// usuario de otra máquina; la interfaz no lo enseña salvo en «Detalles».
    pub detail: String,
}

impl DinoError {
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        let (retryable, action) = match code {
            ErrorCode::Offline | ErrorCode::DownloadFailed | ErrorCode::HashMismatch => (true, Action::Retry),
            ErrorCode::DiskFull => (true, Action::FreeSpace),
            ErrorCode::DiskPermission => (false, Action::ChooseFolder),
            ErrorCode::AccountRequired | ErrorCode::MicrosoftRejected => (false, Action::SignIn),
            ErrorCode::SessionExpired => (true, Action::RenewSession),
            ErrorCode::GameCrashed | ErrorCode::Internal | ErrorCode::UpdateFailed | ErrorCode::JavaUnavailable => {
                (true, Action::OpenLogs)
            }
            ErrorCode::Busy => (true, Action::None),
            ErrorCode::PackNotFound | ErrorCode::PackInvalid | ErrorCode::UnsupportedLoader => (false, Action::None),
        };
        Self { code, retryable, action, detail: detail.into() }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn la_interfaz_recibe_un_codigo_estable() {
        let e = DinoError::new(ErrorCode::DiskFull, "faltan 1,2 GB en C:");
        let v = serde_json::to_value(&e).unwrap();
        assert_eq!(v["code"], "disk_full");
        assert_eq!(v["action"], "free_space");
        assert_eq!(v["retryable"], true);
    }

    #[test]
    fn un_pack_que_no_existe_no_se_reintenta() {
        assert!(!DinoError::new(ErrorCode::PackNotFound, "").retryable);
    }
}
