//! Detección de archivos duplicados (por contenido idéntico).
//!
//! Estrategia en varias fases para que sea rápida incluso con muchos archivos:
//!   1. Agrupar por TAMAÑO exacto: dos archivos distintos en tamaño no pueden
//!      ser idénticos, así que descartamos de golpe la mayoría.
//!   2. Dentro de cada grupo de igual tamaño (>1), hash PARCIAL de los primeros
//!      KB: barato y descarta casi todos los falsos candidatos.
//!   3. Si el hash parcial coincide y el archivo es mayor que la ventana parcial,
//!      hash COMPLETO para confirmar.
//!
//! Los hashes se calculan en PARALELO con rayon. Usamos xxh3 (xxHash), muy
//! rápido y no criptográfico — suficiente para deduplicación por contenido.
//!
//! REVISAR (concurrencia): `spawn` ejecuta todo en un hilo de fondo y envía el
//! resultado por un `crossbeam-channel`, de modo que la UI nunca se bloquea.

use std::collections::HashMap;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use rayon::prelude::*;
use xxhash_rust::xxh3::Xxh3;

/// Tamaño de la ventana de hash parcial (64 KiB). Archivos de este tamaño o
/// menores quedan totalmente cubiertos por el hash parcial (= hash completo).
const PARCIAL: u64 = 64 * 1024;

/// Un grupo de archivos con contenido idéntico.
#[derive(Debug, Clone)]
pub struct DupGroup {
    /// Tamaño de cada archivo del grupo (todos iguales).
    pub size: u64,
    /// Rutas de los archivos idénticos (longitud >= 2).
    pub paths: Vec<PathBuf>,
}

impl DupGroup {
    /// Espacio recuperable si se conserva una copia y se borran las demás.
    pub fn recoverable(&self) -> u64 {
        self.size * (self.paths.len() as u64 - 1)
    }
}

/// Mensajes del cómputo de duplicados hacia la UI.
#[derive(Debug)]
pub enum DedupMsg {
    /// Cómputo terminado con la lista de grupos (ordenada por espacio recuperable).
    Done(Vec<DupGroup>),
}

/// Lanza la detección de duplicados en un hilo de fondo y devuelve el receptor.
///
/// `files` es la lista (ruta, tamaño) de TODOS los archivos a considerar; se
/// recolecta en el llamador (es barato) y el trabajo pesado de E/S + hashing se
/// hace aquí, fuera del hilo de UI.
pub fn spawn(files: Vec<(PathBuf, u64)>) -> Receiver<DedupMsg> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("dirrust-dedup".into())
        .spawn(move || {
            let grupos = find_duplicates(&files);
            let _ = tx.send(DedupMsg::Done(grupos));
        })
        .expect("no se pudo crear el hilo de duplicados");
    rx
}

/// Calcula los grupos de archivos duplicados a partir de (ruta, tamaño).
pub fn find_duplicates(files: &[(PathBuf, u64)]) -> Vec<DupGroup> {
    // Fase 1: agrupar por tamaño exacto (ignoramos archivos vacíos: serían
    // todos "duplicados" entre sí pero con 0 bytes recuperables, puro ruido).
    let mut by_size: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (p, s) in files {
        if *s > 0 {
            by_size.entry(*s).or_default().push(p.clone());
        }
    }

    let mut grupos: Vec<DupGroup> = Vec::new();
    for (size, paths) in by_size {
        if paths.len() < 2 {
            continue; // tamaño único → no puede haber duplicado
        }
        agrupar_por_hash(size, &paths, &mut grupos);
    }

    // Ordenamos por espacio recuperable descendente (lo más rentable arriba).
    grupos.sort_by_key(|b| std::cmp::Reverse(b.recoverable()));
    grupos
}

/// Refina un grupo de igual tamaño con hash parcial y, si hace falta, completo.
fn agrupar_por_hash(size: u64, paths: &[PathBuf], out: &mut Vec<DupGroup>) {
    // Fase 2: hash parcial en paralelo. Los archivos ilegibles se descartan.
    let parciales: Vec<(u64, PathBuf)> = paths
        .par_iter()
        .filter_map(|p| hash_file(p, PARCIAL).ok().map(|h| (h, p.clone())))
        .collect();

    let mut by_partial: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for (h, p) in parciales {
        by_partial.entry(h).or_default().push(p);
    }

    for grupo in by_partial.into_values() {
        if grupo.len() < 2 {
            continue;
        }
        if size <= PARCIAL {
            // El hash parcial cubrió el archivo entero: ya es definitivo.
            out.push(DupGroup { size, paths: grupo });
        } else {
            // Fase 3: hash completo para confirmar (también en paralelo).
            let completos: Vec<(u64, PathBuf)> = grupo
                .par_iter()
                .filter_map(|p| hash_file(p, u64::MAX).ok().map(|h| (h, p.clone())))
                .collect();
            let mut by_full: HashMap<u64, Vec<PathBuf>> = HashMap::new();
            for (h, p) in completos {
                by_full.entry(h).or_default().push(p);
            }
            for g in by_full.into_values() {
                if g.len() >= 2 {
                    out.push(DupGroup { size, paths: g });
                }
            }
        }
    }
}

/// Hash xxh3 de los primeros `limit` bytes de un archivo (o de todo si `limit`
/// es mayor que su tamaño). Lee por bloques para no cargar el archivo entero.
fn hash_file(path: &Path, limit: u64) -> io::Result<u64> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Xxh3::new();
    let mut buf = [0u8; 64 * 1024];
    let mut leido: u64 = 0;
    while leido < limit {
        let n = f.read(&mut buf)?;
        if n == 0 {
            break;
        }
        // No pasarnos del límite en el último bloque.
        let restante = (limit - leido) as usize;
        let take = n.min(restante);
        hasher.update(&buf[..take]);
        leido += take as u64;
    }
    Ok(hasher.digest())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    fn escribir(path: &Path, contenido: &[u8]) {
        let mut f = std::fs::File::create(path).unwrap();
        f.write_all(contenido).unwrap();
    }

    #[test]
    fn detecta_duplicados_pequenos_y_grandes() {
        let dir = std::env::temp_dir().join(format!("dirrust_dup_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();

        // Pequeños (<= PARCIAL): a1 == a2 idénticos; a3 mismo tamaño, distinto.
        escribir(&dir.join("a1.txt"), b"contenido identico");
        escribir(&dir.join("a2.txt"), b"contenido identico");
        escribir(&dir.join("a3.txt"), b"contenido DISTINTO"); // misma longitud

        // Grandes (> PARCIAL): b1 == b2; b3 igual en los primeros 64KB pero
        // diferente después (fuerza el camino de hash COMPLETO).
        let mut grande = vec![b'x'; 128 * 1024];
        escribir(&dir.join("b1.bin"), &grande);
        escribir(&dir.join("b2.bin"), &grande);
        grande[100_000] = b'y'; // diferencia más allá de los 64 KiB
        escribir(&dir.join("b3.bin"), &grande);

        let files: Vec<(PathBuf, u64)> = std::fs::read_dir(&dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let len = e.metadata().unwrap().len();
                (e.path(), len)
            })
            .collect();

        let grupos = find_duplicates(&files);

        // Esperamos exactamente 2 grupos: {a1,a2} y {b1,b2}.
        assert_eq!(grupos.len(), 2, "deben detectarse 2 grupos de duplicados");
        for g in &grupos {
            assert_eq!(g.paths.len(), 2);
            // Ninguno debe contener los archivos "distintos".
            for p in &g.paths {
                let n = p.file_name().unwrap().to_string_lossy();
                assert!(n != "a3.txt" && n != "b3.bin", "{n} no es duplicado");
            }
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
