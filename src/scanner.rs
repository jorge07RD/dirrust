//! Escaneo de directorios en paralelo y NO bloqueante.
//!
//! El escaneo corre en un hilo de fondo con `jwalk` (recorrido paralelo) y se
//! comunica con el hilo de UI mediante un `crossbeam-channel`. La UI nunca se
//! bloquea: drena el canal en cada iteración de su bucle de eventos.
//!
//! REVISAR: concurrencia y canales. El productor (este hilo) envía mensajes de
//! progreso periódicos y, al terminar, un único `Done` con el árbol construido.
//! Si el receptor se cae (UI cerrada), los `send` fallan y abortamos el escaneo
//! limpiamente en lugar de hacer panic.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use crossbeam_channel::{Receiver, Sender};
use jwalk::WalkDirGeneric;

use crate::model::{extension_of, Tree};

/// Cómo medimos el tamaño de cada archivo.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeMode {
    /// Tamaño aparente: el `len()` lógico del archivo (lo que "dice" pesar).
    Apparent,
    /// Tamaño en disco: bloques realmente ocupados (`st_blocks * 512`). Tiene
    /// en cuenta el redondeo a bloques y los archivos dispersos (sparse).
    Disk,
}

/// Configuración del escaneo, derivada de la CLI.
#[derive(Debug, Clone)]
pub struct ScanConfig {
    pub root: PathBuf,
    pub follow_symlinks: bool,
    pub one_file_system: bool,
    pub size_mode: SizeMode,
    /// 0 = dejar que jwalk elija según los núcleos disponibles.
    pub threads: usize,
}

/// Mensajes que el escáner envía a la UI por el canal.
#[derive(Debug)]
pub enum ScanMsg {
    /// Progreso parcial mientras se escanea (solo cifras).
    Progress { files: u64, bytes: u64 },
    /// Snapshot parcial del árbol para mostrar resultados EN VIVO durante el
    /// escaneo. Se envía con baja frecuencia (throttled) por su coste.
    Partial {
        tree: Box<Tree>,
        files: u64,
        bytes: u64,
    },
    /// Escaneo terminado con éxito.
    Done {
        tree: Box<Tree>,
        /// Cuántas entradas se omitieron (p. ej. por falta de permisos).
        skipped: u64,
    },
    /// Error fatal antes de poder construir nada (ruta inexistente, etc.).
    Error(String),
}

/// Cada cuánto, como mucho, enviamos un snapshot parcial del árbol a la UI.
/// REVISAR (concurrencia): el snapshot clona + agrega el árbol, así que es O(n);
/// lo limitamos por TIEMPO para acotar su coste a medida que el árbol crece.
const SNAPSHOT_CADA: std::time::Duration = std::time::Duration::from_millis(400);

/// Lanza el escaneo en un hilo de fondo y devuelve el extremo receptor del canal.
///
/// El hilo se queda detached: cuando termina envía `Done`/`Error` y muere. Si la
/// UI cierra el receptor, los `send` fallan y el hilo aborta por sí solo.
pub fn spawn(config: ScanConfig) -> Receiver<ScanMsg> {
    // Canal con buffer acotado: si la UI va lenta drenando, el escáner se frena
    // un poco en lugar de acumular memoria sin límite.
    let (tx, rx) = crossbeam_channel::bounded(256);
    std::thread::Builder::new()
        .name("dirrust-scanner".into())
        .spawn(move || {
            run_scan(config, tx);
        })
        // El spawn de un hilo solo falla en condiciones extremas del SO; si
        // ocurre no hay UI posible, así que es un panic justificado en setup.
        .expect("no se pudo crear el hilo de escaneo");
    rx
}

/// Cuerpo del escaneo (ejecutado ya en el hilo de fondo).
fn run_scan(config: ScanConfig, tx: Sender<ScanMsg>) {
    // Validamos la raíz antes de nada para dar un error claro.
    let root_meta = match std::fs::symlink_metadata(&config.root) {
        Ok(m) => m,
        Err(e) => {
            let _ = tx.send(ScanMsg::Error(format!(
                "no se puede acceder a '{}': {e}",
                config.root.display()
            )));
            return;
        }
    };

    // Dispositivo de la raíz: necesario para `--one-file-system` (no cruzar
    // montajes). Comparamos el `st_dev` de cada hijo contra este valor.
    let root_dev = device_id(&root_meta);

    // Contador de entradas omitidas, compartido con el closure de poda de jwalk.
    let skipped = Arc::new(AtomicU64::new(0));

    // Construimos el árbol. Si la raíz es un archivo suelto, el árbol tiene un
    // único nodo; si es un directorio, jwalk lo recorre.
    let is_dir = root_meta.is_dir();
    let root_own = own_size(&root_meta, config.size_mode);
    let mut tree = Tree::with_root(&config.root, is_dir, if is_dir { 0 } else { root_own });

    if is_dir {
        build_tree(&config, &mut tree, root_dev, &skipped, &tx);
    }

    // Agregación bottom-up una sola vez (ver model::Tree::aggregate).
    tree.aggregate();

    let _ = tx.send(ScanMsg::Done {
        tree: Box::new(tree),
        skipped: skipped.load(Ordering::Relaxed),
    });
}

/// Recorre el directorio raíz con jwalk y va poblando el arena.
fn build_tree(
    config: &ScanConfig,
    tree: &mut Tree,
    root_dev: u64,
    skipped: &Arc<AtomicU64>,
    tx: &Sender<ScanMsg>,
) {
    // Mapa ruta -> índice de nodo. jwalk garantiza que un directorio se anuncia
    // ANTES que su contenido (no puede listar hijos sin abrir el directorio),
    // por lo que el padre siempre existe en el mapa cuando llega un hijo.
    let mut index: std::collections::HashMap<PathBuf, usize> = std::collections::HashMap::new();
    index.insert(config.root.clone(), tree.root);

    // Acumuladores para el progreso; emitimos cada `PROGRESS_EVERY` archivos.
    const PROGRESS_EVERY: u64 = 4096;
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    let mut since_last: u64 = 0;
    // Marca temporal del último snapshot parcial enviado.
    let mut last_snapshot = std::time::Instant::now();

    let size_mode = config.size_mode;
    let one_fs = config.one_file_system;
    let skipped_walk = Arc::clone(skipped);

    // REVISAR: concurrencia / poda. Usamos `process_read_dir` para podar en el
    // propio jwalk (antes de descender) los subárboles que cruzan a otro sistema
    // de archivos cuando `--one-file-system` está activo, y para contar las
    // lecturas de directorio que fallan por permisos. Podar aquí evita gastar
    // hilos descendiendo en ramas que vamos a descartar.
    let walk = WalkDirGeneric::<((), ())>::new(&config.root)
        .follow_links(config.follow_symlinks)
        .skip_hidden(false)
        .parallelism(parallelism(config.threads))
        .process_read_dir(move |_depth, _path, _state, children| {
            for child in children.iter() {
                // Una entrada `Err` aquí significa que no pudimos leer algo de
                // este directorio (típicamente permisos): lo contamos.
                if child.is_err() {
                    skipped_walk.fetch_add(1, Ordering::Relaxed);
                }
            }
            if one_fs {
                children.retain(|res| match res {
                    Ok(entry) => {
                        // Solo podamos directorios en otro dispositivo; los
                        // archivos sueltos no inician descenso de todos modos.
                        if entry.file_type.is_dir() {
                            match entry.metadata() {
                                Ok(m) => device_id(&m) == root_dev,
                                // Si no podemos leer metadata, conservamos la
                                // entrada y dejamos que el bucle principal decida.
                                Err(_) => true,
                            }
                        } else {
                            true
                        }
                    }
                    Err(_) => true, // los errores ya se contaron arriba
                });
            }
        });

    for entry in walk {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                // Error al producir esta entrada (permiso al hacer stat, etc.).
                skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };

        let path = entry.path();
        // La raíz ya está en el árbol; jwalk también la emite con depth 0.
        if path == config.root {
            continue;
        }

        let is_dir = entry.file_type.is_dir();

        // El padre debe existir ya en el mapa por el orden de jwalk. Si por
        // algún motivo no está (rama podada, symlink raro), omitimos la entrada.
        let parent_path = entry.parent_path();
        let Some(&parent_idx) = index.get(parent_path) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        // Tamaño propio: solo los archivos aportan bytes; los directorios suman 0
        // (su tamaño se agrega desde los hijos).
        let own = if is_dir {
            0
        } else {
            match entry.metadata() {
                Ok(m) => own_size(&m, size_mode),
                Err(_) => {
                    skipped.fetch_add(1, Ordering::Relaxed);
                    0
                }
            }
        };

        let name = entry.file_name().to_string_lossy().into_owned();
        let ext = extension_of(&path, is_dir);
        let idx = tree.add_child(parent_idx, name, is_dir, own, ext);

        if is_dir {
            index.insert(path, idx);
        } else {
            files += 1;
            bytes += own;
            since_last += 1;
            if since_last >= PROGRESS_EVERY {
                since_last = 0;
                // Si la UI cerró el canal, dejamos de escanear.
                if tx.send(ScanMsg::Progress { files, bytes }).is_err() {
                    return;
                }
                // Snapshot parcial throttled por tiempo: clonamos el árbol,
                // lo agregamos y lo enviamos para el refresco en vivo.
                if last_snapshot.elapsed() >= SNAPSHOT_CADA {
                    last_snapshot = std::time::Instant::now();
                    let mut snap = tree.clone();
                    snap.aggregate();
                    if tx
                        .send(ScanMsg::Partial {
                            tree: Box::new(snap),
                            files,
                            bytes,
                        })
                        .is_err()
                    {
                        return;
                    }
                }
            }
        }
    }

    // Un último progreso con las cifras finales antes del `Done`.
    let _ = tx.send(ScanMsg::Progress { files, bytes });
}

/// Convierte el número de hilos de la CLI al ajuste de jwalk.
fn parallelism(threads: usize) -> jwalk::Parallelism {
    match threads {
        // 0 = pool por defecto de rayon (escala con los núcleos disponibles).
        0 => jwalk::Parallelism::RayonDefaultPool {
            busy_timeout: std::time::Duration::from_secs(1),
        },
        // jwalk crea su propio pool con el número de hilos pedido; así no
        // necesitamos depender de `rayon` directamente todavía.
        n => jwalk::Parallelism::RayonNewPool(n),
    }
}

/// Tamaño propio de una entrada según el modo (aparente vs en disco).
fn own_size(meta: &std::fs::Metadata, mode: SizeMode) -> u64 {
    use std::os::unix::fs::MetadataExt;
    match mode {
        SizeMode::Apparent => meta.len(),
        // st_blocks cuenta bloques de 512 bytes por convención de POSIX,
        // independientemente del tamaño de bloque del sistema de archivos.
        SizeMode::Disk => meta.blocks() * 512,
    }
}

/// Identificador de dispositivo (st_dev) de una entrada.
fn device_id(meta: &std::fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.dev()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Crea un fixture con tamaños conocidos y comprueba que el escáner y la
    /// agregación bottom-up producen el total y el conteo correctos.
    #[test]
    fn escaneo_y_agregacion_dan_totales_correctos() {
        // Árbol de prueba:
        //   raíz/
        //     a.txt        (100 bytes)
        //     sub/
        //       b.bin      (200 bytes)
        //       c.bin      (300 bytes)
        //       vacia/     (directorio sin archivos)
        // Total esperado: 600 bytes, 3 archivos.
        let dir = std::env::temp_dir().join(format!("dirrust_test_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub").join("vacia")).unwrap();
        write_bytes(&dir.join("a.txt"), 100);
        write_bytes(&dir.join("sub").join("b.bin"), 200);
        write_bytes(&dir.join("sub").join("c.bin"), 300);

        let config = ScanConfig {
            root: dir.clone(),
            follow_symlinks: false,
            one_file_system: true,
            size_mode: SizeMode::Apparent,
            threads: 0,
        };

        let rx = spawn(config);
        let tree = loop {
            match rx.recv().unwrap() {
                ScanMsg::Done { tree, .. } => break tree,
                ScanMsg::Error(e) => panic!("escaneo falló: {e}"),
                ScanMsg::Progress { .. } | ScanMsg::Partial { .. } => continue,
            }
        };

        assert_eq!(tree.total_size(), 600, "el tamaño agregado debe ser 600");
        assert_eq!(tree.total_files(), 3, "deben contarse 3 archivos");

        let _ = std::fs::remove_dir_all(&dir);
    }

    fn write_bytes(path: &std::path::Path, n: usize) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(&vec![b'x'; n]).unwrap();
    }
}
