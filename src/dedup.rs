//! Detección de archivos duplicados (por contenido idéntico).
//!
//! Estrategia en varias fases, pensada para ser rápida y PRECISA incluso con
//! muchos archivos:
//!   0. Pre-filtro por umbral mínimo de tamaño (descarta el ruido de duplicados
//!      triviales) y agrupación por el tamaño aproximado conocido del árbol.
//!   1. `stat` de los candidatos: tamaño APARENTE real + inodo. Colapsamos los
//!      enlaces duros (mismo inodo = mismo archivo físico, borrar uno no libera
//!      nada) y reagrupamos por tamaño aparente exacto.
//!   2. Hash PARCIAL (primeros 64 KiB) de TODOS los candidatos en un único barrido
//!      paralelo: descarta de golpe casi todos los falsos candidatos.
//!   3. Confirmación BYTE A BYTE dentro de cada grupo que comparte hash parcial:
//!      garantiza que no hay coincidencias falsas (ni por colisión de hash ni por
//!      coincidir solo en los primeros KB).
//!
//! Las fases pesadas (E/S) se paralelizan con rayon. El hash parcial usa xxh3
//! (xxHash), muy rápido y no criptográfico — solo sirve de criba; la igualdad
//! definitiva la decide la comparación byte a byte.
//!
//! REVISAR (concurrencia): `spawn` ejecuta todo en un hilo de fondo y envía el
//! resultado por un `crossbeam-channel`, de modo que la UI nunca se bloquea.

use std::collections::HashMap;
use std::io::{self, Read};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crossbeam_channel::Receiver;
use rayon::prelude::*;
use xxhash_rust::xxh3::Xxh3;

/// Tamaño de la ventana de hash parcial (64 KiB) y del búfer de lectura.
const PARCIAL: u64 = 64 * 1024;

/// Umbral mínimo de tamaño por defecto (4 KiB): por debajo de esto los
/// "duplicados" son ruido (apenas recuperan espacio) y solo ensucian la lista.
pub const MIN_SIZE_PREDETERMINADO: u64 = 4 * 1024;

/// Mínimo de elementos por tarea paralela. Con árboles enormes (decenas de miles
/// de candidatos —típico en modo `--disk`, donde muchos archivos comparten el
/// tamaño redondeado a bloques) un `par_iter` sin límite genera tantísimos
/// sub-trabajos que el robo de trabajo de rayon ANIDA la recursión del puente
/// productor/consumidor hasta desbordar la pila del worker. Agrupar el trabajo en
/// lotes de este tamaño acota el troceado (y de paso va más rápido).
const MIN_TRABAJO_PAR: usize = 256;

/// Tamaño de la pila de cada worker del pool dedicado de duplicados (16 MiB).
/// Holgura amplia frente a la recursión del iterador paralelo de rayon.
const PILA_WORKER: usize = 16 * 1024 * 1024;

/// Un grupo de archivos con contenido idéntico.
#[derive(Debug, Clone)]
pub struct DupGroup {
    /// Tamaño aparente de cada archivo del grupo (todos iguales).
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
/// `files` es la lista (ruta, tamaño aproximado) de los archivos a considerar; se
/// recolecta en el llamador (es barato) y el trabajo pesado de E/S + hashing se
/// hace aquí, fuera del hilo de UI. `min_size` descarta los archivos más pequeños.
pub fn spawn(files: Vec<(PathBuf, u64)>, min_size: u64) -> Receiver<DedupMsg> {
    let (tx, rx) = crossbeam_channel::bounded(1);
    std::thread::Builder::new()
        .name("dirrust-dedup".into())
        .spawn(move || {
            let grupos = en_pool_dedicado(&files, min_size);
            let _ = tx.send(DedupMsg::Done(grupos));
        })
        .expect("no se pudo crear el hilo de duplicados");
    rx
}

/// Ejecuta `find_duplicates` en un pool de rayon DEDICADO con pila amplia por
/// worker. Con árboles enormes (p. ej. miles de archivos del mismo tamaño en modo
/// `--disk`) el robo de trabajo de rayon anida mucho la recursión del iterador
/// paralelo; una pila por worker más grande evita el desbordamiento de pila. Si el
/// pool no se puede crear, caemos al pool global por defecto.
fn en_pool_dedicado(files: &[(PathBuf, u64)], min_size: u64) -> Vec<DupGroup> {
    match rayon::ThreadPoolBuilder::new()
        .stack_size(PILA_WORKER)
        .build()
    {
        Ok(pool) => pool.install(|| find_duplicates(files, min_size)),
        Err(_) => find_duplicates(files, min_size),
    }
}

/// Calcula los grupos de archivos duplicados a partir de (ruta, tamaño aprox).
///
/// `min_size` es el umbral mínimo de tamaño aparente: los archivos por debajo se
/// ignoran (con `0` se consideran todos los no vacíos).
pub fn find_duplicates(files: &[(PathBuf, u64)], min_size: u64) -> Vec<DupGroup> {
    // Fase 0: pre-filtro por umbral y agrupación por el tamaño APROXIMADO conocido
    // (el del árbol). Es solo una criba barata sin tocar disco: dos archivos
    // idénticos comparten tamaño, así que descartar tamaños únicos es seguro.
    let umbral = min_size.max(1); // nunca consideramos archivos vacíos
    let mut by_size_hint: HashMap<u64, Vec<&Path>> = HashMap::new();
    for (p, s) in files {
        if *s >= umbral {
            by_size_hint.entry(*s).or_default().push(p.as_path());
        }
    }

    // Candidatos: rutas que comparten tamaño aproximado con al menos otra.
    let candidatos: Vec<&Path> = by_size_hint
        .into_values()
        .filter(|v| v.len() >= 2)
        .flatten()
        .collect();
    if candidatos.is_empty() {
        return Vec::new();
    }

    // Fase 1: `stat` en paralelo → (tamaño aparente real, dev, ino). Reaplicamos
    // el umbral sobre el tamaño REAL (el aproximado podía estar redondeado a
    // bloques en modo disco) y descartamos lo ilegible.
    let stats: Vec<InfoArchivo> = candidatos
        .par_iter()
        .with_min_len(MIN_TRABAJO_PAR)
        .filter_map(|p| stat_archivo(p, umbral))
        .collect();

    // Reagrupamos por tamaño aparente EXACTO y colapsamos enlaces duros
    // (mismo (dev, ino) = mismo archivo físico): se conserva una sola ruta.
    let mut by_size: HashMap<u64, Vec<InfoArchivo>> = HashMap::new();
    for info in stats {
        by_size.entry(info.size).or_default().push(info);
    }

    // Aplanamos todos los candidatos supervivientes para hashear en un único
    // barrido paralelo (mejor uso de rayon que hacerlo grupo a grupo).
    let mut a_hashear: Vec<(u64, PathBuf)> = Vec::new(); // (tamaño, ruta)
    for (size, mut grupo) in by_size {
        colapsar_enlaces_duros(&mut grupo);
        if grupo.len() < 2 {
            continue; // tras quitar hardlinks ya no hay duplicado posible
        }
        for info in grupo {
            a_hashear.push((size, info.path));
        }
    }
    if a_hashear.is_empty() {
        return Vec::new();
    }

    // Fase 2: hash parcial (64 KiB) de todos los candidatos en paralelo.
    let parciales: Vec<(u64, u64, PathBuf)> = a_hashear
        .par_iter()
        .with_min_len(MIN_TRABAJO_PAR)
        .filter_map(|(size, p)| hash_file(p, PARCIAL).ok().map(|h| (*size, h, p.clone())))
        .collect();

    // Agrupamos por (tamaño, hash parcial).
    let mut by_partial: HashMap<(u64, u64), Vec<PathBuf>> = HashMap::new();
    for (size, h, p) in parciales {
        by_partial.entry((size, h)).or_default().push(p);
    }

    // Fase 3: confirmación byte a byte. Cada grupo con hash parcial común se
    // parte en clases de igualdad REAL; emitimos las clases con >= 2 archivos.
    // Procesamos los grupos en paralelo (cada uno hace su propia E/S).
    let grupos_candidatos: Vec<(u64, Vec<PathBuf>)> = by_partial
        .into_iter()
        .filter(|(_, v)| v.len() >= 2)
        .map(|((size, _), v)| (size, v))
        .collect();

    let mut grupos: Vec<DupGroup> = grupos_candidatos
        .par_iter()
        .with_min_len(MIN_TRABAJO_PAR)
        .flat_map_iter(|(size, paths)| {
            particionar_identicos(paths)
                .into_iter()
                .filter(|clase| clase.len() >= 2)
                .map(move |clase| DupGroup {
                    size: *size,
                    paths: clase,
                })
        })
        .collect();

    // Ordenamos por espacio recuperable descendente (lo más rentable arriba).
    grupos.sort_by_key(|b| std::cmp::Reverse(b.recoverable()));
    grupos
}

/// Datos de un archivo candidato tras `stat`.
struct InfoArchivo {
    path: PathBuf,
    size: u64,
    dev: u64,
    ino: u64,
}

/// `stat` de un archivo: devuelve su info si es legible y cumple el umbral de
/// tamaño aparente. `None` si no se puede leer la metadata o es demasiado pequeño.
fn stat_archivo(path: &Path, umbral: u64) -> Option<InfoArchivo> {
    let meta = std::fs::metadata(path).ok()?;
    let size = meta.len();
    if size < umbral {
        return None;
    }
    Some(InfoArchivo {
        path: path.to_path_buf(),
        size,
        dev: meta.dev(),
        ino: meta.ino(),
    })
}

/// Quita del grupo los enlaces duros redundantes: si varias rutas apuntan al
/// mismo inodo `(dev, ino)`, solo son una copia física, así que conservamos una.
fn colapsar_enlaces_duros(grupo: &mut Vec<InfoArchivo>) {
    let mut vistos = std::collections::HashSet::new();
    grupo.retain(|i| vistos.insert((i.dev, i.ino)));
}

/// Particiona una lista de rutas (que ya comparten tamaño y hash parcial) en
/// clases de archivos byte a byte IDÉNTICOS. La comparación real es lo que evita
/// las coincidencias falsas. Las rutas ilegibles se descartan.
///
/// Comparamos cada archivo contra el representante de cada clase ya formada; como
/// estos grupos son pequeños, el coste es mínimo y la lectura corta en cuanto
/// aparece la primera diferencia.
fn particionar_identicos(paths: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let mut clases: Vec<Vec<PathBuf>> = Vec::new();
    'archivo: for p in paths {
        for clase in clases.iter_mut() {
            match archivos_identicos(&clase[0], p) {
                Ok(true) => {
                    clase.push(p.clone());
                    continue 'archivo;
                }
                Ok(false) => continue, // distinto: probar siguiente clase
                Err(_) => continue 'archivo, // ilegible: lo descartamos
            }
        }
        // No encajó en ninguna clase: empieza una nueva.
        clases.push(vec![p.clone()]);
    }
    clases
}

/// Compara dos archivos byte a byte. Corta en la primera diferencia.
fn archivos_identicos(a: &Path, b: &Path) -> io::Result<bool> {
    let mut fa = std::fs::File::open(a)?;
    let mut fb = std::fs::File::open(b)?;
    // Búferes en el HEAP (no en la pila): esta función se ejecuta en las HOJAS de
    // la recursión paralela de rayon; dos arrays de 64 KiB por marco (128 KiB)
    // dispararían el desbordamiento de pila del worker. En el heap el marco queda
    // diminuto. (Esta fue una de las causas del crash en `--disk`.)
    let mut ba = vec![0u8; 64 * 1024];
    let mut bb = vec![0u8; 64 * 1024];
    loop {
        let na = leer_lleno(&mut fa, &mut ba)?;
        let nb = leer_lleno(&mut fb, &mut bb)?;
        if na != nb {
            return Ok(false); // longitudes distintas (no debería pasar, pero seguro)
        }
        if na == 0 {
            return Ok(true); // ambos al final sin diferencias
        }
        if ba[..na] != bb[..nb] {
            return Ok(false);
        }
    }
}

/// Llena `buf` leyendo repetidamente (maneja lecturas cortas). Devuelve cuántos
/// bytes se leyeron (0 = fin de archivo).
fn leer_lleno(f: &mut std::fs::File, buf: &mut [u8]) -> io::Result<usize> {
    let mut total = 0;
    while total < buf.len() {
        let n = f.read(&mut buf[total..])?;
        if n == 0 {
            break;
        }
        total += n;
    }
    Ok(total)
}

/// Hash xxh3 de los primeros `limit` bytes de un archivo (o de todo si `limit`
/// es mayor que su tamaño). Lee por bloques para no cargar el archivo entero.
fn hash_file(path: &Path, limit: u64) -> io::Result<u64> {
    let mut f = std::fs::File::open(path)?;
    let mut hasher = Xxh3::new();
    // Búfer en el HEAP: ver la nota en `archivos_identicos` (evita marcos de pila
    // grandes en las hojas de la recursión paralela de rayon).
    let mut buf = vec![0u8; 64 * 1024];
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

    fn dir_tmp(nombre: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("dirrust_{nombre}_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn listar(dir: &Path) -> Vec<(PathBuf, u64)> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|e| {
                let e = e.unwrap();
                let len = e.metadata().unwrap().len();
                (e.path(), len)
            })
            .collect()
    }

    /// Regresión del desbordamiento de pila: con decenas de miles de archivos del
    /// MISMO tamaño (el caso que disparaba el crash en modo `--disk`), el cómputo
    /// debe completar sin desbordar la pila del worker de rayon. Va por la ruta de
    /// producción (`spawn` → pool dedicado con pila de 16 MiB). Marcado `#[ignore]`
    /// porque crea muchos archivos; ejecútalo con:
    ///   cargo test --release estres -- --ignored --nocapture
    #[test]
    #[ignore]
    fn estres_muchos_archivos_mismo_tamano_no_desborda() {
        let dir = dir_tmp("estres");
        let datos = vec![b'q'; 4096]; // >= umbral de 4 KiB
        let total = 50_000;
        for i in 0..total {
            escribir(&dir.join(format!("f{i}.bin")), &datos);
        }

        let rx = spawn(listar(&dir), 4096);
        let DedupMsg::Done(grupos) = rx.recv().unwrap();

        // Todos idénticos → un único grupo con los `total` archivos.
        assert_eq!(grupos.len(), 1, "todos comparten contenido");
        assert_eq!(grupos[0].paths.len(), total);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn detecta_duplicados_pequenos_y_grandes() {
        let dir = dir_tmp("dup");

        // Pequeños (<= PARCIAL): a1 == a2 idénticos; a3 mismo tamaño, distinto.
        escribir(&dir.join("a1.txt"), b"contenido identico");
        escribir(&dir.join("a2.txt"), b"contenido identico");
        escribir(&dir.join("a3.txt"), b"contenido DISTINTO"); // misma longitud

        // Grandes (> PARCIAL): b1 == b2; b3 igual en los primeros 64KB pero
        // diferente después (fuerza el camino de comparación byte a byte).
        let mut grande = vec![b'x'; 128 * 1024];
        escribir(&dir.join("b1.bin"), &grande);
        escribir(&dir.join("b2.bin"), &grande);
        grande[100_000] = b'y'; // diferencia más allá de los 64 KiB
        escribir(&dir.join("b3.bin"), &grande);

        // Umbral 0: se consideran todos los archivos no vacíos.
        let grupos = find_duplicates(&listar(&dir), 0);

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

    #[test]
    fn respeta_umbral_minimo() {
        let dir = dir_tmp("umbral");

        // Par idéntico PEQUEÑO (< 4 KiB): debe ignorarse con umbral de 4 KiB.
        escribir(&dir.join("p1.txt"), b"holahola");
        escribir(&dir.join("p2.txt"), b"holahola");

        // Par idéntico GRANDE (>= 4 KiB): debe detectarse.
        let datos = vec![b'z'; 8 * 1024];
        escribir(&dir.join("g1.bin"), &datos);
        escribir(&dir.join("g2.bin"), &datos);

        let grupos = find_duplicates(&listar(&dir), 4 * 1024);
        assert_eq!(grupos.len(), 1, "solo el par grande supera el umbral");
        assert_eq!(grupos[0].paths.len(), 2);
        assert_eq!(grupos[0].size, 8 * 1024);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rechaza_coincidencias_falsas_tras_primer_bloque() {
        let dir = dir_tmp("falsas");

        // Dos archivos del MISMO tamaño con idéntico primer 64 KiB pero distinto
        // contenido después: la confirmación byte a byte NO debe agruparlos.
        let mut a = vec![b'a'; 200 * 1024];
        let mut b = a.clone();
        a[150_000] = b'1';
        b[150_000] = b'2';
        escribir(&dir.join("x.bin"), &a);
        escribir(&dir.join("y.bin"), &b);

        let grupos = find_duplicates(&listar(&dir), 0);
        assert!(
            grupos.is_empty(),
            "no deben agruparse: difieren tras 64 KiB"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn colapsa_enlaces_duros() {
        let dir = dir_tmp("hardlink");

        // Un archivo y un enlace duro a él: mismo inodo → NO es un duplicado real
        // (borrar uno no libera espacio), así que no debe aparecer.
        let datos = vec![b'h'; 8 * 1024];
        let orig = dir.join("orig.bin");
        let link = dir.join("link.bin");
        escribir(&orig, &datos);
        std::fs::hard_link(&orig, &link).unwrap();

        let grupos = find_duplicates(&listar(&dir), 4 * 1024);
        assert!(
            grupos.is_empty(),
            "un hardlink no es un duplicado recuperable"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }
}
