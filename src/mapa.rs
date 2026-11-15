//! Modo "mapa": treemap a pantalla completa, con etiquetas y coloreado por
//! CATEGORÍA (código, toolchains, caché, papelera…) en lugar de por extensión.
//!
//! A diferencia de `treemap.rs` (rejilla de píxeles de medio bloque, estilo
//! WinDirStat), aquí trabajamos directamente en CELDAS del terminal: cada tile
//! es un `Rect` de celdas, lo que permite escribir dentro el nombre y el tamaño.
//! Los directorios con sitio suficiente reservan una fila de CABECERA (nombre +
//! tamaño) y anidan a sus hijos debajo, igual que un treemap "con marcos".
//!
//! Este módulo solo calcula (layout, clasificación, sugerencias); el dibujado
//! vive en `ui/map_view.rs`.

use std::path::Path;

use ratatui::layout::Rect;

use crate::model::Tree;
use crate::treemap::{hijos_ordenados, squarify_node, RectF};
use crate::util::Rgb;

/// Profundidad máxima de anidado del mapa (por legibilidad y coste).
const PROF_MAX: u8 = 8;

/// Categoría de un archivo o carpeta, deducida de su nombre, su extensión y la
/// categoría de su carpeta padre.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Categoria {
    Codigo,
    Agente,
    Toolchain,
    Sincronizado,
    Git,
    Multimedia,
    Documentos,
    Cache,
    Papelera,
    Build,
    Otros,
}

impl Categoria {
    /// Orden de la leyenda.
    /// (Las recuperables primero: son las que más interesa localizar.)
    pub const LEYENDA: [Categoria; 10] = [
        Categoria::Cache,
        Categoria::Papelera,
        Categoria::Build,
        Categoria::Codigo,
        Categoria::Agente,
        Categoria::Toolchain,
        Categoria::Sincronizado,
        Categoria::Git,
        Categoria::Multimedia,
        Categoria::Documentos,
    ];

    /// Nombre legible.
    pub fn label(self) -> &'static str {
        match self {
            Categoria::Codigo => "Código",
            Categoria::Agente => "Agentes",
            Categoria::Toolchain => "Toolchains",
            Categoria::Sincronizado => "Sincronizado",
            Categoria::Git => "Git",
            Categoria::Multimedia => "Multimedia",
            Categoria::Documentos => "Documentos",
            Categoria::Cache => "Caché",
            Categoria::Papelera => "Papelera",
            Categoria::Build => "Build",
            Categoria::Otros => "Otros",
        }
    }

    /// Color de fondo base (tonos apagados para que el texto se lea encima).
    pub fn color(self) -> Rgb {
        match self {
            Categoria::Codigo => (44, 62, 96),
            Categoria::Agente => (112, 70, 40),
            Categoria::Toolchain => (36, 80, 62),
            Categoria::Sincronizado => (30, 86, 100),
            Categoria::Git => (112, 44, 58),
            Categoria::Multimedia => (72, 48, 100),
            Categoria::Documentos => (72, 74, 82),
            Categoria::Cache => (100, 84, 38),
            Categoria::Papelera => (98, 74, 46),
            Categoria::Build => (50, 66, 104),
            Categoria::Otros => (52, 57, 66),
        }
    }

    /// Color de acento (más vivo) para la leyenda y las barras del panel.
    pub fn accent(self) -> Rgb {
        let (r, g, b) = self.color();
        let s = |c: u8| ((c as f64) * 1.9).min(255.0) as u8;
        (s(r), s(g), s(b))
    }

    /// ¿Se puede borrar sin perder nada importante (se regenera o es basura)?
    pub fn recuperable(self) -> bool {
        matches!(
            self,
            Categoria::Cache | Categoria::Papelera | Categoria::Build
        )
    }

    /// Motivo corto por el que merece la pena mirarlo (solo recuperables).
    pub fn motivo(self) -> &'static str {
        match self {
            Categoria::Cache => "regenerable",
            Categoria::Papelera => "papelera",
            Categoria::Build => "salida de build",
            _ => "",
        }
    }

    /// Categorías "fuertes": todo lo que cuelga de ellas hereda la categoría.
    fn absorbente(self) -> bool {
        matches!(
            self,
            Categoria::Cache | Categoria::Papelera | Categoria::Build | Categoria::Git
        )
    }

    /// Categorías "contenedoras": sus hijos heredan salvo que sean recuperables
    /// (p. ej. `~/.cargo/registry/cache` sigue siendo caché).
    fn contenedora(self) -> bool {
        matches!(
            self,
            Categoria::Toolchain | Categoria::Agente | Categoria::Sincronizado
        )
    }
}

/// Clasifica el nodo `idx` sabiendo la categoría de su padre.
///
/// REVISAR: heurística por nombre. Es deliberadamente sencilla y barata (no lee
/// el disco): solo mira el nombre, la extensión y los nombres de los hermanos o
/// hijos directos (p. ej. un `target/` junto a un `Cargo.toml`).
pub fn clasificar(tree: &Tree, idx: usize, padre: Categoria) -> Categoria {
    if padre.absorbente() {
        return padre;
    }
    let nodo = &tree.nodes[idx];
    let propia = if nodo.is_dir {
        regla_dir(tree, idx, padre)
    } else if padre == Categoria::Otros {
        regla_ext(tree.ext(idx))
    } else {
        None
    };
    match propia {
        Some(c) if padre.contenedora() && !c.recuperable() => padre,
        Some(c) => c,
        None => padre,
    }
}

/// Categoría del nodo `idx` calculada desde la raíz (recorre sus ancestros).
pub fn categoria_de(tree: &Tree, idx: usize) -> Categoria {
    let mut cadena = vec![idx];
    let mut cur = idx;
    while let Some(p) = tree.nodes[cur].parent {
        cadena.push(p);
        cur = p;
    }
    let mut cat = Categoria::Otros;
    // La raíz del escaneo no tiene padre: la clasificamos igual que el resto.
    for &n in cadena.iter().rev() {
        cat = clasificar(tree, n, cat);
    }
    cat
}

/// ¿Tiene `idx` un hijo directo con alguno de esos nombres?
fn tiene_hijo(tree: &Tree, idx: usize, nombres: &[&str]) -> bool {
    tree.nodes[idx]
        .children
        .iter()
        .any(|&c| nombres.contains(&tree.name(c)))
}

/// Ficheros que delatan la raíz de un proyecto de código.
const MARCAS_PROYECTO: &[&str] = &[
    ".git",
    "Cargo.toml",
    "package.json",
    "go.mod",
    "pyproject.toml",
    "pom.xml",
    "build.gradle",
    "build.gradle.kts",
    "CMakeLists.txt",
    "composer.json",
    "Gemfile",
    "mix.exs",
];

/// Regla por nombre para directorios.
fn regla_dir(tree: &Tree, idx: usize, padre: Categoria) -> Option<Categoria> {
    let nombre = tree.name(idx).to_lowercase();
    let n = nombre.as_str();

    if matches!(n, "trash" | ".trash" | "$recycle.bin") || n.starts_with(".trash-") {
        return Some(Categoria::Papelera);
    }
    if n == ".git" {
        return Some(Categoria::Git);
    }
    if matches!(
        n,
        "node_modules"
            | "__pycache__"
            | ".pytest_cache"
            | ".mypy_cache"
            | ".ruff_cache"
            | ".next"
            | ".nuxt"
            | ".turbo"
            | ".parcel-cache"
            | ".svelte-kit"
            | ".gradle-build"
    ) {
        return Some(Categoria::Build);
    }
    // `target/`, `dist/`, `build/`… solo cuentan como build dentro de un proyecto.
    if matches!(n, "target" | "dist" | "build" | "out" | ".venv" | "venv") {
        let junto_a_proyecto = tree.nodes[idx]
            .parent
            .is_some_and(|p| tiene_hijo(tree, p, MARCAS_PROYECTO));
        if junto_a_proyecto || padre == Categoria::Codigo {
            return Some(Categoria::Build);
        }
    }
    if matches!(
        n,
        ".cache"
            | "cache"
            | "caches"
            | "_cacache"
            | "cachestorage"
            | "code cache"
            | "gpucache"
            | "shadercache"
            | "dawncache"
            | "grshadercache"
            | "cacheddata"
            | "cachedextensionvsixs"
            | ".tmp"
            | "tmp"
    ) {
        return Some(Categoria::Cache);
    }
    if matches!(
        n,
        ".claude" | ".codex" | ".cursor" | ".gemini" | ".aider" | ".continue" | ".windsurf"
    ) {
        return Some(Categoria::Agente);
    }
    if matches!(
        n,
        ".rustup"
            | ".cargo"
            | "mise"
            | ".nvm"
            | ".pyenv"
            | ".rbenv"
            | ".sdkman"
            | ".gradle"
            | ".m2"
            | ".bun"
            | ".deno"
            | ".npm"
            | ".pnpm-store"
            | ".dotnet"
            | ".nuget"
            | ".android"
            | "android"
            | "sdk"
            | "flatpak"
            | "toolchains"
            | "go"
            | ".go"
            | ".ghcup"
            | ".julia"
            | ".conda"
            | "miniconda3"
            | "anaconda3"
    ) {
        return Some(Categoria::Toolchain);
    }
    if matches!(
        n,
        "dropbox"
            | "onedrive"
            | "nextcloud"
            | "google drive"
            | "icloud drive"
            | "syncthing"
            | "sync"
    ) {
        return Some(Categoria::Sincronizado);
    }
    if matches!(
        n,
        "pictures"
            | "imágenes"
            | "imagenes"
            | "fotos"
            | "photos"
            | "videos"
            | "vídeos"
            | "music"
            | "música"
            | "musica"
            | "steam"
            | "games"
            | "juegos"
    ) {
        return Some(Categoria::Multimedia);
    }
    if matches!(
        n,
        "documents" | "documentos" | "desktop" | "escritorio" | "downloads" | "descargas"
    ) {
        return Some(Categoria::Documentos);
    }
    if matches!(
        n,
        "projects" | "proyectos" | "code" | "dev" | "src" | "repos" | "workspace" | "git"
    ) || tiene_hijo(tree, idx, MARCAS_PROYECTO)
    {
        return Some(Categoria::Codigo);
    }
    None
}

/// Regla por extensión para archivos sueltos (fuera de una carpeta clasificada).
fn regla_ext(ext: Option<&str>) -> Option<Categoria> {
    let e = ext?;
    let media = [
        "mp4", "mkv", "avi", "mov", "webm", "mp3", "flac", "wav", "ogg", "m4a", "jpg", "jpeg",
        "png", "gif", "webp", "heic", "raw", "cr2", "nef", "iso",
    ];
    let docs = [
        "pdf", "doc", "docx", "odt", "xls", "xlsx", "ods", "ppt", "pptx", "odp", "txt", "md",
        "epub", "csv",
    ];
    let code = [
        "rs", "py", "js", "ts", "tsx", "jsx", "go", "c", "h", "cpp", "hpp", "java", "kt", "rb",
        "php", "swift", "cs", "lua", "sh", "toml", "json", "yaml", "yml",
    ];
    if media.contains(&e) {
        Some(Categoria::Multimedia)
    } else if docs.contains(&e) {
        Some(Categoria::Documentos)
    } else if code.contains(&e) {
        Some(Categoria::Codigo)
    } else {
        None
    }
}

/// Un tile del mapa, en coordenadas ABSOLUTAS de celda.
#[derive(Debug, Clone, Copy)]
pub struct MapTile {
    pub node: usize,
    pub rect: Rect,
    /// 0 = hijo directo del directorio mostrado.
    pub depth: u8,
    pub cat: Categoria,
    /// `true` si es un directorio subdividido (tiene fila de cabecera y anida
    /// a sus hijos debajo).
    pub header: bool,
}

/// Calcula el mapa del directorio `current` dentro de `area`.
///
/// Los tiles salen en orden de PINTADO: cada padre antes que sus hijos.
pub fn layout(tree: &Tree, current: usize, area: Rect) -> Vec<MapTile> {
    let mut out = Vec::new();
    if area.width == 0 || area.height == 0 || current >= tree.nodes.len() {
        return out;
    }
    let cat = categoria_de(tree, current);
    subdividir(tree, current, area, 0, cat, &mut out);
    out
}

/// REVISAR: reparte `area` entre los hijos de `dir` (squarified) y recursa en
/// los directorios con sitio. La altura se cuenta DOBLE al hacer el squarify
/// porque una celda es ~2 veces más alta que ancha: así los tiles se ven
/// cuadrados en pantalla y no alargados.
fn subdividir(
    tree: &Tree,
    dir: usize,
    area: Rect,
    depth: u8,
    cat_dir: Categoria,
    out: &mut Vec<MapTile>,
) {
    let hijos = hijos_ordenados(tree, dir);
    if hijos.is_empty() || area.width == 0 || area.height == 0 {
        return;
    }
    let rf = RectF {
        x: 0.0,
        y: 0.0,
        w: area.width as f64,
        h: area.height as f64 * 2.0,
    };
    for (child, r) in squarify_node(tree, &hijos, rf, tree.nodes[dir].size as f64) {
        let x0 = (r.x.round() as u16).min(area.width);
        let x1 = ((r.x + r.w).round() as u16).min(area.width);
        let y0 = ((r.y / 2.0).round() as u16).min(area.height);
        let y1 = (((r.y + r.h) / 2.0).round() as u16).min(area.height);
        if x1 <= x0 || y1 <= y0 {
            continue;
        }
        let mut rect = Rect {
            x: area.x + x0,
            y: area.y + y0,
            width: x1 - x0,
            height: y1 - y0,
        };
        // Canalón de 1 celda a la derecha y abajo: deja ver el fondo del padre
        // y separa tiles vecinos del mismo color.
        if rect.width >= 4 {
            rect.width -= 1;
        }
        if rect.height >= 3 {
            rect.height -= 1;
        }

        let cat = clasificar(tree, child, cat_dir);
        let nodo = &tree.nodes[child];
        // Lo recuperable (caché, papelera, build) se muestra como UN bloque: se
        // borra entero, así que su interior solo añadiría ruido.
        let subdividir_hijo = nodo.is_dir
            && !cat.recuperable()
            && depth < PROF_MAX
            && rect.width >= 8
            && rect.height >= 3
            && nodo.children.iter().any(|&c| tree.nodes[c].size > 0);
        out.push(MapTile {
            node: child,
            rect,
            depth,
            cat,
            header: subdividir_hijo,
        });
        if subdividir_hijo {
            // Debajo de la cabecera, con 1 columna de margen a la izquierda para
            // que se aprecie el anidado (y para que el contorno de selección no
            // pise el contenido de los hijos).
            let cuerpo = Rect {
                x: rect.x + 1,
                y: rect.y + 1,
                width: rect.width - 1,
                height: rect.height - 1,
            };
            subdividir(tree, child, cuerpo, depth + 1, cat, out);
        }
    }
}

/// Carpetas RECUPERABLES (caché, papelera, build) dentro del directorio
/// `current`, de mayor a menor. Solo devuelve las de más arriba: si `.cache` es
/// caché, no lista además sus subcarpetas.
pub fn vale_la_pena(tree: &Tree, current: usize, max: usize) -> Vec<(usize, Categoria)> {
    if current >= tree.nodes.len() {
        return Vec::new();
    }
    let cat0 = categoria_de(tree, current);
    if cat0.recuperable() {
        return vec![(current, cat0)];
    }
    let mut out = Vec::new();
    let mut pila: Vec<(usize, Categoria)> = tree.nodes[current]
        .children
        .iter()
        .map(|&c| (c, cat0))
        .collect();
    while let Some((n, cat_padre)) = pila.pop() {
        let nodo = &tree.nodes[n];
        // Solo carpetas: un archivo suelto rara vez es "una carpeta a revisar".
        if !nodo.is_dir || nodo.size == 0 {
            continue;
        }
        let cat = clasificar(tree, n, cat_padre);
        if cat.recuperable() {
            out.push((n, cat));
        } else {
            pila.extend(nodo.children.iter().map(|&c| (c, cat)));
        }
    }
    out.sort_by_key(|&(n, _)| std::cmp::Reverse(tree.nodes[n].size));
    out.truncate(max);
    out
}

/// Uso del sistema de archivos que contiene `path`: (usado, libre, total) en
/// bytes. `None` si `statvfs` falla.
pub fn uso_disco(path: &Path) -> Option<(u64, u64, u64)> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let c = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `c` es una cadena C válida y `st` se inicializa por completo con
    // `statvfs` antes de leerla (comprobamos el código de retorno).
    let st = unsafe {
        let mut st: libc::statvfs = std::mem::zeroed();
        if libc::statvfs(c.as_ptr(), &mut st) != 0 {
            return None;
        }
        st
    };
    let frsize = st.f_frsize as u64;
    let total = st.f_blocks as u64 * frsize;
    let libre = st.f_bavail as u64 * frsize;
    let usado = total.saturating_sub(st.f_bfree as u64 * frsize);
    Some((usado, libre, total))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn arbol() -> (Tree, usize, usize, usize) {
        // root/
        //   proj/ (Cargo.toml, src/main.rs, target/ 800)
        //   .cache/ 300
        //   foto.jpg 100
        let mut t = Tree::with_root(Path::new("/home/u"), true, 0);
        let proj = t.add_child(t.root, "proj", true, 0);
        t.add_child(proj, "Cargo.toml", false, 10);
        let src = t.add_child(proj, "src", true, 0);
        t.add_child(src, "main.rs", false, 90);
        let target = t.add_child(proj, "target", true, 0);
        t.add_child(target, "bin", false, 800);
        let cache = t.add_child(t.root, ".cache", true, 0);
        t.add_child(cache, "x", false, 300);
        let foto = t.add_child(t.root, "foto.jpg", false, 100);
        t.aggregate();
        (t, target, cache, foto)
    }

    #[test]
    fn clasifica_por_nombre_y_hereda() {
        let (t, target, cache, foto) = arbol();
        assert_eq!(categoria_de(&t, target), Categoria::Build);
        assert_eq!(categoria_de(&t, cache), Categoria::Cache);
        assert_eq!(categoria_de(&t, foto), Categoria::Multimedia);
        // Lo que cuelga de una caché es caché.
        let x = t.nodes[cache].children[0];
        assert_eq!(categoria_de(&t, x), Categoria::Cache);
        // proj tiene Cargo.toml → código.
        let proj = t.nodes[t.root].children[0];
        assert_eq!(categoria_de(&t, proj), Categoria::Codigo);
    }

    #[test]
    fn vale_la_pena_lista_recuperables_de_mayor_a_menor() {
        let (t, target, cache, _) = arbol();
        let v = vale_la_pena(&t, t.root, 10);
        let nodos: Vec<usize> = v.iter().map(|(n, _)| *n).collect();
        assert_eq!(nodos, vec![target, cache]);
    }

    #[test]
    fn layout_cubre_el_area_y_anida() {
        let (t, _, _, _) = arbol();
        let area = Rect::new(0, 0, 80, 24);
        let tiles = layout(&t, t.root, area);
        // Tres hijos directos de primer nivel.
        assert_eq!(tiles.iter().filter(|m| m.depth == 0).count(), 3);
        // Todo dentro del área.
        assert!(tiles
            .iter()
            .all(|m| m.rect.right() <= 80 && m.rect.bottom() <= 24));
        // 'proj' es lo bastante grande para anidar a sus hijos.
        assert!(tiles.iter().any(|m| m.depth == 1));
    }
}
