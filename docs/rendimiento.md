# Rendimiento a escala real (R2.1)

PLAN.md §1 promete **arranque < 1 s, < 150 MB de RAM con 100 k mensajes y 60 fps**. El dueño ya usa
la app con ~62 500 mensajes en 4 cuentas (2 Gmail/Workspace por IMAP+OAuth, Outlook.com por Graph,
1 IMAP). Este documento mide IureDesk con más del triple: **4 cuentas × 50 000 = 200 000 mensajes**.

## Cómo se mide

```text
cargo xtask bench            # los tres bancos (release)
cargo xtask bench --quick    # solo la guarda de regresión (también en CI)
cargo xtask bench --dir RUTA # reutiliza el buzón generado entre corridas
```

| Banco | Qué hace |
|---|---|
| `iuredesk-ui --example scale` | Genera (o reutiliza) el buzón con `sample::populate_scale`: 4 cuentas, 8 carpetas cada una, ≈ 35 % respuestas (hilos), etiquetas en el 25 %, adjuntos en el 12 %, boletines HTML, cuerpo descargado en el 20 % y 3 años de fechas. Mide el arranque en 5 procesos hijos (abrir almacén → sesión → app → primer cuadro → lista agrupada), la memoria, 2 763 cuadros desplazando la bandeja unificada de arriba abajo, cuadros en reposo, recalcular la lista (lo que pasa tras cada tanda de la sincronización), búsqueda, agrupar conversaciones, cambiar de carpeta y abrir un boletín HTML largo. Sin pantalla (`egui_kittest`): el tiempo de cuadro es el de la CPU (diseño + teselado), sin la GPU. |
| `iuredesk-sync --example sync_bench [N]` | Servidor IMAP falso con N = 50 000 mensajes (hilos de 4, 20 % con cuerpo de ~6 KB, adjuntos) y una cuenta recién agregada. Mide la ventana inicial, el historial completo, CPU y memoria del proceso y, mientras tanto, lo que siente la interfaz: leer una página de la bandeja cada 16 ms y «marcar como leído» cada ~250 ms. El servidor corre en el mismo proceso (su CPU y su memoria cuentan). |
| `iuredesk-store --test scale` (`--ignored`, release) | Guarda de regresión con topes generosos (≈ 10× lo medido): paginar, ordenar y agrupar la bandeja unificada, buscar palabra / campo / frase / con filtros. Corre en CI (`cargo xtask bench --quick`). |

**Equipo**: Intel Core i7-8850H (6 núcleos / 12 hilos, 2,6 GHz), 45 GB de RAM, Ubuntu 26.04
(Linux 7.0), Rust 1.97.0, perfil `release` (LTO thin). El buzón vive en tmpfs: los tiempos de
arranque son con la caché de páginas caliente (el arranque en frío de disco depende del disco).

## Antes y después (200 000 mensajes; bandeja unificada = 109 566 mensajes, 82 862 conversaciones)

| Medida | Antes | Después | Arreglo |
|---|---:|---:|---|
| Arranque: primer cuadro (p50) | 149 ms | 144 ms | — (ya cumplía) |
| Arranque: lista agrupada lista (p50) | 757 ms | 283 ms | Agrupar en una transacción con caché de hilos (1) |
| RSS tras arrancar | 160 MB | 106 MB (78 MB anónima) | Caché de redb de 32 MB (2) |
| RSS tras recorrer la bandeja entera | 180 MB | 123 MB | (2) |
| Cuadro desplazando, p50 / p99 / máx | 3,6 / 6,1 / 15 ms | 3,8 / 7,1 / 19 ms | — (CPU; muy por debajo de 16,6 ms) |
| Cuadro en reposo, p50 | 0,34 ms | 0,36 ms | — |
| Agrupar conversaciones de la bandeja unificada | 391 ms | 21 ms (≈ 130 ms la primera vez tras abrir) | (1) |
| Recalcular la lista tras una tanda de la sincronización (hasta tener las conversaciones) | ≈ 400 ms, cada 0,7 s, apilando trabajos | 31 ms; hilo de la UI 7 ms | (1) y (3) |
| Cambiar de carpeta (hasta la lista lista), p50 / máx | 42 / 427 ms | 18 / 39 ms | (1) |
| Buscar palabra / campo / frase (p50) | 8 / 3 / 15 ms | 9 / 4 / 19 ms | — |
| Buscar con filtros (`in:inbox is:unread …`, `is:unread`), p50 / p99 | 35 / 78 ms | 42 / 62 ms | Prefiltro por carpeta y banderas antes de decodificar sobres (4) |
| Abrir boletín HTML: hasta verlo / cuadro más lento de la UI | 42–140 ms / (conversión en la UI) | 68 ms / 6 ms | Conversión a imagen en segundo plano (5) |
| Memoria de texturas HTML | sin tope (24 renders; un boletín largo a 2× pasa de 50 MB) | ≤ 96 MB | LRU por bytes (5) |
| Sync inicial de 50 000: «marcar leído» durante la sincronización, p99 | 230 ms | 14 ms | Recalcular hilos sin reescribir la cuenta (6) |
| Sync inicial de 50 000: tamaño de redb | 68 MB | 34 MB | (6) |
| Sync inicial de 50 000: historial completo / CPU | 6,4 s / 8,0 s | 6,6 s / 8,5 s | — (7 500 msg/s) |
| Sync: leer una página de la bandeja durante la sincronización, p99 | 0,34 ms | 0,37 ms | — (las lecturas no esperan a las escrituras) |
| Guarda: página de 100 sobres al azar, p50 / p99 | — | 0,5 / 0,8 ms | — |

El RSS del primer arranque «antes» incluía 1 GiB de caché de redb a medio llenar; con 100 000
mensajes (la promesa) el arranque queda bien por debajo de 150 MB. El pico de 149 MB al final del
banco de la app incluye dos boletines HTML pintados que `egui_kittest` (sin GPU) conserva en RAM; en
la app real la textura sube a la GPU.

## Los arreglos

1. **Agrupar conversaciones** (`Store::group_by_thread`). Antes: `get_envelope` + `get_thread` por
   mensaje, cada uno con su transacción de lectura y decodificando el sobre entero (110 000
   transacciones). Ahora: una transacción; el hilo de cada mensaje sale de una caché en memoria
   (`ThreadCache`, ≈ 40 B por mensaje) que en frío se llena recorriendo la tabla de miembros de hilo
   (16 B por fila) y los recuentos de cada hilo también se guardan. Las escrituras la mantienen al
   día (`CacheOp::ForgetThread`, `ClearThreads` al recalcular hilos) y unas generaciones evitan que
   una lectura vieja la rellene con datos viejos. Pruebas en `tests/mail.rs`.
2. **Caché de redb** (`REDB_CACHE_BYTES` = 32 MB). La de omisión es 1 GiB: cada sobre leído se
   quedaba en RAM. El sistema ya guarda el archivo en su caché de páginas.
3. **Refrescos de la sincronización sin apilar** (`Session::refresh_rows`). Cada 0,7 s la
   sincronización pide recalcular la lista: antes lanzaba otro trabajo de agrupación aunque el
   anterior no hubiera terminado y, mientras tanto, la lista pasaba a mensajes sueltos (parpadeo y
   salto del desplazamiento). Ahora, si hay uno en curso, se repite al terminar y la lista conserva
   sus filas agrupadas. Las categorías (Principal, Promociones…) leen sobres y banderas en lote
   (`get_envelopes`, `flags_of`): una transacción en vez de 5 000 en el hilo de la UI.
4. **Búsqueda con filtros**: antes de decodificar cada candidato se descarta por pertenencia a las
   carpetas pedidas (sus listas ya están en caché) y por banderas (tabla aparte).
5. **HTML**: el render llega convertido a `ColorImage` desde el hilo de fondo (la UI solo sube la
   textura); la caché es LRU con tope de 96 MB además de 24 renders, y el recién llegado se queda
   aunque solo él pase del tope. Las imágenes remotas descargadas se recuerdan para 16 mensajes.
   Además, un render más alto que la textura máxima de la GPU (`max_texture_side`, a menudo 8 192 px;
   un boletín largo a 2× llega a 12 000) se parte en franjas: antes `load_texture` entraba en
   pánico y la app se cerraba. Pruebas en `html.rs`.
6. **Recalcular hilos** (`Store::rethread_account`, al terminar la ventana inicial y el historial).
   Antes reescribía todos los sobres e hilos de la cuenta en una transacción con el candado de
   escritura tomado: la interfaz, que marca leído/no leído en el almacén, esperaba hasta 230 ms.
   Ahora lee en una transacción y solo reescribe los grupos que cambian de hilo (la asignación
   incremental ya acierta casi siempre). Prueba de regresión en `tests/mail.rs`.

## Panel «Rendimiento»

Ajustes → Avanzado → Rendimiento (captura `screens/rendimiento.png`): tiempo por cuadro (p50, p99,
máx., fps de los últimos 240 cuadros), memoria (residente, de la app y pico; Linux), filas de la
lista, HTML pintado y su memoria, almacén (mensajes, conversaciones, tamaño de base, cuerpos e
índice; calculado en segundo plano cada 10 s), cambios en la cola de sincronización y última
sincronización de cada cuenta. «Copiar informe» lo deja en el portapapeles para los reportes del
dueño.

## Pendiente

- ~~La sesión tardaba ≈ 90 ms en el hilo de la UI antes del primer cuadro clasificando en
  categorías~~ (R1.2: con 1 000 o más sin clasificar, sobres y señales se leen en segundo plano y
  la lista muestra «cargando» hasta tenerlas).
- ~~La memoria en Windows y macOS no se mostraba~~ (R1.2: residente con `memory-stats`; la anónima
  y el pico solo en Linux).
- El tiempo de cuadro de la GPU no se mide sin pantalla; medirlo en la app real con el panel.
- La sincronización real de Gmail/Graph (red, OAuth, `X-GM-LABELS`) no está en el banco: el
  servidor falso es IMAP genérico en local.
