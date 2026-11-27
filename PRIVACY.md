# Política de privacidad de IureDesk / IureDesk privacy policy

*Última actualización / Last updated: 2026-11-27* · [Español](#español) · [English](#english)

## Español

IureDesk (`iuredesk`) es una app de escritorio de correo, calendario, contactos, notas y tareas.
Forma parte de la suite Iurefficient y la publica Eduardo Llaguno Velasco («nosotros»). Esta
política explica qué datos maneja la app (incluidos su línea de comandos y su servidor MCP).

**Resumen:** IureDesk no tiene telemetría, analítica ni reportes automáticos de fallos, y no nos
envía datos tuyos: no operamos ningún servidor que reciba tu correo. Tus mensajes, eventos,
contactos y notas se guardan **en tu equipo** y se sincronizan **directamente** con los servidores
de las cuentas que tú agregas. El asistente de IA es opcional, se usa sólo cuando lo pides y, si el
proveedor es externo, antes te muestra exactamente qué se enviará.

### Tus cuentas
- Puedes agregar cuentas de correo IMAP/POP3 con SMTP, Gmail, Microsoft 365 / Outlook.com,
  iCloud, Exchange (EWS, con NTLM o Kerberos), Iurefficient y servidores de calendario y contactos
  CalDAV/CardDAV (iCloud, Fastmail, Nextcloud y otros). Microsoft 365 se sincroniza con Microsoft
  Graph; Google, por IMAP, CalDAV/CardDAV y la API de Google Tasks (y la de Google Calendar sólo
  para crear una sala de Meet cuando la pides).
- IureDesk se conecta **sólo** a los servidores de esas cuentas, con las direcciones que tú
  configuras o que descubre al agregarlas, para descargar, enviar y sincronizar tu correo,
  calendario, contactos y tareas. Lo que esos servicios hagan con tus datos lo rige su propia
  política (la de tu proveedor de correo o tu organización).
- **Inicio de sesión con Google o Microsoft (OAuth 2.0).** Se abre tu navegador en la página del
  proveedor; IureDesk recibe la autorización en una dirección local de tu equipo (`127.0.0.1`, con
  PKCE) y nunca ve tu contraseña. Pide permiso para leer y enviar correo, y para calendario,
  contactos y tareas (Microsoft: también los buzones y calendarios compartidos contigo). Puedes
  revocar el permiso en la cuenta de Google o Microsoft en cualquier momento.
- **Configuración automática.** Al agregar una cuenta, para averiguar sus servidores IureDesk
  consulta una tabla interna de proveedores conocidos y, si el dominio no está ahí: el propio
  dominio de tu correo (`autoconfig.<dominio>` y `<dominio>/.well-known`, que reciben tu dirección
  completa, como en Thunderbird), la base de datos pública de Mozilla
  (`autoconfig.thunderbird.net`, que recibe sólo el dominio), el servicio Autodiscover de Microsoft
  (que recibe sólo el dominio) o el de tu servidor Exchange, y registros DNS. Sólo ocurre durante
  el alta de la cuenta.

### Qué se guarda en tu equipo
- **Correo, calendario, contactos, tareas y notas** en la carpeta de datos de la app (en Windows,
  `%APPDATA%\iuredesk`): una base de datos local, los mensajes originales con sus adjuntos
  (comprimidos) y un índice de búsqueda. Estos archivos **no están cifrados por IureDesk**; los
  protege la cuenta de usuario de tu sistema (y el cifrado de disco, si lo usas, como BitLocker).
- **Ajustes** (cuentas sin contraseñas, preferencias, configuración del asistente) en la carpeta de
  configuración de la app, y cachés (adjuntos que abres, imágenes remotas que permites, imagen de
  fondo, audio de noticias) en su carpeta de caché, con tope de tamaño y antigüedad.
- **Notas:** cada nota también se guarda como archivo Markdown en `Documentos\IureDesk\Notas`, para
  que la abras con otros programas.
- **Contraseñas y tokens** (de las cuentas, de inicio de sesión OAuth, de servidores DAV, claves
  de API del asistente y contraseñas de tus claves de cifrado) **sólo** en el almacén seguro del
  sistema: Administrador de credenciales de Windows, Llavero de macOS o Secret Service en Linux.
  Nunca en archivos de texto.
- **Cifrado de correo (opcional):** si usas OpenPGP o S/MIME, tus claves se guardan en la carpeta de
  configuración (la clave secreta OpenPGP protegida con su contraseña; el `.p12` de S/MIME con la
  suya). Con Autocrypt, IureDesk aprende las claves públicas que llegan en los correos y adjunta la
  tuya a los que envías.
- **Informes de fallos locales:** si la app falla, deja un archivo de texto con la versión, el
  sistema y el error (sin contenido de tus correos) en su carpeta de datos. **Nada se envía solo:**
  al volver a abrirla puedes copiarlo, abrir la carpeta o reportarlo tú en GitHub.
- **Importación:** importar de Thunderbird, Apple Mail, Outlook (PST), Gmail Takeout, mbox, Maildir,
  `.eml`, `.ics` o `.vcf` ocurre por completo en tu equipo.
- En la versión de la Microsoft Store, desinstalar la app borra su base de datos, ajustes y cachés;
  las notas en `Documentos` y las credenciales del almacén seguro (que comparte con las demás apps
  de la suite) quedan.

### Asistente de IA (opcional)
- El asistente (redactar o reescribir respuestas, resumir un hilo, extraer datos, clasificar,
  minutas de reuniones, el resumen de «Mi día», documentos y chat) usa **IA generativa**. **Nunca
  se ejecuta solo**: sólo cuando tú lo pides con un botón o una orden. Puedes apagarlo en Ajustes,
  también por cuenta.
- Tú eliges el proveedor: tu instancia de **Iurefficient** (el predeterminado; sólo funciona si
  iniciaste sesión en ella), **Anthropic** (Claude) o cualquier servicio **compatible con OpenAI**
  cuya dirección configures (OpenAI, OpenRouter u otros), incluidos modelos **locales** en tu equipo
  (Ollama, llama.cpp), y, de forma experimental, **Microsoft 365 Copilot** de tu organización. Las
  claves de API las pones tú y se guardan en el almacén seguro del sistema.
- Qué se envía: sólo lo necesario para la tarea que pediste, que puede incluir el asunto, remitente,
  destinatarios, fecha y cuerpo del mensaje (recortado), los **nombres** de los adjuntos (**nunca su
  contenido**), la transcripción de una reunión, tu texto y las instrucciones del asistente.
- Con un proveedor **externo** (Anthropic, OpenAI o cualquier servicio que no esté en tu propio
  equipo), IureDesk **te muestra antes exactamente qué se enviará** y espera tu confirmación (puedes
  marcar «no volver a preguntar» para ese proveedor), y por omisión **enmascara** las direcciones de
  correo y los teléfonos (el proveedor ve `[email-1]`, `[tel-1]`…; se restauran sólo en tu equipo).
  Tu instancia de Iurefficient y el Copilot de tu organización se tratan como servicios de tu
  organización: no se pide esa confirmación ni se enmascara por omisión.
- Lo que el proveedor haga con los datos lo rige su política. Las acciones que el asistente
  propone (crear un evento, una tarea, un prospecto o un ticket) siempre se te preguntan antes.

### Iurefficient y la suite (opcional)
- Si inicias sesión en una instancia de **Iurefficient** (la plataforma de tu organización),
  IureDesk se conecta a *esa* instancia para mostrarte el contexto de cada correo (consulta la
  dirección y el nombre de tus interlocutores, también en segundo plano para los remitentes
  recientes de tu bandeja, y los números de caso del asunto), tus avisos (cada pocos minutos), tu
  agenda y tus tareas. Sólo cuando tú lo pides, sube a la instancia un correo vinculado a un caso,
  cliente o ticket (un comentario público en un ticket la instancia lo puede reenviar por correo al
  solicitante), una nota, o crea un prospecto, ticket o tarea. El tratamiento de esos datos lo rige
  la política de privacidad de quien opera la instancia (normalmente tu organización). IureDesk
  nunca guarda tu contraseña de Iurefficient: guarda la sesión en el almacén seguro del sistema y,
  para que las demás apps de la suite arranquen conectadas, la instancia y el correo de la cuenta
  activa en un archivo de configuración compartido (`iurefficient\cuenta-activa.json`, sin
  secretos).
- **Otras apps de la suite**, sólo si están instaladas en tu equipo y cuando usas la función: envía
  a IureTranscribe la grabación y los datos de la cita (título, fecha, lugar, participantes) para
  grabar y transcribir reuniones; a IureOCR un adjunto para hacerlo buscable; a IureDav un adjunto
  para guardarlo en tu unidad; a IureEditor una nota o documento; a IureProject los datos de una
  tarea; y a IureVoz un texto para leerlo en voz alta o para dictar. IureDesk no usa el micrófono:
  lo usan, si las tienes, IureTranscribe e IureVoz.
- **Servidor MCP:** si conectas IureDesk a un asistente (Claude Desktop, VS Code / GitHub Copilot)
  con el botón de Ajustes, ese asistente puede buscar y leer tu correo, tu agenda, tus contactos y
  tus tareas, crear borradores, eventos y notas. **No puede enviar correo.** Lo que el asistente
  envíe a su proveedor lo rige la política de ese asistente. La comunicación entre el servidor MCP y
  la ventana abierta es local (en Windows, una conexión a `127.0.0.1` protegida con un token
  aleatorio).

### Otras conexiones
- **Noticias de «Mi día»:** IureDesk descarga los titulares de `iurefficient.com`,
  `blog.iurefficient.com` y `sesolibre.com` (activado por omisión; se puede apagar) y, sólo si
  pulsas reproducir, el audio del artículo. No envía datos tuyos.
- **Clima (apagado por omisión):** si lo activas, envía a MET Norway (`api.met.no`) las coordenadas
  de la ciudad que elijas y a Open-Meteo el nombre que escribas para buscarla.
- **Imagen del día (opcional):** de Wikimedia Commons, si la eliges como fondo.
- **Imágenes remotas en correos:** bloqueadas por omisión («preguntar»). Si las permites, se
  descargan directamente del servidor del remitente (que recibe tu dirección IP). Aun así, IureDesk
  bloquea los píxeles de rastreo conocidos y las imágenes de 1×1.
- **Bajas de listas de correo:** sólo cuando confirmas, IureDesk avisa al remitente (petición web
  o correo, según el estándar del mensaje).
- **Respuesta automática de vacaciones:** sólo si la activas.
- **Aviso de versiones nuevas:** fuera de la Microsoft Store, IureDesk consulta una vez al día en
  GitHub (`api.github.com`) si hay una versión nueva, sin datos tuyos; se apaga en Ajustes.
- **Enlaces** a la guía de uso, a reportar un fallo o a descargar otras apps se abren en tu
  navegador.
- Como en cualquier conexión, los servidores a los que IureDesk se conecta reciben tu dirección IP.

### Notificaciones
Los avisos del sistema de correo nuevo muestran el remitente y el asunto, y los recordatorios, el
título, la hora y el lugar del evento. Se apagan en Ajustes.

### Actualizaciones
IureDesk no se actualiza solo. En la versión de la Microsoft Store la actualiza la tienda y la app
no busca versiones nuevas ni descarga programas.

### Niños
IureDesk es una herramienta de trabajo y no está dirigida a menores de 13 años.

### Tus opciones y contacto
Puedes usar IureDesk sin el asistente de IA, sin Iurefficient y sin las noticias. Para borrar tus
datos, quita la cuenta en Ajustes o desinstala la app (y, si quieres, borra las notas de
`Documentos\IureDesk` y las credenciales «iuredesk» del almacén seguro del sistema). Para preguntas
sobre esta política, abre un *issue* en <https://github.com/ellaguno/iuredesk/issues>. Si la
política cambia, la nueva versión se publica en esta misma página con su fecha.

## English

IureDesk (`iuredesk`) is a desktop app for mail, calendar, contacts, notes and tasks. It is part of
the Iurefficient suite and is published by Eduardo Llaguno Velasco ("we"). This policy explains
what data the app handles (including its command line and its MCP server).

**Summary:** IureDesk has no telemetry, analytics or automatic crash reports, and sends no data
about you to us: we run no server that receives your mail. Your messages, events, contacts and
notes are stored **on your computer** and synchronized **directly** with the servers of the
accounts you add. The AI assistant is optional, only runs when you ask for it and, if the provider
is external, shows you exactly what will be sent first.

### Your accounts
- You can add IMAP/POP3 mail accounts with SMTP, Gmail, Microsoft 365 / Outlook.com, iCloud,
  Exchange (EWS, with NTLM or Kerberos), Iurefficient, and CalDAV/CardDAV calendar and contact
  servers (iCloud, Fastmail, Nextcloud and others). Microsoft 365 is synchronized with Microsoft
  Graph; Google with IMAP, CalDAV/CardDAV and the Google Tasks API (and the Google Calendar API
  only to create a Meet room when you ask for one).
- IureDesk connects **only** to those accounts' servers, at the addresses you configure or that it
  discovers when you add them, to download, send and synchronize your mail, calendar, contacts and
  tasks. What those services do with your data is governed by their own policy (your mail
  provider's or your organization's).
- **Sign-in with Google or Microsoft (OAuth 2.0).** Your browser opens the provider's page;
  IureDesk receives the authorization at a local address on your computer (`127.0.0.1`, with PKCE)
  and never sees your password. It asks for permission to read and send mail, and for calendar,
  contacts and tasks (Microsoft: also the mailboxes and calendars shared with you). You can revoke
  it in your Google or Microsoft account at any time.
- **Automatic setup.** When you add an account, to find its servers IureDesk checks a built-in
  table of known providers and, if the domain is not there: your mail domain itself
  (`autoconfig.<domain>` and `<domain>/.well-known`, which receive your full address, as in
  Thunderbird), Mozilla's public database (`autoconfig.thunderbird.net`, which receives only the
  domain), Microsoft's Autodiscover service (which receives only the domain) or your Exchange
  server's, and DNS records. This only happens while adding the account.

### What is stored on your computer
- **Mail, calendar, contacts, tasks and notes** in the app's data folder (on Windows,
  `%APPDATA%\iuredesk`): a local database, the original messages with their attachments
  (compressed) and a search index. These files are **not encrypted by IureDesk**; they are protected
  by your system user account (and by disk encryption such as BitLocker, if you use it).
- **Settings** (accounts without passwords, preferences, assistant configuration) in the app's
  configuration folder, and caches (attachments you open, remote images you allow, background
  image, news audio) in its cache folder, limited in size and age.
- **Notes:** each note is also saved as a Markdown file in `Documents\IureDesk\Notas`, so you can
  open it with other programs.
- **Passwords and tokens** (for accounts, OAuth sign-in, DAV servers, assistant API keys and the
  passwords of your encryption keys) **only** in the system's secure store: Windows Credential
  Manager, macOS Keychain or Secret Service on Linux. Never in text files.
- **Mail encryption (optional):** if you use OpenPGP or S/MIME, your keys are stored in the
  configuration folder (the OpenPGP secret key protected by its passphrase; the S/MIME `.p12` by
  its own). With Autocrypt, IureDesk learns the public keys that arrive in mail and attaches yours
  to the messages you send.
- **Local crash reports:** if the app crashes, it leaves a text file with the version, the system
  and the error (no content from your mail) in its data folder. **Nothing is sent automatically:**
  when you reopen it you can copy it, open the folder or report it yourself on GitHub.
- **Import:** importing from Thunderbird, Apple Mail, Outlook (PST), Gmail Takeout, mbox, Maildir,
  `.eml`, `.ics` or `.vcf` happens entirely on your computer.
- In the Microsoft Store version, uninstalling the app deletes its database, settings and caches;
  the notes in `Documents` and the credentials in the secure store (shared with the other suite
  apps) remain.

### AI assistant (optional)
- The assistant (draft or rewrite replies, summarize a thread, extract data, classify, meeting
  minutes, the "My day" summary, documents and chat) uses **generative AI**. **It never runs on
  its own**: only when you ask with a button or a command. You can turn it off in Settings, also
  per account.
- You choose the provider: your **Iurefficient** instance (the default; it only works if you are
  signed in to it), **Anthropic** (Claude) or any **OpenAI-compatible** service whose address you
  set (OpenAI, OpenRouter or others), including **local** models on your computer (Ollama,
  llama.cpp), and, experimentally, your organization's **Microsoft 365 Copilot**. You enter the API
  keys yourself and they are kept in the system's secure store.
- What is sent: only what the task you asked for needs, which may include the subject, sender,
  recipients, date and body of the message (truncated), attachment **names** (**never their
  content**), a meeting transcript, your text and the assistant's instructions.
- With an **external** provider (Anthropic, OpenAI or any service that is not on your own
  computer), IureDesk **shows you exactly what will be sent first** and waits for your
  confirmation (you can tick "don't ask again" for that provider), and by default it **masks**
  email addresses and phone numbers (the provider sees `[email-1]`, `[tel-1]`…; they are restored
  only on your computer). Your Iurefficient instance and your organization's Copilot are treated as
  your organization's services: that confirmation is not requested and masking is off by default.
- What the provider does with the data is governed by its policy. Actions the assistant suggests
  (create an event, a task, a lead or a ticket) are always confirmed with you first.

### Iurefficient and the suite (optional)
- If you sign in to an **Iurefficient** instance (your organization's platform), IureDesk connects
  to *that* instance to show you the context of each message (it looks up your correspondents'
  address and name, also in the background for the recent senders in your inbox, and the case
  numbers in the subject), your notifications (every few minutes), your calendar and your tasks.
  Only when you ask, it uploads to the instance a message linked to a case, client or ticket (the
  instance may email a public ticket comment to the requester), a note, or creates a lead, ticket
  or task. Those data are governed by the privacy policy of whoever runs the instance (usually
  your organization). IureDesk never stores your Iurefficient password: it keeps the session in
  the system's secure store and, so the other suite apps start signed in, the instance and email
  of the active account in a shared settings file (`iurefficient\cuenta-activa.json`, no secrets).
- **Other suite apps**, only if they are installed on your computer and when you use the feature:
  it sends IureTranscribe the recording and the appointment details (title, date, place,
  participants) to record and transcribe meetings; IureOCR an attachment to make it searchable;
  IureDav an attachment to save it to your drive; IureEditor a note or document; IureProject a
  task's details; and IureVoz a text to read aloud or to dictate. IureDesk does not use the
  microphone: IureTranscribe and IureVoz do, if you have them.
- **MCP server:** if you connect IureDesk to an assistant (Claude Desktop, VS Code / GitHub
  Copilot) with the button in Settings, that assistant can search and read your mail, calendar,
  contacts and tasks, and create drafts, events and notes. **It cannot send mail.** What the
  assistant sends to its provider is governed by that assistant's policy. The MCP server talks to
  the open window locally (on Windows, a connection to `127.0.0.1` protected by a random token).

### Other connections
- **"My day" news:** IureDesk downloads the headlines from `iurefficient.com`,
  `blog.iurefficient.com` and `sesolibre.com` (on by default; it can be turned off) and, only if
  you press play, the article's audio. It sends no data about you.
- **Weather (off by default):** if you turn it on, it sends MET Norway (`api.met.no`) the
  coordinates of the city you choose and Open-Meteo the name you type to search for it.
- **Picture of the day (optional):** from Wikimedia Commons, if you choose it as background.
- **Remote images in mail:** blocked by default ("ask"). If you allow them, they are downloaded
  directly from the sender's server (which receives your IP address). Even then, IureDesk blocks
  known tracking pixels and 1×1 images.
- **Unsubscribing from mailing lists:** only when you confirm, IureDesk notifies the sender (web
  request or email, as the message's standard says).
- **Vacation auto-reply:** only if you turn it on.
- **New-version notice:** outside the Microsoft Store, IureDesk checks GitHub (`api.github.com`)
  once a day for a new version, with no data about you; turn it off in Settings.
- **Links** to the user guide, to report a crash or to download other apps open in your browser.
- As with any connection, the servers IureDesk connects to receive your IP address.

### Notifications
System notifications for new mail show the sender and subject; reminders show the event's title,
time and place. Turn them off in Settings.

### Updates
IureDesk does not update itself. In the Microsoft Store version the Store updates it, and the app
neither checks for new versions nor downloads programs.

### Children
IureDesk is a work tool and is not directed at children under 13.

### Your choices and contact
You can use IureDesk without the AI assistant, without Iurefficient and without the news. To
delete your data, remove the account in Settings or uninstall the app (and, if you want, delete the
notes in `Documents\IureDesk` and the "iuredesk" credentials in the system's secure store). For
questions about this policy, open an issue at <https://github.com/ellaguno/iuredesk/issues>. If
the policy changes, the new version is published on this same page with its date.
