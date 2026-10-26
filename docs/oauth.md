# OAuth 2 en IureDesk

Crate `iuredesk-oauth` (capa L3). Flujo de código de autorización con PKCE para apps nativas
(RFC 8252 + RFC 7636): IureDesk abre un receptor HTTP en `127.0.0.1` con puerto efímero, abre el
navegador del sistema (o muestra el enlace para copiarlo), recibe la redirección, comprueba `state`
y canjea el código. El token de refresco va al llavero del sistema (propósito `oauth-refresh`); el
de acceso solo vive en memoria.

## Client id: de dónde salen

Los *client id* **no son secretos** y **no** se escriben en el código. Se buscan en este orden:

1. Ajustes en tiempo de ejecución (`OAuthSettings`: archivo de configuración o política de TI).
2. Variable de entorno al ejecutar.
3. La misma variable al **compilar** (`option_env!`), para los binarios oficiales.

| Proveedor | Variable del client id | Variable del «secreto» |
|---|---|---|
| Google | `IUREDESK_GOOGLE_CLIENT_ID` | `IUREDESK_GOOGLE_CLIENT_SECRET` (opcional) |
| Microsoft | `IUREDESK_MS_CLIENT_ID` | — (cliente público, sin secreto) |

Google entrega a los clientes de escritorio un *client secret* que no es secreto (va dentro de
cada copia de la app, RFC 8252 §8.5), pero su servidor de tokens lo pide igual; por eso se admite.

Compilar los binarios oficiales:

```sh
IUREDESK_GOOGLE_CLIENT_ID=123-abc.apps.googleusercontent.com \
IUREDESK_GOOGLE_CLIENT_SECRET=GOCSPX-... \
IUREDESK_MS_CLIENT_ID=00000000-0000-0000-0000-000000000000 \
cargo build --profile dist -p iuredesk
```

Probar el flujo real sin la app:

```sh
IUREDESK_GOOGLE_CLIENT_ID=... IUREDESK_GOOGLE_CLIENT_SECRET=... \
    cargo run -p iuredesk-oauth --example login -- google
IUREDESK_MS_CLIENT_ID=... cargo run -p iuredesk-oauth --example login -- microsoft tu@empresa.com
```

## Registrar la app en Google (paso a paso)

1. Entra a <https://console.cloud.google.com/> con la cuenta del dueño y crea un proyecto
   («IureDesk»).
2. **APIs y servicios → Biblioteca**: habilita *Gmail API*, *Google Calendar API*, *People API* y
   *Google Tasks API*. (IMAP con XOAUTH2 no necesita la Gmail API, pero la verificación la pide
   para el alcance `https://mail.google.com/`.)
3. **Google Auth Platform → Branding** (antes «Pantalla de consentimiento OAuth»): tipo
   **Externo**; nombre «IureDesk», correo de asistencia, logotipo propio (sin marcas de terceros),
   página principal, política de privacidad y condiciones de servicio (URL públicas en el dominio
   del proyecto; hay que verificar el dominio en Search Console).
4. **Data access (Acceso a datos) → Agregar alcances**: `openid`, `email`, `profile`,
   `https://mail.google.com/` (**restringido**), `.../auth/calendar`, `.../auth/contacts`,
   `.../auth/tasks` (sensibles).
5. **Audience (Público)**: deja el estado de publicación en **Testing (Pruebas)** y añade como
   *usuarios de prueba* las cuentas que usarán IureDesk (máximo **100**). En modo pruebas los
   tokens de refresco **caducan a los 7 días**: los usuarios verán «Vuelve a iniciar sesión» cada
   semana. Es el modo correcto para el desarrollo y la beta privada.
6. **Clients (Clientes) → Crear cliente**: tipo de aplicación **App de escritorio**, nombre
   «IureDesk escritorio». No hace falta registrar URI de redirección: los clientes de escritorio
   aceptan `http://127.0.0.1:<cualquier puerto>`.
7. Copia el **Client ID** y el **Client secret** a `IUREDESK_GOOGLE_CLIENT_ID` y
   `IUREDESK_GOOGLE_CLIENT_SECRET`.
8. **Antes de la beta pública** (PLAN.md §12): pasar a *In production* exige la **verificación de
   la app** y, por el alcance restringido `https://mail.google.com/`, una **evaluación de seguridad
   CASA** (Cloud Application Security Assessment) por un laboratorio autorizado, que se repite cada
   año y tiene costo. Sin ella, la app publicada solo admite 100 usuarios y muestra el aviso de
   «app no verificada». Presupuestar tiempo (semanas) y dinero. Alternativa mientras tanto:
   contraseña de app de Google + IMAP.

## Registrar la app en Microsoft Entra (paso a paso)

1. Entra a <https://entra.microsoft.com/> → **Identidad → Aplicaciones → Registros de
   aplicaciones → Nuevo registro**.
2. Nombre «IureDesk». Tipos de cuenta: **Cuentas en cualquier directorio organizativo y cuentas
   personales de Microsoft** (multiinquilino + Outlook.com).
3. URI de redirección: plataforma **Aplicaciones móviles y de escritorio**, valor
   `http://localhost` (sin puerto: Entra acepta cualquier puerto en tiempo de ejecución).
4. Crear. Copia el **Id. de aplicación (cliente)** a `IUREDESK_MS_CLIENT_ID`.
5. **Autenticación**: en *Configuración avanzada*, **Permitir flujos de cliente público = Sí**.
   No crees secretos ni certificados: es un cliente público con PKCE.
6. **Permisos de API → Agregar un permiso**:
   - *Microsoft Graph → Permisos delegados*: `offline_access`, `openid`, `email`, `profile`,
     `User.Read`, `Mail.ReadWrite`, `Mail.Send`, `Calendars.ReadWrite`, `Contacts.ReadWrite`,
     `Tasks.ReadWrite`, `IMAP.AccessAsUser.All`, `POP.AccessAsUser.All`, `SMTP.Send`.
     (Los tres últimos aparecen bajo Graph en el portal; el token se pide para el recurso
     `https://outlook.office.com/`.)
   - No se piden permisos de EWS: **EWS se retira en Exchange Online**.
7. **Personalización de marca y propiedades**: logotipo propio, URL de la página principal,
   condiciones y privacidad. Recomendado: **comprobación del editor** (*publisher verification*)
   con una cuenta de Microsoft Partner Network; sin ella, los inquilinos que solo permiten apps de
   editores verificados bloquearán IureDesk.
8. Para TI de clientes: muchos inquilinos exigen **consentimiento del administrador**. El
   administrador lo da una vez desde
   `https://login.microsoftonline.com/<inquilino>/adminconsent?client_id=<client id>` o en
   *Aplicaciones empresariales*. Si su organización bloquea IMAP/SMTP AUTH, IureDesk usará Graph.
9. Reservado: los alcances de la API de chat de Microsoft 365 Copilot (asistente ✦, PLAN.md §8)
   están en `MICROSOFT_COPILOT_SCOPES`, vacío por ahora. Añadir un alcance obliga a pedir
   consentimiento de nuevo a todos los usuarios.

## Detalles técnicos

- **Un token por recurso (Microsoft).** El consentimiento se pide una vez con todos los alcances;
  el código se canjea por un token para Outlook (IMAP/POP/SMTP) y con el token de refresco se pide
  otro para Graph. `TokenProvider::access_token(account, TokenAudience::Mail | Api)`. Google: un
  solo token sirve para todo.
- **Rotación.** Microsoft entrega un token de refresco nuevo en cada refresco; se guarda.
- **`invalid_grant`** (revocado, contraseña cambiada, 7 días en modo pruebas de Google):
  `Error::NeedsReconsent`; se borra el token de refresco y la UI pide volver a iniciar sesión con
  `Error::message(es)`.
- **Margen.** Se refresca si el token caduca en menos de 2 minutos. Tras un `401` del servidor de
  correo (`sasl::decode_error(..).is_invalid_token()`), `invalidate(cuenta)` y se reintenta una vez.
- **id_token.** Se lee sin verificar la firma (llega por TLS directamente del servidor de tokens,
  OIDC Core §3.1.3.7) y solo para mostrar el correo.
- **Proveedores genéricos** (Fastmail, Yahoo…): `ProviderConfig::custom(...)` con sus URL y
  alcances cuando registremos la app con ellos.
