# IureDesk en Microsoft 365: guía para TI

*English version below.*

IureDesk es un cliente de escritorio de correo, calendario, contactos y tareas (Linux, macOS y
Windows), de código abierto (Apache-2.0). Con Microsoft 365 entra por **Microsoft Graph** con
OAuth 2 (código + PKCE, cliente público, sin secreto) en nombre del usuario. No hay servidor
intermedio: los datos van del inquilino al equipo del usuario y se guardan ahí.

## Permisos que pide (todos delegados)

| Permiso | Para qué |
|---|---|
| `Mail.ReadWrite`, `Mail.Send` | Leer, ordenar y enviar el correo del usuario |
| `Calendars.ReadWrite` | Su calendario e invitaciones |
| `Contacts.ReadWrite` | Sus contactos |
| `Tasks.ReadWrite` | Microsoft To Do |
| `User.Read`, `openid`, `email`, `profile` | Saber quién inició sesión |
| `offline_access` | Seguir sincronizando sin pedir la contraseña cada hora |
| `IMAP.AccessAsUser.All`, `POP.AccessAsUser.All`, `SMTP.Send` (Outlook) | Reserva por IMAP/SMTP con el mismo inicio de sesión |

Ningún permiso es de aplicación: IureDesk solo ve lo que ve el usuario que inició sesión.

## Aprobar IureDesk para la organización

Si su inquilino no deja que los usuarios aprueben apps por su cuenta, el usuario verá «El
administrador de tu organización no permite…» y un botón **«Copiar el enlace de aprobación para
TI»**. Ese enlace lleva a
`https://login.microsoftonline.com/<inquilino>/v2.0/adminconsent?client_id=…&scope=…`.

1. Ábralo con una cuenta de administrador global, de aplicaciones o de aplicaciones en la nube.
2. Revise los permisos y elija **Aceptar**.
3. Al final Microsoft redirige a `http://localhost`, que **no carga**: es normal, la aprobación
   ya quedó hecha.

También puede hacerlo desde el centro de administración de Entra: *Aplicaciones empresariales →
IureDesk → Permisos → Conceder consentimiento de administrador*.

**Limitar a ciertos usuarios:** en *Aplicaciones empresariales → IureDesk → Propiedades*, active
«¿Se requiere asignación?» y asigne usuarios o grupos. A quien no esté asignado se le explica que
la organización no lo permite (AADSTS50105).

## Acceso condicional

Si una directiva exige un equipo administrado (Intune), unido al dominio o una app cliente
aprobada (AADSTS53000–53003), IureDesk no puede entrar y se lo dice al usuario tal cual; no
intenta saltarse la directiva. Para permitirlo, excluya la aplicación IureDesk de esa directiva o
cree una que la admita con los controles que su organización exija (por ejemplo, MFA).

## Exchange Server local

Exchange 2016/2019/Subscription Edition sin Graph se atenderá por EWS (en desarrollo, F8.2 de
`docs/gaps.md`). Mientras, funciona por IMAP/SMTP si están habilitados en el servidor.

---

# IureDesk on Microsoft 365: guide for IT

IureDesk is an open-source (Apache-2.0) desktop mail, calendar, contacts and tasks client for
Linux, macOS and Windows. With Microsoft 365 it uses **Microsoft Graph** with OAuth 2 (auth code +
PKCE, public client, no secret) on behalf of the user. There is no server in between: data goes
from your tenant to the user's computer and is stored there.

## Permissions requested (all delegated)

`Mail.ReadWrite`, `Mail.Send` (mail), `Calendars.ReadWrite` (calendar and invitations),
`Contacts.ReadWrite` (contacts), `Tasks.ReadWrite` (Microsoft To Do), `User.Read`, `openid`,
`email`, `profile` (who signed in), `offline_access` (keep syncing), and the Outlook
`IMAP.AccessAsUser.All`, `POP.AccessAsUser.All`, `SMTP.Send` scopes (IMAP/SMTP fallback with the
same sign-in). No application permissions: IureDesk only sees what the signed-in user sees.

## Approving IureDesk for the organization

If your tenant does not let users approve apps themselves, the user sees "Your organization's
administrator does not allow…" and a **"Copy the approval link for IT"** button. The link opens
`https://login.microsoftonline.com/<tenant>/v2.0/adminconsent?client_id=…&scope=…`.

1. Open it with a Global, Application or Cloud Application Administrator account.
2. Review the permissions and choose **Accept**.
3. Microsoft then redirects to `http://localhost`, which **does not load**: that is expected, the
   approval is done.

Or in the Entra admin center: *Enterprise applications → IureDesk → Permissions → Grant admin
consent*. To limit it to some users, turn on "Assignment required?" in the app's *Properties* and
assign users or groups (others get AADSTS50105, explained to them in plain words).

## Conditional Access

If a policy requires a managed (Intune), domain-joined device or an approved client app
(AADSTS53000–53003), IureDesk cannot sign in and tells the user so; it never tries to bypass the
policy. To allow it, exclude the IureDesk application from that policy or create one that admits
it with the controls your organization requires (for example, MFA).

## On-premises Exchange Server

Exchange 2016/2019/Subscription Edition without Graph will be supported through EWS (in
development, F8.2 in `docs/gaps.md`). Meanwhile it works over IMAP/SMTP if the server has them
enabled.
