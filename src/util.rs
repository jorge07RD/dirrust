//! Utilidades transversales: formateo de tamaños y paleta de colores.

use humansize::{FormatSizeOptions, BINARY};

/// Color RGB simple (0-255 por canal). Lo usamos en el treemap y el desglose.
pub type Rgb = (u8, u8, u8);

/// Color neutro para archivos SIN extensión (gris).
const SIN_EXT: Rgb = (150, 150, 150);

/// Devuelve un color CONSISTENTE para una extensión dada.
///
/// La misma extensión produce siempre el mismo color (no es aleatorio por
/// frame): hasheamos el texto de la extensión a un tono (hue) y lo convertimos
/// con saturación/brillo fijos. Así obtenemos una paleta agradable y estable,
/// y el treemap y el desglose por extensión comparten exactamente los colores.
pub fn palette_rgb(ext: Option<&str>) -> Rgb {
    match ext {
        None => SIN_EXT,
        Some(e) => {
            // Hue determinista a partir del hash FNV-1a de la extensión.
            let hue = (fnv1a(e.as_bytes()) % 360) as f64;
            // Saturación y brillo moderados: colores vivos pero no chillones,
            // y con suficiente luminosidad para que el texto/bordes se distingan.
            hsv_to_rgb(hue, 0.55, 0.82)
        }
    }
}

/// Hash FNV-1a de 32 bits (rápido y determinista; suficiente para derivar tonos).
fn fnv1a(bytes: &[u8]) -> u32 {
    // Constantes estándar de FNV-1a 32 bits.
    let mut hash: u32 = 0x811c_9dc5;
    for &b in bytes {
        hash ^= b as u32;
        hash = hash.wrapping_mul(0x0100_0193);
    }
    hash
}

/// Conversión HSV→RGB. `h` en [0,360), `s` y `v` en [0,1].
fn hsv_to_rgb(h: f64, s: f64, v: f64) -> Rgb {
    let c = v * s;
    let h2 = h / 60.0;
    let x = c * (1.0 - (h2 % 2.0 - 1.0).abs());
    let (r1, g1, b1) = match h2 as u32 {
        0 => (c, x, 0.0),
        1 => (x, c, 0.0),
        2 => (0.0, c, x),
        3 => (0.0, x, c),
        4 => (x, 0.0, c),
        _ => (c, 0.0, x),
    };
    let m = v - c;
    (
        ((r1 + m) * 255.0).round() as u8,
        ((g1 + m) * 255.0).round() as u8,
        ((b1 + m) * 255.0).round() as u8,
    )
}

/// Formatea un número de bytes de forma legible (KiB, MiB, GiB...).
///
/// Usamos la convención BINARIA (base 1024) porque es la habitual en
/// herramientas de disco tipo WinDirStat/`du -h`, y la que el usuario espera al
/// comparar con el explorador de archivos.
pub fn format_size(bytes: u64) -> String {
    // `DECIMAL`/`BINARY` controlan las unidades; fijamos 2 decimales para tener
    // un ancho de columna estable en la tabla.
    let opts = FormatSizeOptions::from(BINARY).decimal_places(2);
    humansize::format_size(bytes, opts)
}

/// Formatea un conteo de archivos con separador de millares (estilo 1.234.567).
///
/// Lo hacemos a mano para no añadir una dependencia de localización; el punto
/// como separador de millares es el habitual en español.
pub fn format_count(n: u64) -> String {
    let s = n.to_string();
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    let len = bytes.len();
    for (i, b) in bytes.iter().enumerate() {
        // Insertamos un punto cada 3 dígitos contando desde la derecha.
        if i > 0 && (len - i).is_multiple_of(3) {
            out.push('.');
        }
        out.push(*b as char);
    }
    out
}
