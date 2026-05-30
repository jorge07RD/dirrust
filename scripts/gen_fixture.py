#!/usr/bin/env python3
"""Genera un árbol de directorios de prueba para validar dirrust.

Crea archivos con tamaños EXACTOS y extensiones conocidas, incluye duplicados
intencionales, y al final imprime un resumen de:
  - tamaño total y por carpeta de primer nivel,
  - desglose por extensión,
  - grupos de archivos duplicados (mismo contenido) y espacio recuperable.

Así puedes comparar esas cifras con lo que muestra dirrust:

    python3 scripts/gen_fixture.py /tmp/dirrust_fixture
    cargo run --release -- /tmp/dirrust_fixture

Uso:
    python3 scripts/gen_fixture.py [DESTINO]   (por defecto: ./dirrust_fixture)
"""

from __future__ import annotations

import os
import sys
from collections import defaultdict

# Definición del árbol de prueba.
#
# Cada entrada es (ruta_relativa, tamaño_en_bytes, semilla_de_contenido).
# Dos archivos con la MISMA semilla y el MISMO tamaño tienen contenido idéntico
# (son duplicados); con semillas distintas, contenido distinto.
FILES: list[tuple[str, int, str]] = [
    # videos/ : 2 archivos grandes, uno de ellos duplicado en backup/
    ("videos/pelicula.mp4", 5_000_000, "peli"),
    ("videos/clip.mp4", 1_500_000, "clip"),
    # musica/ : varios mp3
    ("musica/cancion1.mp3", 800_000, "c1"),
    ("musica/cancion2.mp3", 800_000, "c2"),
    ("musica/cancion3.mp3", 400_000, "c3"),
    # documentos/ : textos y un pdf; informe duplicado
    ("documentos/informe.pdf", 250_000, "informe"),
    ("documentos/notas.txt", 12_000, "notas"),
    ("documentos/lista.txt", 12_000, "lista"),  # mismo tamaño, distinto contenido
    # backup/ : copias EXACTAS (duplicados) de algunos archivos
    ("backup/pelicula.mp4", 5_000_000, "peli"),       # == videos/pelicula.mp4
    ("backup/informe.pdf", 250_000, "informe"),       # == documentos/informe.pdf
    ("backup/copia_notas.txt", 12_000, "notas"),      # == documentos/notas.txt
    # vacios/ : una carpeta sin archivos (debe contar 0 bytes)
    # (se crea explícitamente más abajo)
]

EMPTY_DIRS = ["vacios"]


def contenido(size: int, semilla: str) -> bytes:
    """Genera `size` bytes deterministas a partir de una semilla.

    El mismo (size, semilla) produce siempre los mismos bytes, de modo que dos
    archivos así son duplicados exactos.
    """
    patron = (semilla.encode("utf-8") or b"x")
    # Repetimos el patrón hasta llenar y recortamos al tamaño exacto.
    repeticiones = size // len(patron) + 1
    return (patron * repeticiones)[:size]


def crear_arbol(destino: str) -> None:
    for rel, size, semilla in FILES:
        ruta = os.path.join(destino, rel)
        os.makedirs(os.path.dirname(ruta), exist_ok=True)
        with open(ruta, "wb") as f:
            f.write(contenido(size, semilla))
    for d in EMPTY_DIRS:
        os.makedirs(os.path.join(destino, d), exist_ok=True)


def fmt(n: int) -> str:
    """Formato binario legible (coincide con el de dirrust: KiB/MiB...)."""
    unidades = ["B", "KiB", "MiB", "GiB", "TiB"]
    x = float(n)
    for u in unidades:
        if x < 1024 or u == unidades[-1]:
            return f"{x:.2f} {u}"
        x /= 1024


def resumen(destino: str) -> None:
    total = 0
    por_carpeta: dict[str, int] = defaultdict(int)
    por_ext: dict[str, list[int]] = defaultdict(lambda: [0, 0])  # ext -> [bytes, count]
    por_contenido: dict[tuple[int, str], list[str]] = defaultdict(list)

    for rel, size, semilla in FILES:
        total += size
        top = rel.split("/", 1)[0]
        por_carpeta[top] += size
        ext = os.path.splitext(rel)[1].lstrip(".").lower() or "(sin ext)"
        por_ext[ext][0] += size
        por_ext[ext][1] += 1
        # Clave de contenido: mismo (tamaño, semilla) => mismo contenido.
        por_contenido[(size, semilla)].append(rel)

    print(f"\n== Fixture generado en: {destino} ==\n")
    print(f"TOTAL: {fmt(total)} ({total} bytes), {len(FILES)} archivos\n")

    print("Por carpeta de primer nivel:")
    for carpeta in sorted(por_carpeta, key=lambda k: -por_carpeta[k]):
        print(f"  {carpeta:<14} {fmt(por_carpeta[carpeta]):>12}")
    print("  vacios          0.00 B   (carpeta sin archivos)")

    print("\nPor extensión:")
    for ext in sorted(por_ext, key=lambda k: -por_ext[k][0]):
        b, c = por_ext[ext]
        print(f"  .{ext:<12} {fmt(b):>12}  ({c} archivos)")

    print("\nDuplicados (grupos con contenido idéntico):")
    recuperable_total = 0
    hay = False
    for (size, _semilla), rutas in sorted(
        por_contenido.items(), key=lambda kv: -kv[0][0] * (len(kv[1]) - 1)
    ):
        if len(rutas) >= 2:
            hay = True
            recuperable = size * (len(rutas) - 1)
            recuperable_total += recuperable
            print(f"  {len(rutas)} copias × {fmt(size)}  (recuperable {fmt(recuperable)})")
            for r in rutas:
                print(f"      {r}")
    if not hay:
        print("  (ninguno)")
    print(f"\nEspacio recuperable total: {fmt(recuperable_total)}\n")


def main() -> int:
    destino = sys.argv[1] if len(sys.argv) > 1 else "dirrust_fixture"
    destino = os.path.abspath(destino)
    crear_arbol(destino)
    resumen(destino)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
