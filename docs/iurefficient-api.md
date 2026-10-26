# API de Iurefficient para IureDesk (F3.0)

Qué ofrece hoy una instancia de Iurefficient a un PIM de escritorio, qué falta y qué se le pide
al equipo del servidor. Todo IureDesk habla con la instancia **sólo** a través de
`iurefficient-connect` (PLAN.md §7, §13.3).

**Fuentes leídas** (sólo lectura): `iur-mcp-dia-wt/instance/backend` en la rama
`feat/mcp-mi-dia` (commit `65b50632`, base v4.111.0) comparada con `main` (v4.114.0), y
`iur-lms-sso-wt` (v4.101.2, plugin Aula: no aporta nada al PIM). IureVoz
(`iurevoz/src/iure.rs`, `resumen.rs`). Las rutas son relativas al dominio de la instancia.

## 0. Autenticación

| Credencial | Dónde vale | Notas |
|---|---|---|
| Sesión de cookies (JWT httpOnly + CSRF de doble envío) | Toda la API REST `/api/**` | `POST /api/auth/login` → (`requires_totp`) `POST /api/auth/verify-totp` → `POST /api/auth/refresh`. Ya implementado en `connect::rest::Session`, compartida entre apps por el llavero. |
| `iurdav_…` (contraseña de aplicación) | WebDAV de documentos (`/webdav`) | Basic auth. `connect::api::create_webdav_token`. |
| `iurmcp_…` (token de agente) | Servidor MCP `/api/mcp` (sólo lectura) | Bearer. Lo usa IureVoz. |
| `iurapi_…` (token de servicio) | **Sólo** API externa de tickets `/api/plugins/tickets/ext/v1` | Alcances `tickets:read/write/comment/internal`, `webhooks:manage`; los de escritura sólo los concede un administrador. No sirve para agenda ni contactos. |

Desde v4.114 (`main`), si el usuario no aceptó los términos vigentes, cualquier `/api/**`
responde **403** `{"code": "terms_acceptance_required"}`: IureDesk debe mostrarlo como "abre
Iurefficient en el navegador y acepta los términos", no como sesión inválida.

## 1. Lo que existe, por área

### 1.1 Agenda / calendario

| ¿Existe? | Endpoint | Campos / parámetros | Notas |
|---|---|---|---|
| Sí | `GET /api/calendar-events?start=&end=&case_id=&event_type=` | `{events: [Event], total}` | Sólo los eventos **propios** del usuario. Sin paginación, sin `updated_since`. |
| Sí | `GET/PUT/DELETE /api/calendar-events/<id>`, `POST /api/calendar-events` | Event: `id, title, description, start_datetime, end_datetime` (ISO, UTC con `Z`), `all_day, location, meeting_url, meeting_provider, event_type` (meeting/appointment/reminder/personal/court/deadline/other), `color, case_id, case{id,title}, is_recurring, recurrence_rule` (RRULE), `reminder_minutes, external_id, external_provider, created_at, updated_at` | Al crear: `title`, `start_datetime`, `end_datetime` obligatorios; con zona se convierte a UTC, sin zona se toma como UTC. **No hay asistentes**, ni UID iCalendar, ni estado de participación. `recurrence_rule` no se puede fijar por la API (sólo lo llena la sincronización con Google/Microsoft). |
| Sí | `GET /api/calendar-events/types` | catálogo fijo | |
| Sí | `GET /api/deadlines?case_id=&upcoming=&standalone=` | `deadline_date` (fecha de pared), `deadline_type, status, location, task_id, is_overdue` | Plazos = tareas de una sola fecha. |
| Parcial | `POST /api/tasks/export-ics` (`{task_ids}` ≤ 500) | `.ics` RFC 5545 (VEVENT por tarea) | Exportación única, no un feed suscribible. `POST /api/tasks/send-digest` manda el resumen semanal por correo con `.ics`. |
| Sí (integración) | `/api/v1/integrations/…` (Google, Microsoft; `calendar/events`, `contacts`, `tasklists`, `drive`) | | La **instancia** sincroniza con Google/Microsoft. Apple/Nextcloud aparecen como CalDAV en `/providers` pero `configured: false`: no hay cliente CalDAV hecho. |
| **No** | Servidor CalDAV, feed iCal suscribible, free/busy, invitaciones iMIP | | Ver §2.1. |

### 1.2 Contactos, clientes y personas del CRM

| ¿Existe? | Endpoint | Campos | Notas |
|---|---|---|---|
| Sí | `GET /api/contacts?type=&entity_type=&parent_client_id=&include_counts=` | `{contacts: [Contact]}`. Contact: `id, contact_type` (prospect/lead/witness/opposing_counsel/expert/court_staff/vendor/insurance/other), `entity_type` (individual/company), `first_name, last_name, company, title, email, phone, phone_alternate, address_line1/2, city, state, postal_code, country, notes, tags, display_name, parent_client_id, external_id/provider, created_at, updated_at, comment_count` | **Sin búsqueda, sin paginación, sin `updated_since`**: devuelve todo lo visible. En modo aislamiento sólo lo propio. |
| Sí | `GET/PUT/PATCH/DELETE /api/contacts/<id>`, `POST /api/contacts` | igual | `POST` puede responder 409 por duplicado. `POST /<id>/convert-to-client`, `GET /export`, `POST /import`. |
| Sí | `GET /api/clients?active=&type=&relationship=&paginate=&page=&limit=` | `{clients, pagination{page,limit,total,total_pages,has_next,has_prev}}`. Client: `id, client_type, first_name, last_name, company_name, tax_id, email, phone, mobile, address, city, state, postal_code, country, display_name, notes, active, relationship_type` (customer/supplier/both), `responsible_user_id, created_at, updated_at` | `limit` ≤ 200. CRUD completo, `merge`, `import/export`. |
| Sí | `GET /api/search?q=` | `results{cases, clients, documents, tasks, plugins[{key,label,items[{id,title,subtitle,url,badge}]}]}` | ≤ 10 por grupo. El CRM aporta leads/oportunidades en `plugins`. Útil para el diálogo "vincular a…". |
| Sí (plugin `iur-crm`) | `GET /api/plugins/crm/leads?search=&status=&organization_id=&has_email=&mine=&limit=&offset=` | `{total, limit, offset, items: [Lead]}`. Lead: `id, lead_type` (person/company), `name, first_name, last_name, company_name, email, email2, email3, phone, phone2, phone3, address, position, location, url, organization_id/name, org_role, status` (active/converted/archived), `do_not_contact, tags, owner_user_id, updated_at` | `limit` ≤ 500. También `organizations` (CRUD, `people`, jerarquía). |
| **No** | CardDAV, `updated_since` / borrados, foto del contacto | | Ver §2.2. |

### 1.3 CRM: leads, oportunidades, actividades y correo de la venta

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `GET/POST /api/plugins/crm/leads`, `GET/PUT/DELETE /leads/<id>`, `GET /leads/<id>/related` | `POST` 409 `duplicate_email` (se fuerza con `allow_duplicate_email`). |
| Sí | `GET/POST /opportunities` (`status`, `search`), `GET/PUT/DELETE /opportunities/<id>`, `POST /<id>/move-stage`, `/<id>/case` | |
| Sí | `GET /activities?lead_id=&opportunity_id=&organization_id=&activity_type=&only_pending=`, `POST /activities`, `PUT/DELETE /activities/<id>`, `POST /<id>/complete` | `activity_type`: call/meeting/email/whatsapp/sms/note/task. Con `scheduled_at` y `schedule_in_calendar` (por omisión `true`) crea un evento en la agenda. |
| Sí | `GET/POST /leads/<id>/documents`, `/opportunities/<id>/documents` (multipart `file` o `{document_id}`) | Ya en connect (`crm_attach_file`, `crm_attach_document`). |
| **Sí — buzón de ventas** | `GET /emails?lead_id=&opportunity_id=&unassigned=`, `GET /emails/<id>`, `POST /emails/<id>/assign` (`{lead_id}` o `{opportunity_id}`), `GET /opportunities/<id>/reply-address` → `{configured, reply_to, bcc}` | La instancia sondea **su propio** buzón IMAP de ventas. Un correo enviado con `Reply-To: ventas+opp-<id>@…` y `Bcc: ventas@…` queda archivado en la oportunidad (y su respuesta también), sin API nueva. `CrmEmailMessage`: `message_id, in_reply_to, references, direction, from/to/cc, subject, body_text, lead_id, opportunity_id, matched_by` (token/thread/address/manual/none), `attachment_document_ids, email_date`. |
| Sí | `GET /leads/<id>/emails/compose`, `POST /leads/<id>/emails`, `POST /opportunities/<id>/emails` (`to_email, subject, body, body_format, cc`) | Envío 1:1 **desde la instancia** (SMTP del CRM, consume la cuota de campañas). IureDesk no lo usa: el correo sale de la cuenta del usuario. |
| Sí (IA) | `POST /extract` con `{text, persist:false}` → `{returned, preview:[candidatos]}` | Extrae leads de texto libre (firma de un correo). Con `persist:true` encola una importación. |
| Sí (IA) | `POST /opportunities/<id>/research`, `GET /<id>/profile`, `POST /<id>/sales-script`, `POST /<id>/chat` | Perfil de empresa y guion de venta (asíncrono). |
| **No** | Importar un correo **externo** (de la cuenta del usuario) a la historia de un lead/oportunidad por API | Ver §3. |

### 1.4 Tickets / soporte (plugin `iur-tickets`, `/api/plugins/tickets`)

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `GET ?page=&per_page=&status=(active\|open\|…)&search=&case_id=&assigned_to=&queue_id=&reported_by_me=&sla=` | `{tickets, pagination}`. Ticket: `id, ticket_number, title, description, status` (open/in_progress/waiting/resolved/closed), `priority, case_id/title, client_id/name, assigned_to, assignee_name, queue_id/name, category, tags, source` (manual/email/api/catalog), `requester_email/name/type/id, sla_deadline, sla_breached, type_*, state_*, created_at, updated_at`. |
| Sí | `POST` (`title` obligatorio; `description, priority, queue_id, case_id, client_id, category, tags, source, requester_*`), `GET/PUT/DELETE /<id>`, `POST /<id>/transition` | `GET /<id>` trae `comments`, `status_history`, aprobaciones, enlaces. |
| Sí | `POST /<id>/comments` (`content, is_internal, time_spent_minutes`) | **Ojo**: un comentario público en un ticket con `requester_email` hace que la instancia le responda por correo al solicitante. |
| Sí | `GET/POST /<id>/documents` (`{document_id, notes}`), `POST /<id>/attachments`, `GET /response-templates`, `GET /<id>/suggestions` (soluciones de tickets parecidos), `GET /<id>/sla` | |
| Sí | Lectura de correo **propia** (`email-config`: buzón IMAP de la cola) con `TicketEmailMessage` (`message_id, in_reply_to, references, from, to, cc, subject, direction, email_date`) | La instancia crea tickets y comentarios desde su buzón, con hilado por `Message-ID`. |
| Sí | API externa `…/ext/v1` con `iurapi_` (`GET/POST /tickets`, `PATCH`, `/transition`, `/comments`, `/attachments`, `/external-refs`, `by-ref/<system>/<id>`) | Contrato estable y errores `{error:{code,message,details}}`. |
| **No** | Importar un correo externo a un ticket como comentario `source=email` sin disparar la respuesta automática | Ver §3. |

### 1.5 Notas

**No existe** una entidad "nota" en el núcleo ni en los plugins (sólo `notes` como campo de
texto en contactos, clientes, leads y plazos; actividades CRM de tipo `note`). Hoy la única
forma es un documento Markdown (`POST /api/documents` o `PUT` por WebDAV en la carpeta del caso
o en `/General`). Ver §2.4.

### 1.6 Tareas ("mis tareas")

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `GET /api/tasks?assigned_to=<user_id>&status=pending,in_progress&include_case=true&needs_attention=&paginate=&page=&limit=` | `{tasks, pagination}`; incluye tareas **personales** (sin caso) y las de corresponsable. `limit` ≤ 500. Task: `id, case_id, title, description, status, priority, assigned_to_id, assignees, start_date, due_date` (fecha de pared, sin `Z`), `completed_at, location, meeting_url, recurrence_rule, is_overdue, estimated_hours…`. No hay `assigned_to=me`: hay que pasar el id de `GET /api/auth/me`. Sin filtro por rango de fechas. |
| Sí | `POST /api/tasks` (`title` obligatorio; sin `case_id` = personal), `GET/PUT/DELETE /api/tasks/<id>`, `GET /recurrence-occurrences`, `POST /export-ics` | |
| Sí (MCP, rama `feat/mcp-mi-dia`, **sin fusionar** en `main`) | herramientas `mis_tareas` (`cuando`: abiertas/hoy/vencidas/semana), `agenda` (eventos + entregas de un rango), `notificaciones`, `mis_horas`, `buscar_cliente` | IureVoz arma "mi día" con ellas (`resumen.rs`) y cae a `proximos_vencimientos` si la instancia no las ofrece. |

### 1.7 Colaboración (plugin `iur-collab`, `/api/plugins/collab`)

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `GET/POST /entities/<tipo>/<id>/comments`, `PUT/DELETE /comments/<id>` | Tipos: task, calendar_event, deadline, case, client, contact, document, lead, opportunity, quote, invoice, contribuyente, cfdi, inventory_*. **No** ticket (tiene los suyos). Markdown con `@menciones`. |
| Sí | `GET /mentions?unread=&limit=`, `POST /mentions/<id>/read`, `GET /mentions/users` | |
| Sí | Canales de chat: `GET/POST /channels`, `/channels/<id>/messages`, `/channels/unread`, `/channels/<id>/read`; reuniones Jitsi `/meetings`; presencia `/entities/<t>/<id>/presence/*`; feed `/activity` | Sin tiempo real: todo por consulta. |

### 1.8 Documentos

Ya cubierto por connect: `POST /api/documents` (multipart, `case_id`, `client_id`, `title`,
`document_type`, `tags`, `as_version_of`), `GET /api/documents/<id>`, `GET /api/documents?case_id=`,
`GET /<id>/download`, y WebDAV (`/webdav/{Casos}/{número} - {título}/…`, `/General`; `PUT` crea
documento o versión; sin `MKCOL` ni `DELETE`). **Extensiones admitidas** (`config.ALLOWED_EXTENSIONS`):
pdf, doc(x), odt, rtf, txt, md, hojas, presentaciones, imágenes, audio y comprimidos. **`.eml` y
`.msg` no se admiten.**

### 1.9 IA

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `POST /api/chat/global/chat` (`message, system_prompt, document_ids`) | Ya en connect (`global_chat`). Sirve hoy para redactar/resumir/analizar un correo con un `system_prompt` propio; 403 `quota_exceeded`. Variante asíncrona `/global/chat/async` + `GET /messages/<id>/stream` (SSE). |
| Sí | `POST /api/chat/cases/<id>/chat` (y `/stream`, `/async`) | Chat con el contexto del caso (RAG). |
| Sí | `GET /api/documents/<id>/ai-options`, `POST /api/blueprints/<id>/compose`, `GET /compose-status/<task>`, `GET/POST /api/documents/<id>/commitments[/apply]` | Ya en connect (minutas y compromisos). |
| Sí | `POST /api/cases/<id>/ai-summary` → `{summary, generated_at}` | Resumen del caso. |
| Sí | `POST /api/chat/cases/<id>/preview-contact` / `apply-contact`, `detect-task-intent`, `preview-tasks` / `apply-tasks` | Captura de contacto y tareas desde texto, atadas a un caso. |
| Sí | CRM `POST /extract` (`persist:false`), tickets `GET /<id>/suggestions` | |
| **No** | Endpoints específicos de correo (borrador de respuesta, resumen de hilo, análisis intención/fechas/montos, "¿a qué caso/lead/ticket pertenece?") | Se cubre con `global_chat` + `system_prompt`; ver §2.6 para endpoints dedicados. |

### 1.10 Avisos y tiempo real

| ¿Existe? | Endpoint | Notas |
|---|---|---|
| Sí | `GET /api/notifications?unread_only=&limit=` → `{notifications, unread_count}` | Notification: `id, type, title, message, related_type, related_id, link, is_read/read, read_at, created_at`. |
| Sí | `PUT /<id>/read`, `PUT /mark-all-read`, `DELETE /<id>` | |
| Parcial | Web Push: `GET /api/notification-preferences/push/public-key`, `POST/DELETE /push/devices` (`platform`: web/android/ios) | VAPID / FCM. Un cliente de escritorio no puede recibirlo sin un servicio push. |
| **No** | WebSocket / SSE de avisos | El único SSE es el del chat de IA. Hay que consultar periódicamente. Webhooks salientes (`/api/outbound-webhooks`) existen pero exigen una URL pública. |

### 1.11 Correo

- La instancia **no guarda correos de los casos** en el núcleo. Sólo hay correo en: buzón de
  ventas del CRM (`CrmEmailMessage`) y buzones IMAP de colas de tickets (`TicketEmailMessage`),
  ambos leídos por la instancia desde buzones propios.
- SMTP de la instancia (`/api/admin/smtp-settings`, `/api/v1/smtp-config`) sólo para avisos,
  invitaciones, campañas y envío 1:1 del CRM.
- Plantillas de correo: sólo las del CRM (`EmailTemplate`, editor por bloques, `/email/preview`)
  y las respuestas predefinidas de tickets (`/response-templates`). No hay plantillas generales.

### 1.12 Servidor MCP (`/api/mcp`)

Streamable HTTP sin sesión, `Bearer iurmcp_…`, sólo lectura. Herramientas definidas en
`app/services/mcp_tools.py` (`_tools_spec(lang)` → `TOOLS_BY_NAME`; cada una es una función
`(user, **args) -> dict` que hereda la visibilidad del dueño del token): `buscar_en_expedientes`,
`buscar_en_proyecto`, `leer_documento`, `listar_documentos`, `listar_proyectos`,
`detalle_proyecto`, `proximos_vencimientos`, `tareas_del_proyecto` y, en `feat/mcp-mi-dia`,
`mis_tareas`, `agenda`, `notificaciones`, `mis_horas`, `buscar_cliente`.

## 2. Lo que falta en el servidor (propuesta)

Decisión del dueño (PLAN.md §13.3): **REST + CalDAV/CardDAV**. Principios comunes:
- Misma visibilidad que la pantalla (aislamiento, supervisión, alcance del CRM).
- Toda colección sincronizable expone **`updated_since`** + **borrados** (lápidas) y `ETag`.
- Fechas: instantes en UTC con `Z`; fechas de pared sin zona (como hoy).
- Errores `{"error": {"code", "message"}}` (como la API ext v1 de tickets).

### 2.1 CalDAV

Montaje propuesto junto al WebDAV actual (WsgiDAV no implementa CalDAV/CardDAV: hace falta un
proveedor propio o un servicio aparte en el mismo dominio):

```
/.well-known/caldav   → 301 /dav/
/.well-known/carddav  → 301 /dav/
/dav/principals/<user_id>/                 current-user-principal, calendar-home-set, addressbook-home-set
/dav/calendars/<user_id>/agenda/           VEVENT  ← calendar_events del usuario (lectura/escritura)
/dav/calendars/<user_id>/tareas/           VTODO   ← tareas asignadas/corresponsable (lectura/escritura)
/dav/calendars/<user_id>/plazos/           VEVENT  ← deadlines visibles (sólo lectura)
/dav/calendars/<user_id>/caso-<case_id>/   VEVENT  ← opcional: eventos y plazos de un caso
```

- Autenticación: Basic con la contraseña de aplicación `iurdav_…` que ya existe (mismo
  llavero, mismo `create_webdav_token`).
- Métodos: `PROPFIND` (Depth 0/1), `REPORT` `calendar-query`, `calendar-multiget` y
  **`sync-collection` (RFC 6578)**, `GET`, `PUT` (con `If-Match`/`If-None-Match: *`), `DELETE`;
  `getctag`/`sync-token` por colección; `supported-calendar-component-set`.
- Mapeo VEVENT ↔ `calendar_events`: `UID` (columna nueva `ical_uid`, única por usuario; se
  conserva la que mande el cliente), `SUMMARY`↔`title`, `DESCRIPTION`, `DTSTART/DTEND` (con
  `VALUE=DATE` si `all_day`), `LOCATION`, `RRULE`↔`recurrence_rule` (hoy no se puede fijar por
  REST: habilitarlo), `EXDATE`/`RECURRENCE-ID` (tabla de excepciones), `VALARM`↔`reminder_minutes`,
  `CATEGORIES`↔`event_type`, `X-IUREFFICIENT-CASE-ID`↔`case_id`, `URL`↔`meeting_url`,
  `ATTENDEE`/`ORGANIZER` (tabla nueva, §2.3), `SEQUENCE`, `LAST-MODIFIED`↔`updated_at`.
- Mapeo VTODO ↔ `tasks`: `SUMMARY, DESCRIPTION, DTSTART`↔`start_date`, `DUE`↔`due_date` (fecha
  de pared), `STATUS` (NEEDS-ACTION/IN-PROCESS/COMPLETED/CANCELLED, como `ICalService.STATUS_MAP`),
  `PRIORITY` (1/3/5/9), `COMPLETED`, `RELATED-TO`↔`parent_task_id`, `X-IUREFFICIENT-CASE-ID`.
- Feed sólo lectura adicional (para Outlook/Google sin CalDAV):
  `GET /api/calendar/feed/<token>.ics` con token revocable por usuario.

### 2.2 CardDAV

```
/dav/addressbooks/<user_id>/contactos/   ← contacts (lectura/escritura)
/dav/addressbooks/<user_id>/clientes/    ← clients (lectura; escritura si el rol lo permite)
/dav/addressbooks/<user_id>/crm/         ← leads del CRM visibles (lectura)
/dav/addressbooks/<user_id>/equipo/      ← usuarios de la instancia (lectura)
```

- `REPORT addressbook-query`, `addressbook-multiget`, `sync-collection`; vCard 4.0 (y 3.0 si el
  cliente lo pide en `Accept`).
- Mapeo: `UID` (= id o `vcard_uid` nuevo), `FN`↔`display_name`, `N`↔`last_name;first_name`,
  `ORG`↔`company`/`company_name`/`organization_name`, `TITLE`↔`title`/`position`, `EMAIL`
  (hasta 3 en leads), `TEL;TYPE=work/cell`↔`phone`/`mobile`/`phone_alternate`, `ADR`↔dirección,
  `NOTE`↔`notes`, `CATEGORIES`↔`tags`, `KIND:org` si `entity_type/client_type = company`,
  `X-IUREFFICIENT-TYPE` (contact/client/lead/user), `X-IUREFFICIENT-CLIENT-ID`↔`parent_client_id`,
  `REV`↔`updated_at`, `PHOTO` (nuevo, opcional).

### 2.3 REST: agenda, contactos y tareas

```http
GET /api/calendar-events?start=&end=&updated_since=2026-10-26T00:00:00Z&include_deleted=true
→ {"events": [...], "deleted": [{"id": "…", "deleted_at": "…Z"}], "sync_token": "…"}

POST /api/calendar-events          (campos nuevos)
{"title": "…", "start_datetime": "…Z", "end_datetime": "…Z", "ical_uid": "abc@iuredesk",
 "recurrence_rule": "FREQ=WEEKLY;BYDAY=MO", "timezone": "America/Mexico_City",
 "attendees": [{"email": "ana@cliente.mx", "name": "Ana", "role": "REQ-PARTICIPANT",
                "partstat": "NEEDS-ACTION", "user_id": null, "contact_id": "…"}]}

POST /api/calendar-events/import-ics   (multipart `file` o text/calendar; invitación iMIP)
→ {"event": {...}, "method": "REQUEST", "created": true}

GET  /api/calendar-events/freebusy?users=<id>,<id>&start=&end=
→ {"busy": {"<user_id>": [{"start": "…Z", "end": "…Z"}]}}

GET /api/contacts?search=ana&page=1&limit=100&updated_since=…&include_deleted=true
→ {"contacts": [...], "deleted": [...], "pagination": {...}}
GET /api/clients?search=&updated_since=…   (mismo patrón)
GET /api/plugins/crm/leads?updated_since=…  (mismo patrón)

GET /api/tasks?assigned_to=me&due_from=2026-10-26&due_to=2026-11-02&updated_since=…
```

### 2.4 Notas

Entidad nueva, personal, Markdown, opcionalmente ligada a una entidad:

```http
GET    /api/notes?updated_since=&linked_type=&linked_id=&search=&page=&limit=
POST   /api/notes      {"title": "…", "content": "# …", "content_format": "markdown",
                        "links": [{"type": "case", "id": "…"}], "pinned": false, "color": null}
GET/PUT/DELETE /api/notes/<id>
→ Note: {"id", "title", "content", "content_format", "links", "pinned", "color",
         "created_at", "updated_at", "deleted_at"}
```
Alternativa mínima si no se quiere entidad nueva: `document_type = "note"` + filtro
`GET /api/documents?document_type=note&owner=me&updated_since=` y lectura del cuerpo por WebDAV.

### 2.5 Avisos en vivo

```http
GET /api/notifications/stream          (text/event-stream, cookie de sesión)
event: notification
data: {"id": "…", "type": "mention", "title": "…", "related_type": "case", "related_id": "…"}
```
Mínimo aceptable: `GET /api/notifications?since=<iso>` barato (sin contar todo) para consultar
cada 60 s, y lo mismo para `mentions` y `channels/unread`.

### 2.6 IA para correo

Hoy se resuelve con `global_chat` + `system_prompt`. Endpoints dedicados (mejor control de
cuota, auditoría y contexto de la instancia):

```http
POST /api/ai/email/analyze
{"subject": "…", "from": "ana@cliente.mx", "to": ["…"], "date": "…Z", "body_text": "…",
 "thread": [{"from": "…", "date": "…", "body_text": "…"}]}
→ {"intent": "support_request|sales_inquiry|scheduling|legal_matter|other",
   "summary": "…", "dates": [{"text": "el martes", "iso": "2026-10-27"}],
   "commitments": [{"title": "…", "due_date": "…", "assignee_hint": "…"}],
   "amounts": [{"text": "$12,000", "value": 12000, "currency": "MXN"}],
   "suggested_links": [{"type": "case|client|contact|lead|opportunity|ticket", "id": "…",
                        "label": "…", "reason": "from_email|thread|mention", "score": 0.92}]}

POST /api/ai/email/draft-reply
{"email": {...como arriba...}, "instructions": "acepta y propone jueves", "tone": "formal",
 "language": "es", "link": {"type": "ticket", "id": "…"}, "variants": 3}
→ {"drafts": [{"subject": "Re: …", "body_text": "…", "body_html": "…"}]}

POST /api/ai/email/summarize-thread   {"thread": [...]} → {"summary": "…", "open_questions": [...]}
```
`suggested_links` puede salir sin IA: coincidencia del remitente con `clients.email`,
`contacts.email`, `leads.email*`, `tickets.requester_email`, y del `In-Reply-To` con los
`Message-ID` ya vinculados (§3).

## 3. Vincular un correo externo a un caso, lead o ticket

El correo vive en la cuenta del usuario (IMAP/Graph/Gmail); la instancia guarda **una
referencia** y, si el usuario lo pide, **una copia** como documento.

### 3.1 Lo que se puede hacer hoy (sin cambios en el servidor)

1. **Caso / cliente**: exportar el correo a PDF o `.txt`/`.md` en IureDesk y subirlo con
   `POST /api/documents` (`case_id` o `client_id`, `document_type: "other"`, `tags: ["email"]`,
   `description` con `Message-ID`, remitente y fecha). Los adjuntos, como documentos aparte.
2. **Lead / oportunidad**: lo anterior + `crm_attach_document`, y registrar
   `POST /api/plugins/crm/activities` `{activity_type: "email", subject, description, lead_id |
   opportunity_id, completed_at}`. Para correos **nuevos** a una oportunidad: poner
   `Reply-To` y `Bcc` de `GET /opportunities/<id>/reply-address`; la instancia archiva envío y
   respuestas en el buzón de ventas.
3. **Ticket**: documento + `POST /<id>/documents {document_id}`, y un comentario **interno**
   (`is_internal: true`) con el resumen; nunca público si el ticket tiene `requester_email`
   (la instancia reenviaría el comentario por correo).

### 3.2 Lo que se pide al servidor

Habilitar `eml` (y opcionalmente `msg`) en `ALLOWED_EXTENSIONS` con `document_type: "email"`
(vista previa: cabeceras + texto saneado; RAG sobre el texto) y un recurso de vínculos:

```http
POST /api/email-links
{
  "message_id": "<CAF=abc@mail.gmail.com>",          // obligatorio, clave de idempotencia
  "in_reply_to": "<…>", "references": ["<…>", "<…>"],
  "subject": "Contrato de arrendamiento",
  "from": {"email": "ana@cliente.mx", "name": "Ana Ruiz"},
  "to": [{"email": "yo@despacho.mx", "name": null}], "cc": [], "bcc": [],
  "date": "2026-10-26T15:04:05Z",
  "direction": "inbound",                            // inbound | outbound
  "snippet": "Primeras 500 letras del texto…",
  "body_text": "…",                                  // opcional (si el usuario permite copiarlo)
  "has_attachments": true,
  "targets": [{"type": "case", "id": "…"}, {"type": "lead", "id": "…"}],
                                                     // case | client | contact | lead | opportunity | ticket | task
  "store": {"eml": true, "attachments": ["contrato.pdf"]},
  "ticket": {"as_comment": true, "is_internal": false, "notify_requester": false},
  "crm": {"log_activity": true}
}
→ 201 {"link": {"id": "…", "message_id": "…", "targets": [...],
                "document_id": "…",               // si store.eml
                "upload_url": "/api/email-links/<id>/eml",
                "attachment_document_ids": []}}
→ 200 si ese message_id ya estaba vinculado (devuelve el vínculo, añade targets nuevos)

PUT    /api/email-links/<id>/eml              (multipart `file`, message/rfc822, ≤ 25 MB)
POST   /api/email-links/<id>/attachments      (multipart `file`, uno por petición)
GET    /api/email-links?message_id=<…>        → vínculos de ese correo (para el panel de IureDesk)
GET    /api/email-links?target_type=case&target_id=…&page=&limit=   → correos de la entidad
GET    /api/email-links/suggest?from=ana@cliente.mx&in_reply_to=<…>&subject=…
       → {"suggestions": [{"type", "id", "label", "reason"}]}
DELETE /api/email-links/<id>?target_type=&target_id=   (quita un destino; sin destinos se borra)
```

Comportamiento esperado: reutilizar las tablas existentes en vez de duplicar — para `ticket`,
crear `TicketEmailMessage` + `TicketComment(source="email")` **sin** disparar
`send_ticket_reply_task` cuando `notify_requester` es `false`; para `lead`/`opportunity`, crear
`CrmEmailMessage(matched_by="manual")` y una `Activity(email)`; para el resto, tabla nueva
`email_links` (`id, message_id, thread_key, subject, from, to, cc, date, direction, snippet,
document_id, created_by, created_at`) + `email_link_targets (link_id, target_type, target_id)`.
El hilado usa `message_id`/`in_reply_to`/`references`, como ya hacen tickets y CRM. Un correo
vinculado debe aparecer en la línea de tiempo de la entidad y en `GET /api/search`.

## 4. Otros pedidos menores

- Fusionar `feat/mcp-mi-dia` (IureVoz y el panel "Hoy" de IureDesk dependen de `agenda`,
  `mis_tareas`, `notificaciones`).
- `assigned_to=me` en `GET /api/tasks`.
- Catálogo de apps: `iuredesk` en `/api/system-settings` / descargas, como las demás apps.
- `GET /api/contacts` paginado: hoy devuelve todo de una vez.
- Alcance `iurapi_` de sólo lectura para agenda/contactos (integraciones sin sesión).

## 5. Lo que ya cubre `iurefficient-connect` (rama `iuredesk`)

Módulo nuevo `connect::pim` (más `Session::put_json` / `delete_json`):

| Área | Funciones |
|---|---|
| Plugins | `plugin_available` |
| Agenda | `events`, `event`, `create_event`, `update_event`, `delete_event` (`EventInput`) |
| Contactos | `contacts`, `contact`, `create_contact`, `update_contact` (`ContactInput`), `clients`, `client`, `search` |
| Tareas | `tasks_assigned_to`, `my_open_tasks`, `task`, `create_task`, `update_task`, `set_task_done` (`TaskInput`) |
| Avisos | `notifications`, `mark_notification_read`, `mark_all_notifications_read` |
| Colaboración | `comments`, `add_comment`, `mentions`, `mark_mention_read` |
| Tickets | `tickets` (`TicketQuery`), `ticket`, `create_ticket` (`NewTicket`, `source: "api"`), `set_ticket_status`, `ticket_comment`, `ticket_attach_document` |
| CRM | `crm_people`, `crm_create_lead` (`NewLead`), `crm_activities`, `crm_emails`, `crm_reply_address` (además de `crm_list`, `crm_activity`, `crm_attach_*` que ya existían) |
| IA | `case_ai_summary`, `crm_extract_leads` (además de `global_chat`, `compose`, `commitments`) |
