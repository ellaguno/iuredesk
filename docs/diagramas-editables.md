# Diagramas Mermaid editables desde el dibujo (S1.33)

Pedido del dueño (2026-11-21): hacer con los diagramas Mermaid lo que ya hacen los mapas mentales:
agregar y cambiar elementos con el ratón, sin escribir el código. Por ejemplo, en un diagrama de
cuadrantes, poner un elemento en un cuadrante eligiendo su posición; en uno de flujo, agregar nodos
eligiendo el tipo y a qué nodo se conectan.

Vale para las tres apps que usan el editor de la suite: notas de IureDesk, IureEditor 2.0 y notas de
tareas de IureProject.

## Lo que ya hay (y se reutiliza)

- **Mapas mentales** (UX-12, S1.26): `html-view` dibuja el mapa y devuelve la caja de cada nodo con
  su línea (`mindmap::draw_boxed` → `NodeBox { line, depth, rect }`); `iure-doc` recibe esas zonas
  (`DiagramImage::hotspots`), dibuja una barra al pulsar un nodo y pide a la app el bloque nuevo
  (`DiagramRenderer::node_action`). Deshacer es el del editor (el bloque cambia como si se tecleara).
- **Enlaces en diagramas** (S1.24): `html-view` ya lee del SVG de `mermaid-svg` la caja de las figuras
  con enlace (`shape_box`).
- `mermaid-svg` 0.7 expone el árbol de cada diagrama (`mermaid_svg::parse`): nodos de un flujo con
  su `id`, texto y forma (`NodeShape`), aristas, subgrafos; puntos de un cuadrante con etiqueta y
  coordenadas. **No** da posiciones en el código ni marca en el SVG qué figura es qué nodo.

## Principio: se edita el texto, no se regenera

El código Mermaid es del usuario (comentarios, orden, sangría, estilos). Cada acción es un cambio de
texto mínimo y local (insertar una línea, cambiar un número, reemplazar una etiqueta), nunca volver a
escribir el diagrama desde el árbol. Así:

- lo que el editor no entiende se conserva tal cual;
- deshacer deshace solo esa acción;
- un error de dibujo no destruye nada: el código sigue ahí.

Los editores de texto viven en `html-view` (sin egui), como `mindmap::edit_node`, con pruebas de
propiedades (cualquier entrada: nunca falla, el resultado se vuelve a leer igual salvo el cambio).

## Interacción (igual en todos los tipos)

1. **Clic en un elemento** → barra flotante junto a él (como en los mapas): acciones con icono y
   nombre al pasar. **Doble clic** → al código, en su línea (como hoy).
2. **Clic en un hueco** del dibujo → las acciones de «agregar aquí» (si el tipo las tiene).
3. **Arrastrar** un elemento movible (puntos de un cuadrante) → cambia sus coordenadas al soltar.
4. Lo que necesita un dato lo pide en un globo pequeño junto al dibujo: **texto** (etiqueta),
   **elección** (forma del nodo, con su dibujo) o **un punto** (con el ratón). Intro acepta, Esc
   cancela.
5. Todo con teclado también: Tab recorre los elementos, Intro abre su barra (accesibilidad).

## Por tipo de diagrama

### Fase 1

**Cuadrantes** (`quadrantChart`)
- Clic en un hueco de la gráfica → «Agregar elemento aquí»: pide la etiqueta y escribe
  `Etiqueta: [x, y]` con las coordenadas del clic (0–1, dos decimales).
- Arrastrar un punto → nuevas coordenadas al soltar (con una guía que muestra x, y y el cuadrante).
- Barra de un punto: cambiar etiqueta, color, tamaño (`radius`), borrar.
- Clic en el nombre de un cuadrante o de un eje → cambiar el texto.
- Geometría: `mermaid-svg` usa medidas fijas (márgenes, 500×500 por omisión, `chartWidth`) y dibuja
  los cuatro cuadrantes y luego los puntos en el orden del código; la zona de la gráfica y la caja de
  cada punto se leen del SVG y cada punto se une con su línea por orden.

**Flujo** (`flowchart` / `graph`)
- Barra de un nodo:
  - **Agregar siguiente** → elegir forma (rectángulo, redondeado, rombo de decisión, círculo,
    cilindro, subrutina…) y texto; escribe un nodo nuevo con un id libre y la flecha
    `A --> B[texto]` después de la última línea del nodo.
  - **Conectar con…** → se pulsa el otro nodo; pide (opcional) el texto de la flecha
    (`A -->|sí| B`).
  - Cambiar texto, cambiar forma, borrar (con sus flechas), estilo (color, como «Colores…»).
- Clic en una flecha → cambiar su texto, su tipo (`-->`, `-.->`, `==>`), invertirla, borrarla.
- Clic en un hueco → «Agregar nodo suelto».
- Cajas de los nodos: hoy el SVG no dice qué figura es qué nodo. Se propone **a `mermaid-svg`** (como
  los PR #393 y #394) que cada nodo y arista lleve su id (`data-id`, como mermaid.js). Mientras no
  esté publicado, se unen figura y nodo por su texto y forma (los nodos con el mismo texto se
  desempatan por orden).

### Fase 2 (después de probar la 1)

- **Secuencia**: agregar participante; agregar mensaje entre dos participantes (se pulsan) con su
  tipo (síncrono, respuesta, asíncrono); notas.
- **Gantt**: agregar tarea en una sección; arrastrar una barra para cambiar fechas o duración.
- **Línea de tiempo**, **pastel** (agregar o cambiar un valor), **Kanban** de Mermaid (mover
  tarjetas entre columnas).
- **Clases / ER**: agregar clase o entidad, atributo, relación.

## Arquitectura

**`html-view`** (IureDesk, por `rev` en IureEditor e IureProject)
- `diagram::elements(block, drawn) -> Vec<Element>`: por tipo, la caja de cada elemento en el
  dibujo, qué es (`Point`, `Node`, `Edge`, `Quadrant`, `Axis`, …), su línea en el código y, si
  aplica, la zona de «agregar aquí» con su sistema de coordenadas.
- `mermaid_edit::{quadrant, flowchart, …}`: los cambios de texto (`add_point`, `move_point`,
  `add_node_after`, `connect`, `set_label`, `set_shape`, `delete`…), sin egui, acotados (tamaño del
  bloque, número de elementos) y con pruebas de propiedades.

**`iure-doc-ui`** (el editor de la suite): se generaliza la barra de los mapas.
- `DiagramImage::hotspots` lleva además el tipo de zona y si se puede arrastrar.
- `DiagramRenderer::actions(block, zona) -> Vec<ActionDef>`: icono, nombre y qué dato pide
  (`Nada`, `Texto { sugerido }`, `Elegir(opciones con su dibujo)`, `Punto`, `OtraZona`).
- `DiagramRenderer::apply(block, zona, acción, dato) -> Option<(bloque nuevo, cursor)>`.
- `iure-doc` dibuja la barra, el globo del dato, la elección de otra zona («Conectar con…») y el
  arrastre (con guía); el bloque cambia en un solo paso de deshacer.
- Los mapas mentales pasan a este mismo esquema (sus tres acciones de hoy quedan igual para el
  usuario). Una versión de `iure-doc` que lo traiga se publica con etiqueta y se sube en las tres
  apps.

**Apps**: `NoteDiagrams` (IureDesk), `Diagrams` (IureEditor) y los ganchos de IureProject solo
pasan las zonas y las acciones de `html-view`; nada propio de cada app.

## Pruebas

- `html-view`: cada edición con casos del dueño y propiedades (nunca falla; el bloque editado se
  vuelve a leer con `mermaid_svg::parse` y tiene el elemento nuevo en su lugar; lo demás, igual
  byte a byte).
- `iure-doc` (kittest): clic en un punto → barra; arrastrar → coordenadas nuevas; «Agregar
  siguiente» → nodo y flecha; deshacer vuelve al texto anterior.
- Capturas para revisar a ojo antes de dar por terminado.

## Decisiones del dueño (2026-11-22)

1. **Etiqueta al agregar**: se pide antes, en un globo con el texto.
2. **Cuadrantes**: los puntos se arrastran para moverlos (cambian sus coordenadas en el código).
3. **Flujo**: el nodo nuevo hereda el estilo (`:::clase`) del anterior, por omisión.
4. **Fase 2**: primero los diagramas de secuencia.

## Estado (2026-11-23)

Hecho en IureDesk, IureEditor 2.0 e IureProject (ramas `diagramas-editables`): fase 1 completa
(cuadrantes y flujo, con flechas), fase 2 completa (secuencia, Gantt —con arrastre de barras—,
línea de tiempo, pastel, Kanban, clases y entidad-relación), teclado y lector de pantalla, y la guía
al arrastrar. Pendiente de la propuesta a `mermaid-svg`: que cada figura lleve su id (`data-id`);
mientras, las figuras se unen con los elementos por su texto y su orden.
