//! Acciones sobre el sistema de archivos: borrado y guardas de seguridad.
//!
//! REVISAR (borrado): todo lo que toca el disco vive aquí, aislado y comentado.
//! La política es "seguridad primero": nunca se borra sin confirmación (eso lo
//! garantiza el flujo de modales en `app`/`input`), y además este módulo RECHAZA
//! por su cuenta las rutas peligrosas mediante `is_protected`.

use std::path::Path;

use anyhow::{anyhow, Result};

/// Cómo se borra un elemento.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteMode {
    /// Enviar a la papelera del sistema (XDG Trash). Reversible.
    Trash,
    /// Borrado permanente e irreversible.
    Permanent,
}

/// Lista de rutas críticas del sistema que NUNCA se deben borrar.
///
/// REVISAR (guardas de seguridad): esta es la lista configurable de rutas
/// protegidas. Se complementa con `$HOME`, los directorios de primer nivel y la
/// detección de puntos de montaje en `is_protected`.
const RUTAS_CRITICAS: &[&str] = &[
    "/", "/home", "/root", "/boot", "/etc", "/usr", "/bin", "/sbin", "/lib", "/lib64", "/var",
    "/sys", "/proc", "/dev", "/run", "/opt", "/srv", "/mnt", "/media", "/tmp",
];

/// ¿Es `path` una ruta protegida que no debe borrarse?
///
/// REVISAR (guardas de seguridad). Reglas (cualquiera que se cumpla protege):
///   1. Está en la lista de rutas críticas del sistema.
///   2. Es exactamente el `$HOME` del usuario.
///   3. Es un directorio de PRIMER NIVEL bajo "/" (p. ej. `/algo`).
///   4. Es un punto de montaje (su dispositivo difiere del de su padre).
pub fn is_protected(path: &Path) -> bool {
    // 1) Rutas críticas exactas.
    if let Some(s) = path.to_str() {
        if RUTAS_CRITICAS.contains(&s) {
            return true;
        }
    }

    // 2) El HOME del usuario (la raíz, no sus subcarpetas).
    if let Ok(home) = std::env::var("HOME") {
        if !home.is_empty() && path == Path::new(&home) {
            return true;
        }
    }

    // 3) Directorio de primer nivel: "/" tiene 1 componente (la raíz) y "/algo"
    //    tiene 2. Borrar algo tan cerca de la raíz es casi siempre un accidente.
    if path.is_absolute() && path.components().count() <= 2 {
        return true;
    }

    // 4) Punto de montaje: borrarlo afectaría a otro sistema de archivos montado.
    if es_punto_de_montaje(path) {
        return true;
    }

    false
}

/// ¿Es `path` la raíz de un punto de montaje? Lo detectamos comparando el
/// dispositivo (`st_dev`) de la ruta con el de su directorio padre.
fn es_punto_de_montaje(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return false;
    };
    let Some(parent) = path.parent() else {
        // Sin padre = es la raíz "/", ya protegida; lo tratamos como montaje.
        return true;
    };
    match std::fs::symlink_metadata(parent) {
        Ok(pm) => meta.dev() != pm.dev(),
        Err(_) => false,
    }
}

/// Borra `path` según el modo. `is_dir` indica si el nodo es un directorio
/// (para elegir entre `remove_dir_all` y `remove_file` en el borrado permanente).
///
/// REVISAR (borrado): este es el ÚNICO punto que efectúa el borrado real. El
/// llamador es responsable de haber pedido confirmación y de comprobar las
/// guardas con `is_protected` antes de invocar esta función.
pub fn delete_path(path: &Path, is_dir: bool, mode: DeleteMode) -> Result<()> {
    // Cinturón de seguridad redundante: aunque la UI ya lo comprueba, nos
    // negamos a borrar una ruta protegida pase lo que pase.
    if is_protected(path) {
        return Err(anyhow!(
            "ruta protegida, borrado rechazado: {}",
            path.display()
        ));
    }

    match mode {
        DeleteMode::Trash => {
            // El crate `trash` usa la papelera XDG en Linux (reversible).
            trash::delete(path).map_err(|e| anyhow!("no se pudo enviar a la papelera: {e}"))
        }
        DeleteMode::Permanent => {
            if is_dir {
                std::fs::remove_dir_all(path)
                    .map_err(|e| anyhow!("no se pudo borrar el directorio: {e}"))
            } else {
                std::fs::remove_file(path).map_err(|e| anyhow!("no se pudo borrar el archivo: {e}"))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn rutas_criticas_protegidas() {
        assert!(is_protected(Path::new("/")));
        assert!(is_protected(Path::new("/home")));
        assert!(is_protected(Path::new("/etc")));
        // Directorio de primer nivel arbitrario.
        assert!(is_protected(Path::new("/cualquiercosa")));
    }

    #[test]
    fn home_protegido_pero_subcarpetas_no() {
        // Forzamos un HOME conocido para la prueba.
        std::env::set_var("HOME", "/home/usuarioprueba");
        assert!(is_protected(Path::new("/home/usuarioprueba")));
        // Una subcarpeta profunda del home NO está protegida por estas reglas.
        let sub: PathBuf = "/home/usuarioprueba/proyectos/x/y".into();
        assert!(!is_protected(&sub));
    }
}
