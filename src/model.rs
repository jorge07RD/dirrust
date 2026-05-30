//! Modelo de datos del árbol de directorios.
//!
//! Decisión de diseño central: usamos un **arena basado en `Vec`** con índices
//! `usize` en lugar de `Rc<RefCell<Node>>`. Motivos:
//!   - Es cache-friendly: todos los nodos viven contiguos en memoria.
//!   - Evita el coste de conteo de referencias y de `borrow()` en caliente.
//!   - Permite navegar padre/hijos con simples índices (muy barato por frame).
//!   - Es el patrón idiomático en Rust para árboles/grafos de solo-añadir.
//!
//! La contrapartida es que hay que tener cuidado de no invalidar índices; como
//! solo añadimos nodos durante el escaneo y nunca eliminamos del `Vec` (los
//! borrados marcan el nodo, no compactan el arena), los índices son estables.

use std::path::{Path, PathBuf};

/// Un nodo del árbol: puede ser un archivo o un directorio.
///
// Algunos campos (name, is_dir, own_size, extension) aún no se leen en la Fase 1
// pero se rellenan ya en el escaneo y los consumirán la tabla (Fase 2), el
// treemap (Fase 3) y el desglose por extensión (Fase 6).
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub struct Node {
    /// Nombre del archivo/carpeta, NO la ruta completa (ahorra memoria en
    /// árboles grandes; la ruta se reconstruye subiendo por `parent`).
    pub name: String,
    pub is_dir: bool,
    /// Tamaño agregado: para un directorio es la suma de todos sus
    /// descendientes; para un archivo es su propio tamaño. Se calcula una vez
    /// tras el escaneo (ver `aggregate`) y se cachea para no recalcular por frame.
    pub size: u64,
    /// Tamaño propio del nodo (relevante en archivos; 0 en directorios).
    pub own_size: u64,
    /// Número de archivos bajo este nodo (1 para un archivo).
    pub file_count: u64,
    /// Índice del padre dentro del arena (`None` solo para la raíz).
    pub parent: Option<usize>,
    /// Índices de los hijos dentro del arena.
    pub children: Vec<usize>,
    /// Extensión en minúsculas, sin el punto (p. ej. "mp4"). `None` si no tiene.
    pub extension: Option<String>,
}

impl Node {
    /// Crea un nodo "hoja" inicial. La agregación rellena `size`/`file_count`.
    fn new(name: String, is_dir: bool, own_size: u64, extension: Option<String>) -> Self {
        Node {
            name,
            is_dir,
            // Inicialmente size == own_size; la agregación bottom-up suma hijos.
            size: own_size,
            own_size,
            // Un archivo cuenta como 1; un directorio cuenta 0 (suma de hijos).
            file_count: if is_dir { 0 } else { 1 },
            parent: None,
            children: Vec::new(),
            extension,
        }
    }
}

/// El árbol completo: el arena de nodos más el índice de la raíz.
#[derive(Debug, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: usize,
}

impl Tree {
    /// Crea un árbol con un único nodo raíz a partir de la ruta escaneada.
    pub fn with_root(root_path: &Path, is_dir: bool, own_size: u64) -> Self {
        let name = display_name(root_path);
        let ext = extension_of(root_path, is_dir);
        let root = Node::new(name, is_dir, own_size, ext);
        Tree {
            nodes: vec![root],
            root: 0,
        }
    }

    /// Añade un nodo hijo al arena y devuelve su índice.
    ///
    /// INVARIANTE IMPORTANTE: como `push` siempre asigna un índice mayor que el
    /// de cualquier nodo previo, y solo creamos un hijo después de su padre, se
    /// cumple `parent_idx < child_idx` para todos los nodos. La agregación
    /// bottom-up (ver `aggregate`) depende de esta invariante.
    pub fn add_child(
        &mut self,
        parent_idx: usize,
        name: String,
        is_dir: bool,
        own_size: u64,
        extension: Option<String>,
    ) -> usize {
        let mut node = Node::new(name, is_dir, own_size, extension);
        node.parent = Some(parent_idx);
        let idx = self.nodes.len();
        self.nodes.push(node);
        self.nodes[parent_idx].children.push(idx);
        idx
    }

    /// Agrega tamaños y conteos de archivos desde las hojas hacia la raíz.
    ///
    /// REVISAR: agregación bottom-up. Recorremos el arena en orden de índice
    /// DECRECIENTE. Gracias a la invariante `parent_idx < child_idx`, cuando
    /// procesamos el nodo `i` todos sus hijos (índices > i) ya sumaron su total
    /// a `nodes[i].size`. Por tanto `nodes[i]` está completo y podemos propagar
    /// su total al padre. Esto evita recursión y recalcula todo en O(n) una
    /// sola vez; el resultado queda cacheado en `size`/`file_count`.
    pub fn aggregate(&mut self) {
        // Empezamos en el último índice y bajamos hasta 1 (la raíz es 0 y no
        // tiene padre al que propagar).
        for i in (1..self.nodes.len()).rev() {
            // En este punto nodes[i].size ya incluye su subárbol completo.
            let size = self.nodes[i].size;
            let count = self.nodes[i].file_count;
            if let Some(parent) = self.nodes[i].parent {
                debug_assert!(parent < i, "se rompió la invariante padre<hijo");
                self.nodes[parent].size += size;
                self.nodes[parent].file_count += count;
            }
        }
    }

    /// Reconstruye la ruta completa de un nodo subiendo por sus padres.
    // Se usará a partir de la Fase 2 (navegación) y Fase 5 (borrado).
    #[allow(dead_code)]
    pub fn path_of(&self, idx: usize, root_path: &Path) -> PathBuf {
        // Acumulamos nombres desde el nodo hasta la raíz y luego invertimos.
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = idx;
        // Subimos mientras haya padre; al llegar a la raíz (sin padre) paramos.
        while let Some(p) = self.nodes[cur].parent {
            parts.push(&self.nodes[cur].name);
            cur = p;
        }
        let mut path = root_path.to_path_buf();
        for part in parts.iter().rev() {
            path.push(part);
        }
        path
    }

    /// Localiza el nodo correspondiente a una ruta absoluta, descendiendo desde
    /// la raíz componente a componente. Devuelve `None` si no existe en el árbol.
    ///
    /// Se usa para sincronizar los agregados cuando se borra un archivo desde la
    /// vista de duplicados (cuya posición en el árbol no conocemos de antemano).
    pub fn find_by_path(&self, root_path: &Path, target: &Path) -> Option<usize> {
        let rel = target.strip_prefix(root_path).ok()?;
        let mut cur = self.root;
        for comp in rel.components() {
            let name = comp.as_os_str().to_string_lossy();
            let next = self.nodes[cur]
                .children
                .iter()
                .copied()
                .find(|&c| self.nodes[c].name == name)?;
            cur = next;
        }
        Some(cur)
    }

    /// Total agregado de la raíz (tamaño escaneado).
    pub fn total_size(&self) -> u64 {
        self.nodes[self.root].size
    }

    /// Total de archivos bajo la raíz.
    pub fn total_files(&self) -> u64 {
        self.nodes[self.root].file_count
    }
}

/// Nombre legible de una ruta (el último componente, o la ruta entera si es "/").
fn display_name(path: &Path) -> String {
    path.file_name()
        .map(|s| s.to_string_lossy().into_owned())
        // Para "/" o rutas sin file_name usamos la representación completa.
        .unwrap_or_else(|| path.to_string_lossy().into_owned())
}

/// Extensión en minúsculas sin punto; `None` para directorios o sin extensión.
pub fn extension_of(path: &Path, is_dir: bool) -> Option<String> {
    if is_dir {
        return None;
    }
    path.extension().map(|e| e.to_string_lossy().to_lowercase())
}
