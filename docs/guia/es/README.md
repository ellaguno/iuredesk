# Guía de uso de IureDesk

IureDesk reúne tu correo, calendario, contactos, tareas y notas en una sola ventana. Funciona con
tus propias cuentas (Gmail, Outlook.com y Microsoft 365, iCloud o cualquier correo IMAP) y no
necesita Iurefficient; si lo usas, IureDesk lo conecta con tu correo.

[English](../en/README.md) · [Atajos de teclado](atajos.md)

> **Beta.** Esta es una versión de prueba. Si algo falla, IureDesk guarda un informe en tu equipo
> y te lo ofrece al volver a abrir; nada se envía sin que tú lo decidas.

## Índice

1. [Primeros pasos](#primeros-pasos)
2. [Agregar cuentas](#agregar-cuentas)
3. [Correo](#correo)
4. [Calendario](#calendario)
5. [Contactos, tareas y notas](#contactos-tareas-y-notas)
6. [Mi día](#mi-día)
7. [Iurefficient y el asistente ✦](#iurefficient-y-el-asistente-)
8. [Importar desde otro programa](#importar-desde-otro-programa)
9. [Ajustes útiles](#ajustes-útiles)
10. [Privacidad](#privacidad)
11. [Preguntas frecuentes](#preguntas-frecuentes)

## Primeros pasos

- **Instalar.** Lo más fácil es [iureapps](https://github.com/ellaguno/iureapps), el lanzador de
  la suite, que instala y actualiza IureDesk. También puedes bajar el instalador de tu sistema de
  la [página de versiones](https://github.com/ellaguno/iuredesk/releases).
- **Primera vez.** IureDesk abre el asistente de alta: escribe tu correo y te propone **un solo
  botón** para entrar. Si prefieres mirar antes, abre `iuredesk --sample`: es un buzón de
  ejemplo que no se conecta a nada.
- **La ventana.** A la izquierda está el riel con los módulos (Mi día, Correo, Calendario,
  Contactos, Notas, Tareas y Asistente) y abajo Ajustes. En el centro, la lista; a la derecha, lo
  que tengas seleccionado.
- **Todo está en la paleta.** Pulsa `Ctrl+K` (⌘K en Mac) y escribe lo que quieres hacer:
  «archivar», «nuevo evento», «tema oscuro»… La paleta muestra el atajo de cada acción.
- **Deshacer siempre.** Archivar, eliminar, mover, marcar o completar se deshace con `Ctrl+Z` (`Z`
  con los atajos de Gmail) o con el botón «Deshacer» del aviso.
- **Sin conexión.** Todo lo que ya bajó se lee sin internet. Lo que hagas sin conexión queda en
  cola y se manda solo al volver.

## Agregar cuentas

Ajustes → Cuentas → «Agregar cuenta» (o la paleta: «Agregar cuenta»). Escribe tu correo:
IureDesk busca la configuración del servidor por ti.

### Gmail y Google Workspace

Pulsa «Iniciar sesión con Google»: se abre tu navegador, eliges la cuenta y das permiso.

**Mientras dure la beta**, Google tiene a IureDesk en *modo de pruebas*: solo pueden entrar las
cuentas que están en su lista de usuarios de prueba (hasta 100). Si no estás, Google mostrará
«Esta app no está verificada» o un error 403, e IureDesk te lo explicará. Tienes dos caminos:

1. Pide que agreguen tu cuenta como usuario de prueba, o
2. usa una **contraseña de app**: activa la verificación en dos pasos en tu cuenta de Google,
   crea una contraseña en <https://myaccount.google.com/apppasswords> y, en IureDesk, elige
   «Usar una contraseña de app» y pégala. Con contraseña de app tienes correo, calendario y
   contactos igual.

En cuentas de empresa (Workspace), el administrador puede bloquear apps externas; IureDesk te dirá
si es el caso.

### Outlook.com, Hotmail y Microsoft 365

Pulsa «Iniciar sesión con Microsoft». En cuentas de empresa puede pasar que:

- **tu organización pida aprobar la app**: IureDesk muestra el enlace de aprobación para que se lo
  mandes a tu área de TI (hay una guía para ellos en
  [`microsoft-365-ti.md`](../../microsoft-365-ti.md));
- **haya acceso condicional** (solo equipos administrados): no se puede saltar; IureDesk te lo
  explica.

### iCloud

iCloud pide una **contraseña específica de app**: créala en <https://account.apple.com> → Inicio
de sesión y seguridad → Contraseñas específicas de app, y pégala en IureDesk.

### Otro correo (IMAP/SMTP)

Casi siempre basta con tu correo y contraseña. Si IureDesk no encuentra la configuración, te
pide los datos del servidor (te los da tu proveedor): servidor de entrada IMAP o POP3, de salida
SMTP, puertos y cifrado.

### Exchange de tu empresa (servidor propio)

Aún no está disponible en la beta. Si tu empresa usa Microsoft 365, entra con «Iniciar sesión con
Microsoft».

## Correo

- **Leer.** Selecciona un mensaje para verlo a la derecha; doble clic o `Intro` lo abre en su
  propia ventana. `Ctrl` + `+`/`-`/`0` cambia el tamaño del texto del mensaje.
- **Conversaciones.** Los mensajes de un mismo hilo se agrupan (se puede apagar en Ajustes →
  Correo).
- **Escribir.** «Nuevo» (`Ctrl+N` con los atajos de Outlook), «Responder», «Responder a todos» y «Reenviar» en la barra o
  con los atajos de tu esquema. Arrastra archivos a la ventana para adjuntarlos.
- **Enviar después y deshacer el envío.** Los mensajes esperan unos segundos en la bandeja de
  salida antes de irse, para que puedas arrepentirte. También puedes programar el envío.
- **Buscar.** El campo de arriba busca en todas tus cuentas, también sin conexión.
- **Organizar.** Archivar, mover, banderas, etiquetas y categorías, «Posponer» y reglas
  («Cuando llegue un correo de… → Moverlo a…») en Ajustes → Reglas.
- **Correo seguro.** Firmar y cifrar con OpenPGP o S/MIME desde el redactor; las claves se
  administran en la paleta: «Claves de cifrado…».
- **Imprimir o guardar en PDF** desde la paleta: «Imprimir mensaje».

## Calendario

- Vistas de día, semana laboral, semana, mes y agenda (`1`–`5` o `Ctrl+Alt+1`–`5`, según el
  esquema). «Hoy» vuelve al día actual.
- **Crear un evento:** arrastra en la rejilla o pulsa «Nuevo» estando en el calendario. Si
  agregas invitados, IureDesk avisa que les mandará una invitación.
- **Videollamada:** en cuentas de Google o Microsoft, el editor ofrece agregar Google Meet o
  Teams. En «Mi día» y en los recordatorios aparece «Unirse».
- **Invitaciones:** las que llegan por correo se aceptan, se marcan como «Tal vez» o se rechazan
  desde el propio mensaje.
- A la izquierda eliges qué calendarios ver; puedes agregar una segunda zona horaria.

## Contactos, tareas y notas

- **Contactos:** doble clic o `Intro` para editar; «+» para uno nuevo. Se sincronizan con tu
  cuenta (Google, Microsoft, iCloud o CardDAV).
- **Tareas:** con vencimiento, prioridad, recordatorio y listas; se sincronizan con Google Tasks,
  Microsoft To Do o CalDAV.
- **Notas:** se escriben en [IureEditor](https://github.com/ellaguno/iureditor). Si no lo tienes
  instalado, el módulo de Notas no aparece. Cada nota es un archivo Markdown en la carpeta que
  elijas (Ajustes → General → Carpeta de notas).

## Mi día

Lo primero que ves al abrir: agenda de hoy, tareas que vencen, correo importante y recordatorios.
Puedes mover y quitar tarjetas, cambiar el fondo (Ajustes → Apariencia) y ver el clima. En Ajustes
→ General puedes pedir que IureDesk abra donde lo dejaste.

## Iurefficient y el asistente ✦

- **Iurefficient** (opcional): en Ajustes → Integraciones inicia sesión con tu instancia. Junto a
  cada correo verás a la persona o cliente, su lead, oportunidad, caso o ticket, y podrás
  vincular el mensaje o crear un lead o un ticket.
- **Asistente ✦:** resume hilos, propone respuestas, saca fechas y tareas de un correo y responde
  preguntas sobre tu día. Usa la IA de Iurefficient o el proveedor que elijas (OpenAI, Anthropic,
  Ollama…). **Antes de mandar datos a un proveedor externo, IureDesk te muestra qué saldrá del
  equipo** y te pide permiso. Si no hay asistente configurado, no se ve nada de esto.
- **Apps hermanas:** IureEditor, IureTranscribe, IureVoz, IureOCR, IureDav e IureProject se
  ofrecen solo si están instaladas.

## Importar desde otro programa

Paleta → «Importar correo de Outlook, Thunderbird, Apple Mail…». Lee los datos en tu equipo
(Thunderbird, Apple Mail, archivos `.pst`, `.mbox`, `.eml`, Google Takeout), muestra el avance y
se puede reanudar si se corta. Al final ves un informe con lo que se importó y lo que no.

## Ajustes útiles

- **Apariencia:** tema claro, oscuro, de alto contraste o el del sistema; tres densidades;
  tipografía y **tamaño del texto**.
- **General:** atajos de Outlook, Gmail o Apple Mail; idioma; abrir al iniciar sesión;
  **actualizaciones** (buscar versiones nuevas cada día).
- **Privacidad:** cuándo mostrar las imágenes remotas.
- **Avanzado → Rendimiento:** datos para reportar lentitud («Copiar informe»).

## Privacidad

- Tu correo se guarda **solo en tu equipo** y en los servidores de tus cuentas. IureDesk no tiene
  servidores propios ni manda estadísticas de uso.
- Las contraseñas y los tokens van en el llavero del sistema.
- Las **imágenes remotas** de los correos están bloqueadas hasta que las pides (evita que el
  remitente sepa que abriste el mensaje); los píxeles de rastreo no se piden nunca.
- La búsqueda de actualizaciones solo pregunta a GitHub cuál es la última versión.

## Preguntas frecuentes

**Google dice «Esta app no está verificada».** Ver [Gmail](#gmail-y-google-workspace): mientras
dure la beta, usa una contraseña de app o pide que agreguen tu cuenta como usuario de prueba.

**Windows avisa de que la app no es de un editor conocido.** Pulsa «Más información» → «Ejecutar
de todas formas». Desaparecerá cuando el instalador de Windows esté firmado.

**No llegan correos nuevos.** Mira el punto de sincronización arriba: si hay un error, el aviso
trae «Reintentar» y el texto dice qué pasó. Pulsa el estado del riel para ver cada cuenta.

**IureDesk se cerró solo.** Al volver a abrir verás «IureDesk se cerró inesperadamente». Con
«Reportar en GitHub» se abre un reporte que revisas antes de enviar; el informe completo queda en
el portapapeles para pegarlo.

**¿Dónde están mis datos?** En la carpeta de datos del sistema (`~/.local/share/iuredesk` en
Linux, `%APPDATA%\iuredesk` en Windows, `~/Library/Application Support/iuredesk` en macOS).

**¿Cómo actualizo?** Con iureapps, o bajando el instalador nuevo. IureDesk te avisa cuando hay
una versión nueva (Ajustes → General → Actualizaciones).

**Encontré un error o quiero proponer algo.** Abre un *issue* en
<https://github.com/ellaguno/iuredesk/issues>.
