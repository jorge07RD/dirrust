//! Tema visual común, inspirado en el estilo de `btop`: bordes REDONDEADOS,
//! acento cian/teal, títulos resaltados y un look oscuro y pulido.
//!
//! Centralizar aquí los colores y la construcción de los marcos hace que todos
//! los paneles compartan el mismo aspecto y sea trivial cambiar el tema.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;
use ratatui::widgets::{Block, BorderType, Borders};

/// Acento principal (cian/teal), como el de btop. Bordes con foco y resaltes.
pub const ACCENT: Color = Color::Rgb(0, 205, 225);
/// Acento atenuado: títulos de paneles sin foco.
pub const ACCENT_DIM: Color = Color::Rgb(70, 130, 145);
/// Color de los bordes sin foco (gris azulado tenue).
pub const BORDER: Color = Color::Rgb(58, 66, 78);
/// Texto principal.
pub const TEXT: Color = Color::Rgb(220, 224, 228);
/// Texto secundario / tenue.
pub const DIM: Color = Color::Rgb(130, 140, 150);
/// Verde "bueno" (gradiente de barras / OK).
pub const GOOD: Color = Color::Rgb(70, 220, 120);
/// Amarillo "medio".
pub const WARN: Color = Color::Rgb(235, 205, 70);
/// Rojo "peligro" (gradiente alto / errores / borrado).
pub const BAD: Color = Color::Rgb(235, 80, 90);
/// Fondo de la fila/elemento seleccionado.
pub const SEL_BG: Color = Color::Rgb(0, 150, 175);
/// Texto sobre la selección.
pub const SEL_FG: Color = Color::Rgb(10, 14, 16);

/// Construye un marco redondeado con el título al estilo btop.
///
/// El título se resalta en el color de acento (vivo si el panel tiene el foco,
/// atenuado si no), y el borde sigue el mismo criterio.
pub fn panel(title: &str, focused: bool) -> Block<'static> {
    let color_titulo = if focused { ACCENT } else { ACCENT_DIM };
    let color_borde = if focused { ACCENT } else { BORDER };
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color_borde))
        .title(Span::styled(
            format!(" {title} "),
            Style::default()
                .fg(color_titulo)
                .add_modifier(Modifier::BOLD),
        ))
}

/// Igual que `panel` pero con un color de borde/título explícito (p. ej. rojo en
/// los modales de peligro). El título ya viene formateado por el llamador.
pub fn panel_colored(title: Span<'static>, color: Color) -> Block<'static> {
    Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(Style::default().fg(color))
        .title(title)
}

/// Estiliza una "tecla" de atajo: letra en acento sobre fondo tenue.
pub fn key(k: &str) -> Span<'static> {
    Span::styled(
        format!(" {k} "),
        Style::default()
            .fg(SEL_FG)
            .bg(ACCENT)
            .add_modifier(Modifier::BOLD),
    )
}

/// Color de gradiente verde→amarillo→rojo según una fracción en [0, 1], al
/// estilo de los gráficos de btop.
pub fn gradient(frac: f64) -> Color {
    let f = frac.clamp(0.0, 1.0);
    // Interpolamos en dos tramos: verde→amarillo (0..0.5) y amarillo→rojo (0.5..1).
    let (a, b, t) = if f < 0.5 {
        (GOOD, WARN, f / 0.5)
    } else {
        (WARN, BAD, (f - 0.5) / 0.5)
    };
    lerp(a, b, t)
}

/// Interpolación lineal entre dos colores RGB.
fn lerp(a: Color, b: Color, t: f64) -> Color {
    let (ar, ag, ab) = rgb(a);
    let (br, bg, bb) = rgb(b);
    let mix = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    Color::Rgb(mix(ar, br), mix(ag, bg), mix(ab, bb))
}

/// Extrae las componentes de un `Color::Rgb` (los del tema siempre lo son).
fn rgb(c: Color) -> (u8, u8, u8) {
    match c {
        Color::Rgb(r, g, b) => (r, g, b),
        _ => (255, 255, 255),
    }
}
