//! Algoritmo de treemap squarified y rasterizado a una rejilla de "píxeles".
//!
//! El treemap representa cada nodo como un rectángulo de área proporcional a su
//! tamaño. Usamos el algoritmo SQUARIFIED (Bruls, Huizing, van Wijk, 2000), que
//! produce rectángulos con una relación de aspecto cercana a 1 (cuadrados), muy
//! superior al "slice & dice" clásico para leer tamaños de un vistazo.
//!
//! Trabajamos sobre una rejilla de píxeles cuya ALTURA es el doble de las filas
//! del terminal: en `treemap_view.rs` cada celda se dibuja con el carácter de
//! medio bloque `▀` (color de primer plano = píxel superior, color de fondo =
//! píxel inferior), duplicando así la resolución vertical efectiva.

use crate::model::Tree;
use crate::util::{palette_rgb, Rgb};

/// Color de fondo de las zonas del treemap sin tile (hueco).
const FONDO: Rgb = (18, 18, 22);
/// Color de un directorio que se dibuja "sólido" (vacío o demasiado pequeño
/// para subdividir): un gris azulado neutro.
const DIR_SOLIDO: Rgb = (90, 100, 120);

/// Por debajo de esta área en píxeles no merece la pena subdividir un directorio:
/// se dibuja como un único rectángulo. Evita recursión inútil en tiles diminutos.
const AREA_MIN_SUBDIVIDIR: f64 = 24.0;
/// Tope de profundidad de recursión, por seguridad ante árboles muy profundos.
const PROF_MAX: u32 = 14;

/// Rectángulo en coordenadas de píxel (en punto flotante para repartir sin sesgo).
#[derive(Debug, Clone, Copy)]
struct RectF {
    x: f64,
    y: f64,
    w: f64,
    h: f64,
}

impl RectF {
    fn area(&self) -> f64 {
        self.w * self.h
    }
}

/// Rectángulo entero (en píxeles) de un tile de primer nivel. `x1`/`y1` son
/// EXCLUSIVOS. Sirve para resaltado y, en la Fase 4, para el hit-testing.
#[derive(Debug, Clone, Copy)]
pub struct TileRect {
    pub x0: usize,
    pub y0: usize,
    pub x1: usize,
    pub y1: usize,
}

impl TileRect {
    /// ¿Contiene el píxel (px, py)?
    // Se usará en la Fase 4 para el hit-testing del mouse sobre el treemap.
    #[allow(dead_code)]
    pub fn contains(&self, px: usize, py: usize) -> bool {
        px >= self.x0 && px < self.x1 && py >= self.y0 && py < self.y1
    }
}

/// Resultado del cálculo del treemap: la rejilla de colores y los tiles de
/// primer nivel (un tile por hijo directo del directorio mostrado).
pub struct TreemapLayout {
    pub width: usize,
    pub height: usize, // alto en PÍXELES (= filas del terminal * 2)
    /// Color por píxel, fila por fila (índice = y * width + x).
    pub colors: Vec<Rgb>,
    /// Tiles de primer nivel: (índice de nodo hijo, rectángulo en píxeles).
    pub tiles: Vec<(usize, TileRect)>,
}

impl TreemapLayout {
    fn color_at(&self, x: usize, y: usize) -> Rgb {
        self.colors[y * self.width + x]
    }

    /// Color de un píxel (para el render). Acceso seguro acotado.
    pub fn get(&self, x: usize, y: usize) -> Rgb {
        if x < self.width && y < self.height {
            self.color_at(x, y)
        } else {
            FONDO
        }
    }
}

/// Calcula el treemap del directorio `node` para una rejilla `width × height`.
///
/// `cushion` activa el sombreado tipo "cushion" (degradado radial por tile) que
/// imita el aspecto 3D de WinDirStat. Está aislado: si es `false`, se usa el
/// relleno plano con bordes oscurecidos.
///
/// REVISAR: squarified treemap. Punto de entrada que prepara la rejilla y lanza
/// la subdivisión recursiva de los hijos del directorio mostrado.
pub fn build(
    tree: &Tree,
    node: usize,
    width: usize,
    height: usize,
    cushion: bool,
) -> TreemapLayout {
    let mut layout = TreemapLayout {
        width,
        height,
        colors: vec![FONDO; width.saturating_mul(height)],
        tiles: Vec::new(),
    };
    if width == 0 || height == 0 {
        return layout;
    }

    // Referencia para escalar: el tamaño AGREGADO del directorio (la suma de los
    // tamaños de sus hijos). Así sum(áreas hijos) == área de la rejilla.
    let area_ref = tree.nodes[node].size as f64;
    let hijos = hijos_ordenados(tree, node);
    let tiles_f = squarify_node(
        tree,
        &hijos,
        RectF {
            x: 0.0,
            y: 0.0,
            w: width as f64,
            h: height as f64,
        },
        area_ref,
    );

    // Cada tile de primer nivel se registra (para resaltado/hit-testing) y se
    // rellena de forma recursiva para conseguir el aspecto anidado de WinDirStat.
    for (child, r) in tiles_f {
        if let Some(ir) = rasterizar(&r, width, height) {
            layout.tiles.push((child, ir));
        }
        rellenar_recursivo(tree, child, r, 0, cushion, &mut layout);
    }

    layout
}

/// Hijos de un nodo con tamaño > 0, ordenados por tamaño descendente.
///
/// El squarify EXIGE entrada ordenada de mayor a menor para dar buenos aspectos.
fn hijos_ordenados(tree: &Tree, node: usize) -> Vec<usize> {
    let mut v: Vec<usize> = tree.nodes[node]
        .children
        .iter()
        .copied()
        .filter(|&c| tree.nodes[c].size > 0)
        .collect();
    v.sort_by(|&a, &b| tree.nodes[b].size.cmp(&tree.nodes[a].size));
    v
}

/// Subdivide `rect` entre `hijos` con el algoritmo squarified.
///
/// REVISAR: núcleo del squarified treemap. Vamos formando "filas" de tiles a lo
/// largo del lado más CORTO del rectángulo libre; añadimos tiles a la fila
/// mientras la peor relación de aspecto MEJORE, y cuando empeora cerramos la
/// fila, consumimos esa franja del rectángulo y seguimos con el espacio restante.
fn squarify_node(
    tree: &Tree,
    hijos: &[usize],
    rect: RectF,
    tamano_total: f64,
) -> Vec<(usize, RectF)> {
    let mut salida: Vec<(usize, RectF)> = Vec::with_capacity(hijos.len());
    if hijos.is_empty() || tamano_total <= 0.0 {
        return salida;
    }

    // Escalamos los tamaños a ÁREA en píxeles: el factor reparte el área del
    // rectángulo en proporción a los bytes, de modo que sum(areas) == área.
    let factor = rect.area() / tamano_total;
    let areas: Vec<f64> = hijos
        .iter()
        .map(|&c| tree.nodes[c].size as f64 * factor)
        .collect();

    let mut libre = rect;
    let mut i = 0;
    let n = hijos.len();
    while i < n {
        // Lado a lo largo del cual se colocan los tiles de esta fila (el corto).
        let lado = libre.w.min(libre.h);
        if lado <= 0.0 {
            break;
        }

        // Crecemos la fila mientras la peor proporción no empeore.
        let mut fin = i + 1;
        let mut suma = areas[i];
        let mut peor_actual = peor(areas[i], areas[i], suma, lado);
        while fin < n {
            let nueva_suma = suma + areas[fin];
            // En una fila ordenada desc, el máximo es el primero y el mínimo el
            // último que añadimos; así evitamos recalcular min/max de todo.
            let nuevo_peor = peor(areas[i], areas[fin], nueva_suma, lado);
            if nuevo_peor <= peor_actual {
                suma = nueva_suma;
                peor_actual = nuevo_peor;
                fin += 1;
            } else {
                break;
            }
        }

        // Colocamos la fila [i, fin) y recortamos el rectángulo libre.
        libre = colocar_fila(hijos, &areas, i, fin, suma, libre, &mut salida);
        i = fin;
    }

    salida
}

/// Peor (mayor) relación de aspecto de una fila, dado su máximo, su mínimo, la
/// suma de áreas de la fila y la longitud del lado. Fórmula de Bruls et al.
fn peor(max: f64, min: f64, suma: f64, lado: f64) -> f64 {
    let s2 = suma * suma;
    let l2 = lado * lado;
    // max(  l²·max / s²  ,  s² / (l²·min)  )
    (l2 * max / s2).max(s2 / (l2 * min))
}

/// Coloca los tiles `[ini, fin)` como una franja en el lado corto de `libre`,
/// añade sus rectángulos a `salida` y devuelve el rectángulo libre restante.
fn colocar_fila(
    hijos: &[usize],
    areas: &[f64],
    ini: usize,
    fin: usize,
    suma: f64,
    libre: RectF,
    salida: &mut Vec<(usize, RectF)>,
) -> RectF {
    if libre.w <= libre.h {
        // Lado corto = anchura → franja HORIZONTAL en la parte superior.
        // grosor = área de la fila / anchura.
        let grosor = (suma / libre.w).min(libre.h);
        let mut x = libre.x;
        for k in ini..fin {
            // Anchura proporcional al área dentro de la franja.
            let w = if grosor > 0.0 { areas[k] / grosor } else { 0.0 };
            salida.push((
                hijos[k],
                RectF {
                    x,
                    y: libre.y,
                    w,
                    h: grosor,
                },
            ));
            x += w;
        }
        RectF {
            x: libre.x,
            y: libre.y + grosor,
            w: libre.w,
            h: libre.h - grosor,
        }
    } else {
        // Lado corto = altura → franja VERTICAL a la izquierda.
        let grosor = (suma / libre.h).min(libre.w);
        let mut y = libre.y;
        for k in ini..fin {
            let h = if grosor > 0.0 { areas[k] / grosor } else { 0.0 };
            salida.push((
                hijos[k],
                RectF {
                    x: libre.x,
                    y,
                    w: grosor,
                    h,
                },
            ));
            y += h;
        }
        RectF {
            x: libre.x + grosor,
            y: libre.y,
            w: libre.w - grosor,
            h: libre.h,
        }
    }
}

/// Rellena un tile en la rejilla. Si es un directorio suficientemente grande,
/// se subdivide recursivamente entre sus hijos (aspecto anidado); si es un
/// archivo (o un directorio diminuto/vacío) se pinta como un rectángulo sólido.
fn rellenar_recursivo(
    tree: &Tree,
    node: usize,
    rect: RectF,
    prof: u32,
    cushion: bool,
    layout: &mut TreemapLayout,
) {
    let nodo = &tree.nodes[node];
    let puede_subdividir = nodo.is_dir
        && prof < PROF_MAX
        && rect.area() >= AREA_MIN_SUBDIVIDIR
        && nodo.children.iter().any(|&c| tree.nodes[c].size > 0);

    if puede_subdividir {
        let hijos = hijos_ordenados(tree, node);
        // El área "total" de referencia para escalar es el tamaño agregado del
        // directorio, de modo que sum(areas hijos) == área del rectángulo.
        let area_ref = tree.nodes[node].size as f64;
        for (child, r) in squarify_node(tree, &hijos, rect, area_ref) {
            rellenar_recursivo(tree, child, r, prof + 1, cushion, layout);
        }
    } else {
        // Hoja visual: color por extensión (archivo) o gris (directorio sólido).
        let color = if nodo.is_dir {
            DIR_SOLIDO
        } else {
            palette_rgb(nodo.extension.as_deref())
        };
        pintar_rect(layout, &rect, color, cushion);
    }
}

/// Convierte un `RectF` a límites enteros de píxel, recortando a la rejilla.
/// Devuelve `None` si queda vacío (más fino que un píxel).
fn rasterizar(rect: &RectF, width: usize, height: usize) -> Option<TileRect> {
    let x0 = rect.x.round().max(0.0) as usize;
    let y0 = rect.y.round().max(0.0) as usize;
    let x1 = ((rect.x + rect.w).round() as i64).clamp(0, width as i64) as usize;
    let y1 = ((rect.y + rect.h).round() as i64).clamp(0, height as i64) as usize;
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some(TileRect { x0, y0, x1, y1 })
}

/// Pinta un rectángulo en la rejilla.
///
/// Con `cushion`, aplica un degradado radial de brillo (claro en el centro,
/// oscuro en los bordes) que simula un relieve 3D — `// REVISAR:` técnica de
/// cushion shading, aislada y opcional. Sin él, relleno plano oscureciendo solo
/// el borde superior/izquierdo para separar tiles adyacentes.
fn pintar_rect(layout: &mut TreemapLayout, rect: &RectF, color: Rgb, cushion: bool) {
    let Some(ir) = rasterizar(rect, layout.width, layout.height) else {
        return;
    };
    let w = layout.width;

    if cushion {
        // Centro y semiejes del tile para normalizar la posición a [-1, 1].
        let cx = (ir.x0 + ir.x1) as f64 / 2.0;
        let cy = (ir.y0 + ir.y1) as f64 / 2.0;
        let hx = ((ir.x1 - ir.x0) as f64 / 2.0).max(1.0);
        let hy = ((ir.y1 - ir.y0) as f64 / 2.0).max(1.0);
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                let nx = (x as f64 - cx) / hx;
                let ny = (y as f64 - cy) / hy;
                // Brillo: máximo en el centro, decae con la distancia al cuadrado.
                let brillo = (1.15 - 0.55 * (nx * nx + ny * ny)).clamp(0.4, 1.15);
                layout.colors[y * w + x] = escalar(color, brillo);
            }
        }
    } else {
        let borde = escalar(color, 0.65);
        for y in ir.y0..ir.y1 {
            for x in ir.x0..ir.x1 {
                // Borde en la primera fila/columna del tile.
                let c = if x == ir.x0 || y == ir.y0 {
                    borde
                } else {
                    color
                };
                layout.colors[y * w + x] = c;
            }
        }
    }
}

/// Escala un color por un factor de brillo (saturando a 255 si supera 1.0).
fn escalar((r, g, b): Rgb, f: f64) -> Rgb {
    let s = |c: u8| (c as f64 * f).round().clamp(0.0, 255.0) as u8;
    (s(r), s(g), s(b))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::Tree;
    use std::path::Path;

    fn area(r: &TileRect) -> usize {
        (r.x1 - r.x0) * (r.y1 - r.y0)
    }

    #[test]
    fn squarify_reparte_proporcional_y_cubre_el_area() {
        // root/ con tres archivos de tamaños 600 / 300 / 100.
        let mut tree = Tree::with_root(Path::new("/root"), true, 0);
        let a = tree.add_child(tree.root, "a.bin".into(), false, 600, Some("bin".into()));
        let b = tree.add_child(tree.root, "b.bin".into(), false, 300, Some("bin".into()));
        let _c = tree.add_child(tree.root, "c.bin".into(), false, 100, Some("bin".into()));
        tree.aggregate();
        assert_eq!(tree.total_size(), 1000);

        let (w, h) = (100usize, 100usize);
        let layout = build(&tree, tree.root, w, h, false);

        // Un tile de primer nivel por hijo con tamaño > 0.
        assert_eq!(layout.tiles.len(), 3);

        // El archivo más grande (a) debe tener el tile de mayor área.
        let area_de = |node: usize| area(&layout.tiles.iter().find(|(n, _)| *n == node).unwrap().1);
        let aa = area_de(a);
        let ab = area_de(b);
        assert!(aa > ab, "el mayor tamaño debe tener el mayor tile");

        // La suma de áreas debe cubrir casi todo el rectángulo (sin grandes
        // huecos), permitiendo una pequeña pérdida por redondeo a píxeles.
        let suma: usize = layout.tiles.iter().map(|(_, r)| area(r)).sum();
        let total = w * h;
        assert!(
            suma as f64 >= 0.85 * total as f64,
            "cobertura insuficiente: {suma} de {total}"
        );

        // Proporción aproximada: a ≈ 6·c, comprobamos que a ocupa ~60%.
        assert!(
            (aa as f64 / total as f64) > 0.5,
            "el tile de 'a' debería rondar el 60% del área"
        );
    }
}
