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

use crate::model::Tree;

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

/// Intervalo MÍNIMO entre snapshots parciales del árbol enviados a la UI.
/// REVISAR (concurrencia): un snapshot es una copia O(n) del árbol. Además de
/// este mínimo, esperamos `SNAPSHOT_FACTOR` veces lo que costó la última copia,
/// de modo que copiar nunca se lleve más de ~1/SNAPSHOT_FACTOR del escaneo
/// aunque el árbol tenga millones de nodos.
const SNAPSHOT_MIN: std::time::Duration = std::time::Duration::from_millis(400);
/// Proporción mínima entre tiempo escaneando y tiempo copiando snapshots.
const SNAPSHOT_FACTOR: u32 = 10;

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
    // Metadata de la raíz: para saber si es un directorio y su dispositivo.
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

    // Dispositivo de la raíz, para --one-file-system.
    let root_dev = device_id(&root_meta);

    // Contador compartido de entradas omitidas (errores de permisos, etc.).
    let skipped = Arc::new(AtomicU64::new(0));

    let is_dir = root_meta.is_dir();
    let root_own = own_size(&root_meta, config.size_mode);
    let mut tree = Tree::with_root(&config.root, is_dir, if is_dir { 0 } else { root_own });

    // Si la UI cerró el canal, abortamos sin más.
    if is_dir && !build_tree(&config, &mut tree, root_dev, &skipped, &tx) {
        return;
    }

    // El árbol ya está agregado (agregación incremental): solo compactamos.
    tree.shrink();

    let _ = tx.send(ScanMsg::Done {
        tree: Box::new(tree),
        skipped: skipped.load(Ordering::Relaxed),
    });
}

/// Estado por entrada que calculan los hilos de jwalk: el tamaño propio del
/// archivo (0 en directorios o si su metadata falló).
type Walk = WalkDirGeneric<((), u64)>;

/// Recorre el directorio raíz con jwalk y va poblando el arena. Devuelve
/// `false` si la UI cerró el canal (hay que abortar).
///
/// REVISAR (rendimiento), las tres claves de la velocidad:
///   1. La metadata (`stat`) de cada archivo se lee en `process_read_dir`, que
///      jwalk ejecuta EN PARALELO en sus hilos; este hilo solo recibe el
///      tamaño ya calculado en `client_state`.
///   2. jwalk entrega las entradas en orden PRIMERO EN PROFUNDIDAD (preorden),
///      así que el padre de una entrada de profundidad `d` es el último
///      directorio visto a profundidad `d - 1`: basta una pila, sin construir
///      ni hashear la ruta completa de cada entrada.
///   3. El tamaño se suma a los ancestros al insertar (agregación incremental),
///      así el árbol siempre está agregado y un snapshot es una copia plana.
fn build_tree(
    config: &ScanConfig,
    tree: &mut Tree,
    root_dev: u64,
    skipped: &Arc<AtomicU64>,
    tx: &Sender<ScanMsg>,
) -> bool {
    // pila[d] = índice del último directorio visto a profundidad d.
    let mut pila: Vec<usize> = vec![tree.root];

    // Cada cuántos archivos enviamos progreso (y valoramos un snapshot).
    const PROGRESS_EVERY: u64 = 4096;
    let mut files: u64 = 0;
    let mut bytes: u64 = 0;
    let mut since_last: u64 = 0;
    let mut last_snapshot = std::time::Instant::now();
    let mut espera_snapshot = SNAPSHOT_MIN;

    let size_mode = config.size_mode;
    let one_fs = config.one_file_system;
    let skipped_walk = Arc::clone(skipped);

    let walk = Walk::new(&config.root)
        .follow_links(config.follow_symlinks)
        .skip_hidden(false)
        .parallelism(parallelism(config.threads))
        .process_read_dir(move |_depth, _path, _state, children| {
            // Se ejecuta en los hilos de jwalk, una vez por directorio leído.
            children.retain_mut(|res| {
                let entry = match res {
                    Ok(e) => e,
                    Err(_) => {
                        skipped_walk.fetch_add(1, Ordering::Relaxed);
                        return false;
                    }
                };
                if entry.file_type.is_dir() {
                    // --one-file-system: no descendemos a otros dispositivos.
                    if one_fs {
                        if let Ok(m) = entry.metadata() {
                            return device_id(&m) == root_dev;
                        }
                    }
                } else {
                    match entry.metadata() {
                        Ok(m) => entry.client_state = own_size(&m, size_mode),
                        Err(_) => {
                            skipped_walk.fetch_add(1, Ordering::Relaxed);
                        }
                    }
                }
                true
            });
        });

    for entry in walk {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => {
                skipped.fetch_add(1, Ordering::Relaxed);
                continue;
            }
        };
        // jwalk devuelve también la raíz (profundidad 0); ya está en el árbol.
        let depth = entry.depth;
        if depth == 0 {
            continue;
        }

        // Padre = último directorio visto a profundidad depth - 1.
        let Some(&parent_idx) = pila.get(depth - 1) else {
            skipped.fetch_add(1, Ordering::Relaxed);
            continue;
        };

        let is_dir = entry.file_type.is_dir();
        let own = entry.client_state;
        let name = entry.file_name.to_string_lossy();
        let idx = tree.add_child(parent_idx, &name, is_dir, own);

        if is_dir {
            pila.truncate(depth);
            pila.push(idx);
        } else {
            tree.add_to_ancestors(idx, own, 1);
            files += 1;
            bytes += own;
            since_last += 1;
            if since_last >= PROGRESS_EVERY {
                since_last = 0;
                if tx.send(ScanMsg::Progress { files, bytes }).is_err() {
                    return false;
                }
                if last_snapshot.elapsed() >= espera_snapshot {
                    // El árbol ya está agregado: el snapshot es una copia plana.
                    let t = std::time::Instant::now();
                    let snap = Box::new(tree.clone());
                    espera_snapshot = SNAPSHOT_MIN.max(t.elapsed() * SNAPSHOT_FACTOR);
                    last_snapshot = std::time::Instant::now();
                    let msg = ScanMsg::Partial {
                        tree: snap,
                        files,
                        bytes,
                    };
                    if tx.send(msg).is_err() {
                        return false;
                    }
                }
            }
        }
    }

    // Progreso final para que la UI muestre las cifras definitivas.
    tx.send(ScanMsg::Progress { files, bytes }).is_ok()
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

    /// Tamaño recursivo calculado con `std::fs` (referencia independiente).
    fn du(path: &std::path::Path) -> (u64, u64) {
        let m = std::fs::symlink_metadata(path).unwrap();
        if !m.is_dir() {
            return (m.len(), 1);
        }
        let (mut s, mut n) = (0, 0);
        for e in std::fs::read_dir(path).unwrap() {
            let (a, b) = du(&e.unwrap().path());
            s += a;
            n += b;
        }
        (s, n)
    }

    #[test]
    fn padres_y_agregados_coinciden_con_el_disco() {
        // Árbol con hermanos que intercalan archivos y carpetas a varias
        // profundidades: si la pila de padres se equivocara, algún nodo
        // colgaría del directorio equivocado y su ruta no existiría.
        let dir = std::env::temp_dir().join(format!("dirrust_pila_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut k = 1usize;
        for a in 0..4 {
            for b in 0..3 {
                for c in 0..3 {
                    let d = dir
                        .join(format!("a{a}"))
                        .join(format!("b{b}"))
                        .join(format!("c{c}"));
                    std::fs::create_dir_all(&d).unwrap();
                    write_bytes(&d.join(format!("f{k}.Bin")), k * 7);
                    k += 1;
                }
                write_bytes(&dir.join(format!("a{a}")).join(format!("b{b}.txt")), k);
                k += 1;
            }
            std::fs::create_dir_all(dir.join(format!("a{a}")).join("vacia")).unwrap();
        }

        let rx = spawn(ScanConfig {
            root: dir.clone(),
            follow_symlinks: false,
            one_file_system: true,
            size_mode: SizeMode::Apparent,
            threads: 0,
        });
        let tree = loop {
            match rx.recv().unwrap() {
                ScanMsg::Done { tree, .. } => break tree,
                ScanMsg::Error(e) => panic!("escaneo falló: {e}"),
                _ => continue,
            }
        };

        for idx in 0..tree.nodes.len() {
            let ruta = tree.path_of(idx, &dir);
            let meta = std::fs::symlink_metadata(&ruta)
                .unwrap_or_else(|_| panic!("ruta inexistente: {}", ruta.display()));
            let n = &tree.nodes[idx];
            assert_eq!(n.is_dir, meta.is_dir(), "{}", ruta.display());
            assert_eq!((n.size, n.file_count), du(&ruta), "{}", ruta.display());
        }
        // Las extensiones se guardan en minúsculas.
        let f = tree
            .find_by_path(&dir, &dir.join("a0/b0/c0/f1.Bin"))
            .unwrap();
        assert_eq!(tree.ext(f), Some("bin"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
