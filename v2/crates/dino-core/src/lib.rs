//! Contratos de Dino 2.0.
//!
//! Todo lo que cruza una frontera vive aquí y en ningún otro sitio:
//! - [`schema`]: lo que se guarda en disco (con versión de esquema y escritura atómica);
//! - [`events`]: lo que el núcleo le cuenta a la interfaz sin que ella pregunte;
//! - [`error`]: errores con código estable, para que la interfaz redacte el mensaje;
//! - [`api`]: lo que la interfaz le pide al núcleo y lo que recibe;
//! - [`migrate`]: cómo se convierten los datos de 1.x sin perder nada.
//!
//! Ver `v2/docs/contratos.md` para la lista completa de comandos.

pub mod api;
pub mod error;
pub mod events;
pub mod migrate;
pub mod schema;

pub use error::{Action, DinoError, ErrorCode};
