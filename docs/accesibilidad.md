# Accesibilidad: prueba manual con lectores de pantalla (R4.1)

Lo automático vive en `crates/ui/tests/a11y.rs`: cada pantalla se recorre en el árbol de AccessKit
y todo lo interactivo debe tener nombre; F6 recorre los paneles; Ctrl +/−/0 cambia el tamaño del
texto; contraste AA de las combinaciones propias. Lo que no se puede probar sin un lector real está
aquí. Se hace antes de cada beta en los tres sistemas y lo encontrado entra en `gaps.md` como
`BUG-…`.

## Preparación

| Sistema | Lector | Cómo encenderlo |
|---|---|---|
| Linux (GNOME, Wayland o X11) | Orca | `Super+Alt+S` o Ajustes → Accesibilidad → Lector de pantalla |
| Windows 10/11 | NVDA (gratuito) | <https://www.nvaccess.org/download/>; `Ctrl+Alt+N` |
| macOS | VoiceOver | `Cmd+F5` |

Abrir IureDesk con el buzón de ejemplo (`iuredesk --sample`) para que todos vean los mismos datos.
En Linux, AccessKit habla por AT-SPI (D-Bus): si Orca no lee nada, comprobar que el bus de
accesibilidad está activo (`gsettings get org.gnome.desktop.interface toolkit-accessibility`).

## Recorrido (unos 15 minutos por sistema)

Marcar cada paso con ✓ o anotar lo que se oyó.

1. **Arranque.** Al abrir, el lector anuncia la ventana «IureDesk» y el módulo («Mi día»).
2. **Riel.** Con `Tab` se llega a los módulos; cada uno se lee con su nombre y su atajo («Correo,
   Ctrl+1»). `Intro` cambia de módulo.
3. **F6.** En Correo, `F6` va a la búsqueda («Buscar en el correo, texto editable»), luego a la
   lista de mensajes («Mensajes…») y luego al mensaje («Mensaje: <asunto>»); `Mayús+F6` vuelve.
4. **Lista.** Con flechas arriba/abajo cada fila se lee con remitente, asunto, fecha y si está sin
   leer o con bandera; la insignia de Iurefficient se lee como texto («Oportunidad: ACME»).
5. **Acciones.** `Ctrl+K` abre la paleta («Paleta de comandos, texto editable»); al escribir
   «arch» se lee la primera coincidencia; `Esc` la cierra y el foco vuelve a donde estaba.
6. **Deshacer.** Archivar un mensaje: se anuncia «Archivado… Deshacer»; `Ctrl+Z` lo devuelve.
7. **Redactor.** `Ctrl+N` (esquema Outlook): Para, Asunto y el cuerpo se leen con su nombre;
   `Tab` recorre sin quedar atrapado; `Esc` pregunta por el borrador.
8. **Calendario.** Las vistas se cambian con el teclado; un evento se lee con título y hora; el
   editor de eventos se recorre entero con `Tab`.
9. **Ajustes.** Cada sección y cada control se lee con su nombre: «Fuente», «Tamaño del texto,
   deslizador, 100 %», «Segundos para deshacer el envío»…
10. **Tamaño del texto.** `Ctrl` + `+` en Calendario agranda toda la interfaz; el lector lo
    anuncia («Tamaño del texto: 105 %»); `Ctrl+0` lo devuelve.
11. **Avisos.** Un error de sincronización (sin red) se anuncia una vez con «Reintentar».
12. **Alto contraste.** Ajustes → Apariencia → Alto contraste: el anillo de foco amarillo se ve en
    cada control que recibe el foco.

## Resultados

| Fecha | Sistema y lector | Versión | Resultado | Notas |
|---|---|---|---|---|
| | | | | |
