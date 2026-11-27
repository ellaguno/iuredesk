# IureDesk

**A 100 % Rust desktop PIM** — mail, calendar, contacts, notes and tasks — built to replace
Outlook, Thunderbird or Apple Mail for anyone, with more information on screen than any of them,
and to be the desktop hub of the Iurefficient ecosystem. Linux first; macOS and Windows next.
[Versión en español](README.es.md).

| Mail (light, compact) | Mail (dark, dense) |
|---|---|
| ![](docs/screens/mail-light.png) | ![](docs/screens/mail-dark-dense.png) |
| **Calendar** | **Command palette (Ctrl+K)** |
| ![](docs/screens/calendar-light.png) | ![](docs/screens/palette-light.png) |

More screenshots (every module, light and dark) in [`docs/screens/`](docs/screens).

## What it is

- **Works without Iurefficient.** Any IMAP/SMTP account, Gmail, Microsoft 365 or iCloud: type
  your address and IureDesk finds the settings, then shows **one** button.
- **Dense and calm.** Three densities (the dense one fits more rows than Outlook), light, dark and
  high-contrast themes that follow your desktop live, no ribbon: a ≤ 6-button action bar plus a
  command palette (`Ctrl+K`) that reaches everything.
- **Keyboard first, with the keys you already know**: Outlook, Gmail or Apple Mail shortcuts.
- **Undo everything**: archive, delete, move, flags, tasks.
- **Offline-first**: everything reads from the local store (redb + tantivy); actions go to a local
  queue that sync replays.
- **Private**: HTML mail is rendered with Blitz (no webview, no JavaScript) and remote images
  stay blocked until you ask.
- **Pure Rust**, no C libraries, no webview; bilingual (English/Spanish).

## Try it

```sh
cargo run -p iuredesk -- --sample      # demo mailbox: 3 accounts, ~1 900 messages, calendar, contacts…
cargo run -p iuredesk                  # your accounts (first run shows the add-account assistant)
cargo run -p iuredesk -- mailto:ana@example.com?subject=Hello
```

Headless screenshots: `cargo run -p iuredesk-ui --example ui_shot -- /tmp/shot.png --module calendar --dark`.
Quality gate: `cargo xtask ci`. The plan is in [`PLAN.md`](PLAN.md) and the work list in
[`docs/gaps.md`](docs/gaps.md).

## Microsoft Store (MSIX)

The Windows job of `.github/workflows/release.yml` also builds the MSIX package for the Microsoft
Store (artifacts `msix-store`, unsigned, for Partner Center, and `msix-prueba`, signed with a test
certificate; neither goes to the release). It contains the same files as the `.exe`/`.msi`
installers, declares `runFullTrust` and `internetClient`, the `iuredesk:` and `mailto:` protocols,
`.eml`/`.ics`/`.vcf`, the `iuredesk.exe` execution alias (registered in Claude Desktop and VS Code)
and the `IureDeskInicio` StartupTask. Installed from the Store, IureDesk never updates itself nor
checks GitHub for new versions, and writes nothing to the registry. Package version: the `.msi`
`ProductVersion` with the first number plus one and a trailing `.0` (`0.1.0-beta.12` →
`1.1.312.0`, `0.1.0` → `1.1.999.0`). Details (in Spanish): [README.es.md](README.es.md#microsoft-store-msix).
Privacy policy: [`PRIVACY.md`](PRIVACY.md).

## License

[Apache-2.0](LICENSE.md),
© Eduardo Llaguno Velasco.
