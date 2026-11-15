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
/// REVISAR (memoria): el nodo es compacto (72 bytes) y no hace ninguna reserva
/// de memoria propia salvo la lista de hijos de los directorios. El NOMBRE vive
/// en un buffer de texto compartido del árbol (`Tree::names`) y la EXTENSIÓN es
/// un id en una tabla de extensiones internadas (`Tree::exts`): con millones de
/// nodos, dos `String` por nodo costaban cientos de MiB y hacían lentísimo
/// clonar el árbol para los snapshots en vivo. Se accede con `Tree::name(idx)`
/// y `Tree::ext(idx)`.
#[derive(Debug, Clone)]
pub struct Node {
    /// Desplazamiento del nombre dentro de `Tree::names`.
    name_off: u32,
    /// Id de la extensión en `Tree::exts` (0 = sin extensión).
    ext: u32,
    /// Longitud en bytes del nombre (NAME_MAX es 255 en Linux).
    name_len: u16,
    pub is_dir: bool,
    /// Tamaño agregado: para un directorio es la suma de todos sus
    /// descendientes; para un archivo es su propio tamaño.
    pub size: u64,
    /// Número de archivos bajo este nodo (1 para un archivo).
    pub file_count: u64,
    /// Índice del padre dentro del arena (`None` solo para la raíz).
    pub parent: Option<usize>,
    /// Índices de los hijos dentro del arena.
    pub children: Vec<usize>,
}

/// El árbol completo: el arena de nodos, el índice de la raíz y el texto
/// compartido (nombres y extensiones).
#[derive(Debug, Clone)]
pub struct Tree {
    pub nodes: Vec<Node>,
    pub root: usize,
    /// Todos los nombres concatenados (cada nodo guarda su rango).
    names: String,
    /// Extensiones internadas; la posición 0 es "sin extensión".
    exts: Vec<Box<str>>,
    /// Índice inverso extensión → id, para internar sin duplicar.
    ext_ids: std::collections::HashMap<Box<str>, u32>,
}

impl Tree {
    /// Crea un árbol con un único nodo raíz a partir de la ruta escaneada.
    pub fn with_root(root_path: &Path, is_dir: bool, own_size: u64) -> Self {
        let mut tree = Tree {
            nodes: Vec::new(),
            root: 0,
            names: String::new(),
            exts: vec!["".into()],
            ext_ids: std::collections::HashMap::new(),
        };
        let name = display_name(root_path);
        let node = tree.new_node(&name, is_dir, own_size);
        tree.nodes.push(node);
        tree
    }

    /// Construye un nodo guardando su nombre y su extensión en las tablas.
    fn new_node(&mut self, name: &str, is_dir: bool, own_size: u64) -> Node {
        let name_off = self.names.len() as u32;
        // NAME_MAX = 255 bytes; acotamos por si acaso (nunca debería pasar).
        let name = &name[..floor_char_boundary(name, u16::MAX as usize)];
        self.names.push_str(name);
        let ext = if is_dir { 0 } else { self.intern_ext(name) };
        Node {
            name_off,
            ext,
            name_len: name.len() as u16,
            is_dir,
            // Inicialmente size == tamaño propio; los directorios suman hijos.
            size: own_size,
            // Un archivo cuenta como 1; un directorio cuenta 0 (suma de hijos).
            file_count: if is_dir { 0 } else { 1 },
            parent: None,
            children: Vec::new(),
        }
    }

    /// Id de la extensión (en minúsculas) del nombre `name`; 0 si no tiene.
    fn intern_ext(&mut self, name: &str) -> u32 {
        let Some(e) = Path::new(name).extension().and_then(|e| e.to_str()) else {
            return 0;
        };
        // Solo reservamos memoria si hay mayúsculas (lo habitual es que no).
        let lower;
        let e = if e.chars().any(|c| c.is_uppercase()) {
            lower = e.to_lowercase();
            lower.as_str()
        } else {
            e
        };
        if let Some(&id) = self.ext_ids.get(e) {
            return id;
        }
        let id = self.exts.len() as u32;
        self.exts.push(e.into());
        self.ext_ids.insert(e.into(), id);
        id
    }

    /// Nombre del nodo `idx` (no la ruta completa).
    pub fn name(&self, idx: usize) -> &str {
        let n = &self.nodes[idx];
        let ini = n.name_off as usize;
        &self.names[ini..ini + n.name_len as usize]
    }

    /// Extensión en minúsculas, sin el punto, del nodo `idx` (`None` si no
    /// tiene o es un directorio).
    pub fn ext(&self, idx: usize) -> Option<&str> {
        match self.nodes[idx].ext {
            0 => None,
            id => Some(&self.exts[id as usize]),
        }
    }

    /// Añade un nodo hijo al arena y devuelve su índice.
    ///
    /// INVARIANTE IMPORTANTE: como `push` siempre asigna un índice mayor que el
    /// de cualquier nodo previo, y solo creamos un hijo después de su padre, se
    /// cumple `parent_idx < child_idx` para todos los nodos. La agregación
    /// bottom-up (ver `aggregate`) depende de esta invariante.
    ///
    /// No propaga el tamaño a los ancestros: hay que llamar después a
    /// `aggregate` (árbol completo) o a `add_to_ancestors` (incremental).
    pub fn add_child(
        &mut self,
        parent_idx: usize,
        name: &str,
        is_dir: bool,
        own_size: u64,
    ) -> usize {
        let mut node = self.new_node(name, is_dir, own_size);
        node.parent = Some(parent_idx);
        let idx = self.nodes.len();
        self.nodes.push(node);
        self.nodes[parent_idx].children.push(idx);
        idx
    }

    /// Suma `size` bytes y `count` archivos a todos los ANCESTROS de `idx`.
    ///
    /// REVISAR: agregación incremental. El escáner la usa al insertar cada
    /// archivo (coste O(profundidad)), así el árbol está SIEMPRE agregado: un
    /// snapshot en vivo es una simple copia y no hace falta `aggregate` al final.
    pub fn add_to_ancestors(&mut self, idx: usize, size: u64, count: u64) {
        let mut cur = self.nodes[idx].parent;
        while let Some(p) = cur {
            let n = &mut self.nodes[p];
            n.size += size;
            n.file_count += count;
            cur = n.parent;
        }
    }

    /// Libera la capacidad sobrante de los vectores tras el escaneo.
    pub fn shrink(&mut self) {
        self.nodes.shrink_to_fit();
        self.names.shrink_to_fit();
        for n in &mut self.nodes {
            n.children.shrink_to_fit();
        }
        // El índice inverso solo sirve para internar durante la construcción.
        self.ext_ids = std::collections::HashMap::new();
    }

    /// Agrega tamaños y conteos de archivos desde las hojas hacia la raíz.
    ///
    /// REVISAR: agregación bottom-up. Recorremos el arena en orden de índice
    /// DECRECIENTE. Gracias a la invariante `parent_idx < child_idx`, cuando
    /// procesamos el nodo `i` todos sus hijos (índices > i) ya sumaron su total
    /// a `nodes[i].size`. Por tanto `nodes[i]` está completo y podemos propagar
    /// su total al padre. Esto evita recursión y recalcula todo en O(n) una
    /// sola vez; el resultado queda cacheado en `size`/`file_count`.
    // Solo lo usan los tests (el escáner agrega de forma incremental).
    #[cfg(test)]
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
    pub fn path_of(&self, idx: usize, root_path: &Path) -> PathBuf {
        // Acumulamos nombres desde el nodo hasta la raíz y luego invertimos.
        let mut parts: Vec<&str> = Vec::new();
        let mut cur = idx;
        // Subimos mientras haya padre; al llegar a la raíz (sin padre) paramos.
        while let Some(p) = self.nodes[cur].parent {
            parts.push(self.name(cur));
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
                .find(|&c| self.name(c) == name)?;
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

/// Mayor índice <= `max` que cae en un límite de carácter UTF-8 de `s`.
fn floor_char_boundary(s: &str, max: usize) -> usize {
    if s.len() <= max {
        return s.len();
    }
    let mut i = max;
    while !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}
