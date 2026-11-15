//! Modo mapa: treemap a pantalla completa con etiquetas, colores por categoría
//! y un panel lateral de detalle (selección, sugerencias, marcados y disco).
//!
//! El layout lo calcula `mapa::layout`; aquí solo se pinta celda a celda en el
//! buffer de ratatui y se componen los textos del panel.

use std::path::Path;

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use super::theme;
use crate::app::App;
use crate::mapa::{self, Categoria, MapTile};
use crate::model::Tree;
use crate::util::{format_count, format_size, Rgb};

/// Fondo del área del mapa (se ve en los canalones entre tiles de primer nivel).
const FONDO: Rgb = (14, 15, 19);
/// Texto principal sobre los tiles.
const TEXTO: Color = Color::Rgb(232, 235, 238);
/// Texto secundario (tamaños) sobre los tiles.
const TEXTO_TENUE: Color = Color::Rgb(160, 168, 178);
/// Contorno del tile seleccionado.
const SELECCION: Color = Color::Rgb(255, 255, 255);
/// Contorno del tile bajo el cursor.
const HOVER: Color = Color::Rgb(255, 214, 90);
/// Contorno de los tiles marcados para borrar.
const MARCADO: Color = Color::Rgb(230, 130, 220);
/// Ancho del panel lateral.
const ANCHO_PANEL: u16 = 40;

/// Dibuja el modo mapa en `area` (el cuerpo de la pantalla).
pub fn draw(frame: &mut Frame, app: &mut App, area: Rect) {
    app.ensure_worth();
    // El panel lateral solo cabe en terminales anchos; si no, todo es mapa.
    if area.width >= ANCHO_PANEL + 50 {
        let cols =
            Layout::horizontal([Constraint::Min(1), Constraint::Length(ANCHO_PANEL)]).split(area);
        draw_map(frame, app, cols[0]);
        draw_panel(frame, app, cols[1]);
    } else {
        draw_map(frame, app, area);
    }
}

// ---------------------------------------------------------------------------
// Mapa
// ---------------------------------------------------------------------------

fn draw_map(frame: &mut Frame, app: &mut App, area: Rect) {
    let block = theme::panel("mapa", true);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.height < 3 || inner.width < 4 {
        app.map_tiles.clear();
        app.map_area = Rect::default();
        return;
    }

    let leyenda = Rect { height: 1, ..inner };
    let zona = Rect {
        y: inner.y + 1,
        height: inner.height - 1,
        ..inner
    };
    app.map_area = zona;

    let Some(tree) = app.tree() else {
        app.map_tiles.clear();
        return;
    };
    frame.render_widget(Paragraph::new(linea_leyenda(tree, app.current)), leyenda);

    let tiles = mapa::layout(tree, app.current, zona);
    let sel = app.selected_node();
    let hov = app.hover_node;

    let buf = frame.buffer_mut();
    rellenar(buf, zona, FONDO);
    for t in &tiles {
        pintar_tile(buf, tree, t);
    }
    // Contornos, de menor a mayor prioridad: marcados, hover y selección.
    for t in tiles.iter().filter(|t| app.is_marked(t.node)) {
        contorno(buf, tree, t, MARCADO);
    }
    if let Some(t) = hov.and_then(|h| tiles.iter().find(|t| t.node == h)) {
        if Some(t.node) != sel {
            contorno(buf, tree, t, HOVER);
        }
    }
    if let Some(t) = sel.and_then(|s| tiles.iter().find(|t| t.depth == 0 && t.node == s)) {
        contorno(buf, tree, t, SELECCION);
    }

    app.map_tiles = tiles;
}

/// Primera fila del mapa: total del directorio y leyenda de categorías.
fn linea_leyenda(tree: &Tree, current: usize) -> Line<'static> {
    let n = &tree.nodes[current];
    let mut spans = vec![
        Span::styled(
            format!(" {} ", tam_corto(n.size)),
            Style::default().fg(TEXTO).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            format!("· {} archivos   ", compacto(n.file_count)),
            Style::default().fg(theme::DIM),
        ),
        Span::styled("╱", Style::default().fg(theme::DIM)),
        Span::styled(" recuperable  ", Style::default().fg(theme::DIM)),
    ];
    for c in Categoria::LEYENDA {
        spans.push(Span::styled("■", Style::default().fg(rgb(c.accent()))));
        spans.push(Span::styled(
            format!(" {}  ", c.label()),
            Style::default().fg(theme::DIM),
        ));
    }
    Line::from(spans)
}

/// Pinta el fondo, la etiqueta y (si es un directorio anidado) la cabecera.
fn pintar_tile(buf: &mut Buffer, tree: &Tree, t: &MapTile) {
    let r = t.rect;
    // Los niveles más profundos se aclaran un poco para que se lea el anidado.
    let cuerpo = escalar(t.cat.color(), (1.0 + 0.09 * t.depth as f64).min(1.45));
    let rayado = t.cat.recuperable();
    let fg_rayado = rgb(escalar(cuerpo, 1.4));
    let bg = rgb(cuerpo);
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(if rayado { "╱" } else { " " });
                cell.set_fg(fg_rayado);
                cell.set_bg(bg);
            }
        }
    }

    // Sin sitio para al menos "ab…" la etiqueta solo estorbaría.
    if r.width < 4 {
        return;
    }
    let nodo = &tree.nodes[t.node];
    let (x_txt, max_w) = zona_texto(r);
    let nombre = truncar(tree.name(t.node), max_w);
    let tam = tam_corto(nodo.size);
    let estilo_nombre = Style::default().fg(TEXTO).add_modifier(Modifier::BOLD);
    let estilo_tam = Style::default().fg(TEXTO_TENUE);

    if t.header {
        // Cabecera: una fila algo más clara, sin rayado, con nombre y tamaño.
        let bg_cab = rgb(escalar(cuerpo, 1.22));
        for x in r.left()..r.right() {
            if let Some(cell) = buf.cell_mut((x, r.y)) {
                cell.set_symbol(" ");
                cell.set_bg(bg_cab);
            }
        }
        buf.set_stringn(x_txt, r.y, &nombre, max_w, estilo_nombre);
        let w_nombre = nombre.chars().count();
        let w_tam = tam.chars().count();
        if w_nombre + 2 + w_tam <= max_w {
            // En el primer nivel el tamaño va a la derecha; dentro, tras el nombre.
            let x_tam = if t.depth == 0 {
                x_txt + (max_w - w_tam) as u16
            } else {
                x_txt + (w_nombre + 2) as u16
            };
            buf.set_stringn(x_tam, r.y, &tam, w_tam, estilo_tam);
        }
    } else {
        let tam_cabe = r.height >= 2 && tam.chars().count() <= max_w;
        if rayado {
            // Hueco liso tras la etiqueta para que el rayado no se pegue al texto.
            limpiar(buf, r, r.y, x_txt + nombre.chars().count() as u16 + 1);
            if tam_cabe {
                limpiar(buf, r, r.y + 1, x_txt + tam.chars().count() as u16 + 1);
            }
        }
        buf.set_stringn(x_txt, r.y, &nombre, max_w, estilo_nombre);
        // El tamaño se muestra entero o no se muestra ("222Mi" confunde).
        if tam_cabe {
            buf.set_stringn(x_txt, r.y + 1, &tam, max_w, estilo_tam);
        }
    }
}

/// Columna donde empieza el texto de un tile y ancho disponible (deja una celda
/// a cada lado si hay sitio, para que el contorno no pise la etiqueta).
fn zona_texto(r: Rect) -> (u16, usize) {
    if r.width >= 6 {
        (r.x + 1, (r.width - 2) as usize)
    } else {
        (r.x, r.width as usize)
    }
}

/// Dibuja el contorno de un tile con líneas finas, sin tapar su etiqueta.
fn contorno(buf: &mut Buffer, tree: &Tree, t: &MapTile, color: Color) {
    let r = t.rect;
    if r.width < 6 || r.height < 3 {
        // Demasiado pequeño para un marco: lo resaltamos invirtiendo colores.
        for y in r.top()..r.bottom() {
            for x in r.left()..r.right() {
                if let Some(cell) = buf.cell_mut((x, y)) {
                    cell.set_bg(color);
                    cell.set_fg(Color::Rgb(10, 12, 14));
                }
            }
        }
        return;
    }
    let (x0, y0) = (r.x, r.y);
    let (x1, y1) = (r.right() - 1, r.bottom() - 1);
    // Zona de la fila superior ocupada por el nombre (no se pisa).
    let (x_txt, max_w) = zona_texto(r);
    let w_nombre = truncar(tree.name(t.node), max_w).chars().count() as u16;
    let estilo = Style::default().fg(color).add_modifier(Modifier::BOLD);

    let mut poner = |x: u16, y: u16, s: &str, solo_vacia: bool| {
        if let Some(cell) = buf.cell_mut((x, y)) {
            if solo_vacia && !matches!(cell.symbol(), " " | "╱") {
                return;
            }
            cell.set_symbol(s);
            cell.set_style(estilo);
        }
    };
    for x in x0 + 1..x1 {
        let sobre_nombre = x >= x_txt && x < x_txt + w_nombre + 1;
        if !sobre_nombre {
            poner(x, y0, "─", true);
        }
        poner(x, y1, "─", false);
    }
    for y in y0 + 1..y1 {
        poner(x0, y, "│", false);
        poner(x1, y, "│", false);
    }
    poner(x0, y0, "┌", false);
    poner(x1, y0, "┐", false);
    poner(x0, y1, "└", false);
    poner(x1, y1, "┘", false);
    // Re-estilizamos el nombre para que destaque igual que el marco.
    for x in x_txt..x_txt + w_nombre {
        if let Some(cell) = buf.cell_mut((x, y0)) {
            cell.set_fg(color);
        }
    }
}

/// Borra el rayado de la fila `y` del tile `r` desde su borde izquierdo hasta
/// la columna `hasta` (exclusiva).
fn limpiar(buf: &mut Buffer, r: Rect, y: u16, hasta: u16) {
    for x in r.left()..hasta.min(r.right()) {
        if let Some(cell) = buf.cell_mut((x, y)) {
            cell.set_symbol(" ");
        }
    }
}

fn rellenar(buf: &mut Buffer, r: Rect, color: Rgb) {
    for y in r.top()..r.bottom() {
        for x in r.left()..r.right() {
            if let Some(cell) = buf.cell_mut((x, y)) {
                cell.set_symbol(" ");
                cell.set_bg(rgb(color));
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Panel lateral
// ---------------------------------------------------------------------------

fn draw_panel(frame: &mut Frame, app: &App, area: Rect) {
    let block = theme::panel("detalle", false);
    let inner = block.inner(area);
    frame.render_widget(block, area);
    let Some(tree) = app.tree() else { return };
    let partes = Layout::vertical([Constraint::Min(1), Constraint::Length(5)]).split(inner);
    let w = inner.width as usize;

    let mut l: Vec<Line> = Vec::new();
    seccion_seleccion(&mut l, app, tree, w);
    l.push(Line::from(""));
    seccion_sugerencias(&mut l, app, tree, w);
    l.push(Line::from(""));
    seccion_marcados(&mut l, app, tree, w);
    frame.render_widget(Paragraph::new(l), partes[0]);

    let mut d: Vec<Line> = Vec::new();
    seccion_disco(&mut d, &app.root_path, w);
    frame.render_widget(Paragraph::new(d), partes[1]);
}

fn seccion_seleccion(l: &mut Vec<Line>, app: &App, tree: &Tree, w: usize) {
    l.push(titulo_seccion("SELECCIÓN", None, w));
    let Some(sel) = app.selected_node() else {
        l.push(tenue("  (nada seleccionado)"));
        return;
    };
    let n = &tree.nodes[sel];
    let cat = mapa::categoria_de(tree, sel);
    l.push(Line::from(vec![
        Span::styled("▌ ", Style::default().fg(rgb(cat.accent()))),
        Span::styled(
            truncar(tree.name(sel), w.saturating_sub(2)),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
    ]));
    let ruta = ruta_corta(&tree.path_of(sel, &app.root_path));
    l.push(tenue(&format!(
        "  {}",
        truncar_izq(&ruta, w.saturating_sub(2))
    )));
    l.push(Line::from(""));

    // Tamaño grande: número resaltado + unidad tenue.
    let txt = format_size(n.size);
    let (num, unidad) = txt.split_once(' ').unwrap_or((&txt, ""));
    l.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            num.to_string(),
            Style::default()
                .fg(theme::ACCENT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {unidad}"), Style::default().fg(theme::DIM)),
    ]));
    let total = tree.total_size().max(1);
    let frac = n.size as f64 / total as f64;
    l.push(barra(frac, w.saturating_sub(4), rgb(cat.accent())));
    l.push(Line::from(""));
    l.push(dos_columnas("DEL ESCANEO", "ARCHIVOS", w, true));
    l.push(dos_columnas(
        &format!("{:.1}%", frac * 100.0),
        &compacto(n.file_count),
        w,
        false,
    ));
    l.push(dos_columnas("TIPO", "RECUPERABLE", w, true));
    let recup = if cat.recuperable() {
        format!("sí · {}", cat.motivo())
    } else {
        "no".to_string()
    };
    l.push(dos_columnas(cat.label(), &recup, w, false));
    l.push(Line::from(""));
    let marcado = app.is_marked(sel);
    let lbl = |t: &'static str| Span::styled(t, Style::default().fg(theme::DIM));
    let mut acciones = vec![Span::raw("  ")];
    if n.is_dir {
        acciones.push(theme::key("⏎"));
        acciones.push(lbl(" abrir "));
    }
    acciones.push(theme::key("Spc"));
    acciones.push(lbl(if marcado { " desmarcar " } else { " marcar " }));
    acciones.push(theme::key("d"));
    acciones.push(lbl(" borrar"));
    l.push(Line::from(acciones));
    if marcado {
        l.push(Line::from(Span::styled(
            "  ● marcado para borrar",
            Style::default().fg(MARCADO),
        )));
    }
}

fn seccion_sugerencias(l: &mut Vec<Line>, app: &App, tree: &Tree, w: usize) {
    let total: u64 = app.worth.iter().map(|&(n, _)| tree.nodes[n].size).sum();
    l.push(titulo_seccion(
        "VALE LA PENA MIRAR",
        Some(&tam_corto(total)),
        w,
    ));
    if app.worth.is_empty() {
        l.push(tenue("  nada recuperable a la vista"));
        return;
    }
    let mayor = app
        .worth
        .first()
        .map(|&(n, _)| tree.nodes[n].size)
        .unwrap_or(1)
        .max(1);
    for &(n, cat) in app.worth.iter().take(6) {
        let tam = tam_corto(tree.nodes[n].size);
        let nombre = truncar_izq(
            &ruta_relativa(tree, n, app.current),
            w.saturating_sub(tam.chars().count() + 4),
        );
        let hueco = w.saturating_sub(nombre.chars().count() + tam.chars().count() + 3);
        let color = rgb(cat.accent());
        l.push(Line::from(vec![
            Span::styled(" ▏", Style::default().fg(color)),
            Span::styled(nombre, Style::default().fg(theme::TEXT)),
            Span::raw(" ".repeat(hueco)),
            Span::styled(tam, Style::default().fg(theme::TEXT)),
        ]));
        // Segunda línea: motivo + barra relativa a la mayor sugerencia.
        let ancho_barra = 10usize.min(w.saturating_sub(4));
        let llenas = ((tree.nodes[n].size as f64 / mayor as f64) * ancho_barra as f64)
            .round()
            .max(1.0) as usize;
        let motivo = cat.motivo();
        let hueco = w.saturating_sub(motivo.chars().count() + ancho_barra + 4);
        l.push(Line::from(vec![
            Span::styled(" ▏", Style::default().fg(color)),
            Span::styled(motivo.to_string(), Style::default().fg(theme::DIM)),
            Span::raw(" ".repeat(hueco)),
            Span::styled("▬".repeat(llenas), Style::default().fg(color)),
            Span::styled(
                "▬".repeat(ancho_barra - llenas.min(ancho_barra)),
                Style::default().fg(theme::BORDER),
            ),
        ]));
    }
}

fn seccion_marcados(l: &mut Vec<Line>, app: &App, tree: &Tree, w: usize) {
    let (n, total) = app.marked_summary();
    let resumen = if n > 0 {
        Some(format!("{n} · {}", tam_corto(total)))
    } else {
        None
    };
    l.push(titulo_seccion("MARCADOS", resumen.as_deref(), w));
    if n == 0 {
        l.push(tenue("  Espacio marca el tile seleccionado"));
        return;
    }
    for idx in app.marked_sorted().into_iter().take(4) {
        let Some(nodo) = tree.nodes.get(idx) else {
            continue;
        };
        let tam = tam_corto(nodo.size);
        let nombre = truncar(tree.name(idx), w.saturating_sub(tam.chars().count() + 5));
        let hueco = w.saturating_sub(nombre.chars().count() + tam.chars().count() + 3);
        l.push(Line::from(vec![
            Span::styled("  ● ", Style::default().fg(MARCADO)),
            Span::styled(nombre, Style::default().fg(theme::TEXT)),
            Span::raw(" ".repeat(hueco.saturating_sub(1))),
            Span::styled(tam, Style::default().fg(theme::DIM)),
        ]));
    }
    if n > 4 {
        l.push(tenue(&format!("  … y {} más", n - 4)));
    }
    l.push(Line::from(vec![
        Span::raw("  "),
        theme::key("D"),
        Span::styled(" borrar los marcados", Style::default().fg(theme::DIM)),
    ]));
}

fn seccion_disco(l: &mut Vec<Line>, root: &Path, w: usize) {
    let Some((usado, libre, total)) = mapa::uso_disco(root) else {
        return;
    };
    l.push(titulo_seccion("DISCO", None, w));
    let txt = format_size(libre);
    let (num, unidad) = txt.split_once(' ').unwrap_or((&txt, ""));
    l.push(Line::from(vec![
        Span::raw("  "),
        Span::styled(
            num.to_string(),
            Style::default()
                .fg(theme::TEXT)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {unidad} libres"), Style::default().fg(theme::DIM)),
    ]));
    let frac = usado as f64 / total.max(1) as f64;
    l.push(barra(frac, w.saturating_sub(4), theme::gradient(frac)));
    l.push(dos_columnas(
        &format!("{} usados", tam_corto(usado)),
        &format!("{} total", tam_corto(total)),
        w,
        true,
    ));
}

// ---------------------------------------------------------------------------
// Utilidades de texto
// ---------------------------------------------------------------------------

/// Título de sección en versalitas tenues, con un valor opcional a la derecha.
fn titulo_seccion(titulo: &str, derecha: Option<&str>, w: usize) -> Line<'static> {
    let mut spans = vec![Span::styled(
        format!(" {titulo}"),
        Style::default()
            .fg(theme::ACCENT_DIM)
            .add_modifier(Modifier::BOLD),
    )];
    if let Some(d) = derecha {
        let hueco = w.saturating_sub(titulo.chars().count() + d.chars().count() + 2);
        spans.push(Span::raw(" ".repeat(hueco)));
        spans.push(Span::styled(
            d.to_string(),
            Style::default().fg(theme::TEXT),
        ));
    }
    Line::from(spans)
}

/// Dos valores en columnas (mitad izquierda / mitad derecha del panel).
fn dos_columnas(a: &str, b: &str, w: usize, etiqueta: bool) -> Line<'static> {
    let estilo = if etiqueta {
        Style::default().fg(theme::DIM)
    } else {
        Style::default().fg(theme::TEXT)
    };
    let mitad = w / 2;
    let a = truncar(a, mitad.saturating_sub(3));
    let hueco = mitad.saturating_sub(a.chars().count() + 2);
    Line::from(vec![
        Span::styled(format!("  {a}"), estilo),
        Span::raw(" ".repeat(hueco)),
        Span::styled(truncar(b, w - mitad), estilo),
    ])
}

/// Barra de progreso horizontal de `ancho` celdas.
fn barra(frac: f64, ancho: usize, color: Color) -> Line<'static> {
    let llenas = ((frac.clamp(0.0, 1.0) * ancho as f64).round() as usize).min(ancho);
    Line::from(vec![
        Span::raw("  "),
        Span::styled("━".repeat(llenas), Style::default().fg(color)),
        Span::styled(
            "━".repeat(ancho - llenas),
            Style::default().fg(theme::BORDER),
        ),
    ])
}

fn tenue(s: &str) -> Line<'static> {
    Line::from(Span::styled(s.to_string(), Style::default().fg(theme::DIM)))
}

/// Recorta por la derecha con "…" si no cabe en `max` caracteres.
fn truncar(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out: String = s.chars().take(max - 1).collect();
    out.push('…');
    out
}

/// Recorta por la IZQUIERDA (para rutas: lo importante es el final).
fn truncar_izq(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    let mut out = String::from("…");
    out.extend(s.chars().skip(n - (max - 1)));
    out
}

/// Ruta con `$HOME` abreviado a `~`.
fn ruta_corta(p: &Path) -> String {
    let s = p.display().to_string();
    match std::env::var("HOME") {
        Ok(home) if !home.is_empty() && s.starts_with(&home) => format!("~{}", &s[home.len()..]),
        _ => s,
    }
}

/// Ruta de `node` relativa al directorio `base` (p. ej. "share/Trash").
fn ruta_relativa(tree: &Tree, node: usize, base: usize) -> String {
    let mut partes = Vec::new();
    let mut cur = node;
    while cur != base {
        partes.push(tree.name(cur));
        match tree.nodes[cur].parent {
            Some(p) => cur = p,
            None => break,
        }
    }
    partes.reverse();
    partes.join("/")
}

/// Tamaño compacto para etiquetas: "337GiB", "8.6GiB", "512B".
fn tam_corto(b: u64) -> String {
    const U: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];
    let mut v = b as f64;
    let mut i = 0;
    while v >= 1024.0 && i < U.len() - 1 {
        v /= 1024.0;
        i += 1;
    }
    if i == 0 {
        format!("{b}B")
    } else if v < 10.0 {
        format!("{v:.1}{}", U[i])
    } else {
        format!("{v:.0}{}", U[i])
    }
}

/// Conteo compacto: "789.3k", "1.8M" (o el número completo si es pequeño).
fn compacto(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}k", n as f64 / 1_000.0)
    } else {
        format_count(n)
    }
}

fn escalar((r, g, b): Rgb, f: f64) -> Rgb {
    let s = |c: u8| (c as f64 * f).round().clamp(0.0, 255.0) as u8;
    (s(r), s(g), s(b))
}

fn rgb((r, g, b): Rgb) -> Color {
    Color::Rgb(r, g, b)
}
