//! Panel del treemap: rasteriza el `TreemapLayout` a la pantalla.
//!
//! REVISAR: técnica de MEDIO BLOQUE. Cada celda del terminal dibuja el carácter
//! `▀` (UPPER HALF BLOCK). Su color de PRIMER PLANO pinta la mitad superior de la
//! celda y su color de FONDO pinta la mitad inferior. Como el treemap se calcula
//! sobre una rejilla cuyo alto es el doble de las filas (height = filas*2), cada
//! celda (cx, cy) toma dos píxeles verticales — el (2·cy) arriba y el (2·cy+1)
//! abajo — y así DUPLICAMOS la resolución vertical sin coste extra.

use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::treemap::{self, TileRect};
use crate::util::{format_size, Rgb};

/// Borde del tile SELECCIONADO (blanco puro, máximo contraste).
const RESALTE: Rgb = (255, 255, 255);
/// Borde del tile bajo el cursor (HOVER): amarillo, para distinguirlo del
/// resaltado de selección.
const HOVER: Rgb = (255, 230, 0);

/// Dibuja el panel del treemap en `area`. `focused` resalta el borde del marco.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    // Título: si el cursor está sobre un tile, mostramos su nombre y tamaño
    // (hace de barra de estado contextual para el treemap).
    let titulo = match (app.tree(), app.hover_node) {
        (Some(tree), Some(h)) if h < tree.nodes.len() => {
            let n = &tree.nodes[h];
            format!("treemap — {} ({})", n.name, format_size(n.size))
        }
        _ => "treemap".to_string(),
    };

    let block = theme::panel(&titulo, focused);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    // Guardamos el área interior para el hit-testing aunque no haya árbol.
    app.treemap_area = inner;

    let w = inner.width as usize;
    let h_pix = inner.height as usize * 2; // doble resolución vertical
    if w == 0 || h_pix == 0 {
        app.treemap_tiles.clear();
        return;
    }

    // Calculamos el treemap del directorio actual. El layout es propietario (no
    // referencia al árbol), así que el préstamo inmutable de `app` se libera al
    // salir de este bloque y podemos mutar `app` después.
    let (mut layout, sel, hov) = {
        let Some(tree) = app.tree() else {
            app.treemap_tiles.clear();
            return;
        };
        let layout = treemap::build(tree, app.current, w, h_pix, app.cushion);
        (layout, app.selected_node(), app.hover_node)
    };

    // Resaltados: primero el hover (amarillo) y encima la selección (blanco),
    // para que si coinciden gane visualmente la selección.
    if let Some(h) = hov {
        if let Some((_, r)) = layout.tiles.iter().find(|(n, _)| *n == h).copied() {
            dibujar_borde(&mut layout.colors, w, h_pix, r, HOVER);
        }
    }
    if let Some(s) = sel {
        if let Some((_, r)) = layout.tiles.iter().find(|(n, _)| *n == s).copied() {
            dibujar_borde(&mut layout.colors, w, h_pix, r, RESALTE);
        }
    }

    // Volcado a la pantalla con el carácter de medio bloque.
    let buf = frame.buffer_mut();
    for cy in 0..inner.height {
        for cx in 0..inner.width {
            let px = cx as usize;
            let arriba = layout.get(px, cy as usize * 2);
            let abajo = layout.get(px, cy as usize * 2 + 1);
            if let Some(cell) = buf.cell_mut((inner.x + cx, inner.y + cy)) {
                cell.set_symbol("▀");
                cell.set_fg(Color::Rgb(arriba.0, arriba.1, arriba.2));
                cell.set_bg(Color::Rgb(abajo.0, abajo.1, abajo.2));
            }
        }
    }

    // Guardamos los tiles de primer nivel para el hit-testing del mouse.
    app.treemap_tiles = layout.tiles;
}

/// Pinta el contorno (1 píxel) de un tile en la rejilla de colores.
fn dibujar_borde(colors: &mut [Rgb], width: usize, height: usize, r: TileRect, color: Rgb) {
    let set = |colors: &mut [Rgb], x: usize, y: usize| {
        if x < width && y < height {
            colors[y * width + x] = color;
        }
    };
    // Bordes superior e inferior.
    for x in r.x0..r.x1 {
        set(colors, x, r.y0);
        set(colors, x, r.y1.saturating_sub(1));
    }
    // Bordes izquierdo y derecho.
    for y in r.y0..r.y1 {
        set(colors, r.x0, y);
        set(colors, r.x1.saturating_sub(1), y);
    }
}
