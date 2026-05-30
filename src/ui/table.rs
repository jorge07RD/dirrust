//! Tabla principal: ranking del contenido del directorio actual.
//!
//! Muestra los hijos directos del directorio visible ordenados según el criterio
//! activo, con columnas nombre / tamaño / % del directorio / nº de archivos. La
//! fila seleccionada se resalta y se sincroniza con `App::selected`.

use ratatui::layout::{Constraint, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Text;
use ratatui::widgets::{Cell, Row, Table};
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::util::{format_count, format_size, palette_rgb};

// Colores de las celdas (truecolor, en armonía con el tema btop-like).
// REVISAR (contraste): la fila seleccionada se pinta con texto oscuro sobre un
// fondo de acento uniforme. Antes el resaltado dejaba el color propio de cada
// celda (verde/cian) sobre el fondo, dando combinaciones ilegibles; ahora
// forzamos un par de alto contraste para TODAS las celdas de esa fila.
use theme::{ACCENT as DIR_FG, GOOD as SIZE_FG, SEL_BG, SEL_FG, TEXT as FILE_FG};

/// Dibuja la tabla de ranking en el área dada. `focused` resalta el marco.
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect, focused: bool) {
    // Necesitamos el árbol; si aún no está listo no dibujamos filas.
    let Some(tree) = app.tree() else {
        frame.render_widget(theme::panel("contenido", focused), area);
        return;
    };

    // Tamaño total del directorio actual: base para el cálculo de porcentajes.
    let total_dir = tree.nodes[app.current].size.max(1); // evita división por 0

    let hijos = app.sorted_children();
    let total_filas = hijos.len();
    let sel = app.selected.min(total_filas.saturating_sub(1));

    // Construimos una fila por hijo. Conocemos la posición para poder dar a la
    // fila seleccionada un estilo de alto contraste celda a celda.
    let filas: Vec<Row> = hijos
        .iter()
        .enumerate()
        .map(|(pos, &idx)| {
            let node = &tree.nodes[idx];
            let seleccionada = pos == sel;
            let marcada = app.is_marked(idx);

            // Texto de cada columna. Los elementos MARCADOS llevan un "●" delante.
            let base = if node.is_dir {
                format!("{}/", node.name)
            } else {
                node.name.clone()
            };
            let nombre = if marcada { format!("● {base}") } else { base };
            let pct = node.size as f64 / total_dir as f64 * 100.0;

            // Colores: en la fila seleccionada todo va en negro (lo aporta el
            // estilo de fila). En el resto, los DIRECTORIOS usan el acento y los
            // ARCHIVOS el color de su extensión, EXACTAMENTE el mismo que reciben
            // en el panel "por extensión" (vía `palette_rgb`), para que el color
            // identifique el tipo de archivo de forma consistente en toda la UI.
            let (c_nombre, c_size) = if seleccionada {
                (SEL_FG, SEL_FG)
            } else if node.is_dir {
                (DIR_FG, SIZE_FG)
            } else {
                let (r, g, b) = palette_rgb(node.extension.as_deref());
                (Color::Rgb(r, g, b), SIZE_FG)
            };

            let fila = Row::new(vec![
                Cell::from(Text::from(nombre)).style(Style::default().fg(c_nombre)),
                Cell::from(Text::from(format_size(node.size))).style(Style::default().fg(c_size)),
                Cell::from(Text::from(format!("{pct:5.1}%")))
                    .style(Style::default().fg(if seleccionada { SEL_FG } else { FILE_FG })),
                Cell::from(Text::from(format_count(node.file_count)))
                    .style(Style::default().fg(if seleccionada { SEL_FG } else { FILE_FG })),
            ]);

            // El fondo claro y la negrita los aplica el estilo de fila para que
            // cubran toda la anchura (incluido el hueco entre columnas).
            if seleccionada {
                fila.style(Style::default().bg(SEL_BG).add_modifier(Modifier::BOLD))
            } else {
                fila
            }
        })
        .collect();

    // Encabezado de columnas: texto de acento sobre el fondo, estilo btop.
    let header = Row::new(vec![
        Cell::from("Nombre"),
        Cell::from("Tamaño"),
        Cell::from("%"),
        Cell::from("#Arch."),
    ])
    .style(
        Style::default()
            .fg(theme::ACCENT)
            .add_modifier(Modifier::BOLD),
    );

    // Anchos de columna: el nombre se lleva el espacio flexible; el resto fijo.
    let widths = [
        Constraint::Min(20),    // nombre
        Constraint::Length(12), // tamaño
        Constraint::Length(7),  // porcentaje
        Constraint::Length(12), // nº archivos
    ];

    let titulo = format!("contenido ({total_filas})");

    let table = Table::new(filas, widths)
        .header(header)
        .block(theme::panel(&titulo, focused))
        // El estilo de resaltado de ratatui lo dejamos vacío: ya estilamos la
        // fila seleccionada manualmente arriba. Solo conservamos el símbolo guía.
        .highlight_symbol("▶ ");

    // Guardamos el área donde se dibujan las FILAS de datos (excluyendo el borde
    // superior, la cabecera y el borde inferior) para el hit-testing del mouse.
    app.table_rows_area = Rect {
        x: area.x + 1,
        y: area.y + 2,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(3),
    };

    // Sincronizamos el estado de la tabla con la selección de la app antes de
    // renderizar; `table_state` conserva el desplazamiento entre frames.
    app.table_state
        .select(if total_filas == 0 { None } else { Some(sel) });
    frame.render_stateful_widget(table, area, &mut app.table_state);
}
