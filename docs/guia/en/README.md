# IureDesk user guide

IureDesk brings your mail, calendar, contacts, tasks and notes together in one window. It works
with your own accounts (Gmail, Outlook.com and Microsoft 365, iCloud or any IMAP mail) and does
not need Iurefficient; if you use it, IureDesk connects it to your mail.

[Español](../es/README.md) · [Keyboard shortcuts](shortcuts.md)

> **Beta.** This is a test version. If something breaks, IureDesk saves a report on your
> computer and offers it the next time you open it; nothing is sent unless you choose to.

## Contents

1. [Getting started](#getting-started)
2. [Adding accounts](#adding-accounts)
3. [Mail](#mail)
4. [Calendar](#calendar)
5. [Contacts, tasks and notes](#contacts-tasks-and-notes)
6. [My day](#my-day)
7. [Iurefficient and the ✦ assistant](#iurefficient-and-the--assistant)
8. [Importing from another program](#importing-from-another-program)
9. [Useful settings](#useful-settings)
10. [Privacy](#privacy)
11. [FAQ](#faq)

## Getting started

- **Install.** The easiest way is [iureapps](https://github.com/ellaguno/iureapps), the suite
  launcher, which installs and updates IureDesk. You can also download the installer for your
  system from the [releases page](https://github.com/ellaguno/iuredesk/releases).
- **First run.** IureDesk opens the setup assistant: type your email address and it offers **a
  single button** to sign in. To look around first, run `iuredesk --sample`: a sample mailbox
  that connects to nothing.
- **The window.** On the left is the rail with the modules (My day, Mail, Calendar, Contacts,
  Notes, Tasks and Assistant) and Settings at the bottom. In the middle, the list; on the right,
  whatever you selected.
- **Everything is in the palette.** Press `Ctrl+K` (⌘K on a Mac) and type what you want to do:
  "archive", "new event", "dark theme"… The palette shows each action's shortcut.
- **Always undo.** Archive, delete, move, flag or complete can be undone with `Ctrl+Z` (`Z` with
  Gmail shortcuts) or the "Undo" button in the notice.
- **Offline.** Everything already downloaded can be read without internet. What you do offline is
  queued and sent when you're back.

## Adding accounts

Settings → Accounts → "Add account" (or the palette: "Add account"). Type your email address:
IureDesk finds the server settings for you.

### Gmail and Google Workspace

Press "Sign in with Google": your browser opens, you pick the account and allow access.

**During the beta**, Google keeps IureDesk in *testing mode*: only accounts on its test-user list
(up to 100) can sign in. If yours isn't, Google shows "This app isn't verified" or a 403 error,
and IureDesk explains it. You have two options:

1. ask for your account to be added as a test user, or
2. use an **app password**: turn on 2-Step Verification in your Google account, create a password
   at <https://myaccount.google.com/apppasswords> and, in IureDesk, choose "Use an app password"
   and paste it. With an app password you still get mail, calendar and contacts.

In company accounts (Workspace), the administrator may block outside apps; IureDesk will tell you.

### Outlook.com, Hotmail and Microsoft 365

Press "Sign in with Microsoft". With work accounts:

- **your organization may need to approve the app**: IureDesk shows the approval link to send to
  your IT team (there is a guide for them in [`microsoft-365-ti.md`](../../microsoft-365-ti.md));
- **conditional access** (managed devices only) can't be bypassed; IureDesk explains it.

### iCloud

iCloud needs an **app-specific password**: create it at <https://account.apple.com> → Sign-In and
Security → App-Specific Passwords, and paste it into IureDesk.

### Other mail (IMAP/SMTP)

Your address and password are usually enough. If IureDesk can't find the settings, it asks for the
server details (your provider has them): IMAP or POP3 incoming server, SMTP outgoing server, ports
and encryption.

### Your company's Exchange (own server)

Not available in the beta yet. If your company uses Microsoft 365, use "Sign in with Microsoft".

## Mail

- **Read.** Select a message to see it on the right; double-click or `Enter` opens it in its own
  window. `Ctrl` + `+`/`-`/`0` changes the message text size.
- **Conversations.** Messages in the same thread are grouped (can be turned off in Settings →
  Mail).
- **Write.** "New" (`Ctrl+N` with Outlook shortcuts), "Reply", "Reply all" and "Forward" in the
  bar or with your scheme's shortcuts. Drop files on the window to attach them.
- **Send later and undo send.** Messages wait a few seconds in the outbox before leaving, so you
  can change your mind. You can also schedule them.
- **Search.** The field at the top searches all your accounts, offline too.
- **Organize.** Archive, move, flags, labels and categories, "Snooze", and rules ("When mail
  arrives from… → Move it to…") in Settings → Rules.
- **Secure mail.** Sign and encrypt with OpenPGP or S/MIME from the composer; manage keys from
  the palette: "Encryption keys…".
- **Print or save as PDF** from the palette: "Print message".

## Calendar

- Day, work week, week, month and agenda views (`1`–`5` or `Ctrl+Alt+1`–`5`, depending on the
  scheme). "Today" returns to the current day.
- **Create an event:** drag on the grid or press "New" while in the calendar. If you add guests,
  IureDesk tells you it will send them an invitation.
- **Video call:** on Google or Microsoft accounts the editor offers Google Meet or Teams. "Join"
  shows up in My day and in reminders.
- **Invitations** that arrive by mail are accepted, marked "Maybe" or declined from the message.
- On the left you choose which calendars to show; you can add a second time zone.

## Contacts, tasks and notes

- **Contacts:** double-click or `Enter` to edit; "+" for a new one. They sync with your account
  (Google, Microsoft, iCloud or CardDAV).
- **Tasks:** with due date, priority, reminder and lists; they sync with Google Tasks, Microsoft
  To Do or CalDAV.
- **Notes** are written in [IureEditor](https://github.com/ellaguno/iureditor). Without it the
  Notes module isn't shown. Each note is a Markdown file in the folder you choose (Settings →
  General → Notes folder).

## My day

The first thing you see: today's agenda, tasks due, important mail and reminders. You can move and
remove cards, change the background (Settings → Appearance) and see the weather. In Settings →
General you can have IureDesk open where you left off instead.

## Iurefficient and the ✦ assistant

- **Iurefficient** (optional): sign in to your instance in Settings → Integrations. Next to each
  message you see the person or client, their lead, opportunity, case or ticket, and you can link
  the message or create a lead or ticket.
- **✦ Assistant:** summarizes threads, drafts replies, pulls dates and tasks out of a message and
  answers questions about your day. It uses Iurefficient's AI or the provider you pick (OpenAI,
  Anthropic, Ollama…). **Before sending data to an outside provider, IureDesk shows you exactly
  what will leave your computer** and asks for permission. With no assistant configured, none of
  this is shown.
- **Sister apps:** IureEditor, IureTranscribe, IureVoz, IureOCR, IureDav and IureProject are
  offered only if they are installed.

## Importing from another program

Palette → "Import mail from Outlook, Thunderbird, Apple Mail…". It reads the data on your computer
(Thunderbird, Apple Mail, `.pst`, `.mbox` and `.eml` files, Google Takeout), shows progress and
can resume if interrupted. At the end you get a report of what was and wasn't imported.

## Useful settings

- **Appearance:** light, dark, high-contrast or system theme; three densities; typography and
  **text size**.
- **General:** Outlook, Gmail or Apple Mail shortcuts; language; open at sign-in; **updates**
  (check for new versions every day).
- **Privacy:** when to show remote images.
- **Advanced → Performance:** numbers for reporting slowness ("Copy report").

## Privacy

- Your mail is stored **only on your computer** and on your accounts' servers. IureDesk has no
  servers of its own and sends no usage statistics.
- Passwords and tokens are kept in the system keyring.
- **Remote images** in messages are blocked until you ask for them (so senders don't learn you
  opened the message); tracking pixels are never fetched.
- The update check only asks GitHub for the latest version number.

## FAQ

**Google says "This app isn't verified".** See [Gmail](#gmail-and-google-workspace): during the
beta, use an app password or ask for your account to be added as a test user.

**Windows warns the app is from an unknown publisher.** Click "More info" → "Run anyway". This goes
away once the Windows installer is signed.

**New mail isn't arriving.** Look at the sync dot at the top: if there's an error, its notice has
"Retry" and says what happened. Click the status in the rail to see each account.

**IureDesk closed by itself.** Next time you open it you'll see "IureDesk closed unexpectedly".
"Report on GitHub" opens a report you review before sending; the full report is on your clipboard
to paste.

**Where is my data?** In the system data folder (`~/.local/share/iuredesk` on Linux,
`%APPDATA%\iuredesk` on Windows, `~/Library/Application Support/iuredesk` on macOS).

**How do I update?** With iureapps, or by downloading the new installer. IureDesk tells you when a
new version is out (Settings → General → Updates).

**I found a bug or have an idea.** Open an issue at <https://github.com/ellaguno/iuredesk/issues>.
