# Genera las libretas SQLite sintéticas de las pruebas (`abook.sqlite`, `wal.sqlite`, `wal.sqlite-wal`).
#
# Herramienta de desarrollo (no la usa el producto ni las pruebas): los archivos generados se
# versionan. Todos los datos son inventados. Uso: `python3 make_sqlite.py` desde este directorio.
import os
import shutil
import sqlite3

SCHEMA = [
    "CREATE TABLE properties (card TEXT, name TEXT, value TEXT)",
    "CREATE TABLE lists (uid TEXT PRIMARY KEY, name TEXT, nickName TEXT, description TEXT)",
    "CREATE TABLE list_cards (list TEXT, card TEXT, PRIMARY KEY(list, card))",
]


def fresh(path):
    for suffix in ("", "-wal", "-shm", "-journal"):
        if os.path.exists(path + suffix):
            os.remove(path + suffix)


def abook():
    path = "abook.sqlite"
    fresh(path)
    db = sqlite3.connect(path)
    db.execute("PRAGMA page_size=512")
    for s in SCHEMA:
        db.execute(s)
    rows = []
    for i in range(120):
        card = "card-%03d" % i
        rows += [
            (card, "DisplayName", "Persona %d Ejemplo" % i),
            (card, "FirstName", "Persona %d" % i),
            (card, "LastName", "Ejemplo"),
            (card, "PrimaryEmail", "persona%d@example.com" % i),
            (card, "WorkPhone", "+52 55 0000 %04d" % i),
        ]
    # Formato moderno (Thunderbird 102+): la tarjeta entera en `_vCard`.
    vcard = (
        "BEGIN:VCARD\r\nVERSION:4.0\r\nUID:card-vcard\r\nFN:Ana Sintética\r\n"
        "N:Sintética;Ana;;;\r\nEMAIL;TYPE=work:ana@example.org\r\nTEL;TYPE=cell:+34 600 000 000\r\n"
        "ORG:Ejemplo S.A.\r\nEND:VCARD\r\n"
    )
    rows.append(("card-vcard", "_vCard", vcard))
    rows.append(("card-vcard", "DisplayName", "Ana Sintética"))
    rows.append(("card-000", "Notes", "Nota larga " + "x" * 2990))
    db.executemany("INSERT INTO properties VALUES (?,?,?)", rows)
    db.execute("INSERT INTO lists VALUES ('list-1', 'Equipo', '', '')")
    db.execute("INSERT INTO list_cards VALUES ('list-1', 'card-001')")
    db.commit()
    db.close()


def wal():
    path = "wal.sqlite"
    fresh(path)
    db = sqlite3.connect(path)
    db.execute("PRAGMA page_size=1024")
    db.execute("PRAGMA journal_mode=WAL")
    for s in SCHEMA:
        db.execute(s)
    db.execute("INSERT INTO properties VALUES ('base-card', 'DisplayName', 'Base')")
    db.commit()
    db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
    db.execute("PRAGMA wal_autocheckpoint=0")
    db.execute("INSERT INTO properties VALUES ('wal-card', 'DisplayName', 'Solo en WAL')")
    db.execute("INSERT INTO properties VALUES ('wal-card', 'PrimaryEmail', 'wal@example.com')")
    db.commit()
    # Copia mientras la conexión sigue abierta (al cerrar, SQLite haría checkpoint y borraría el WAL).
    shutil.copy(path, path + ".tmp")
    shutil.copy(path + "-wal", path + "-wal.tmp")
    db.close()
    fresh(path)
    os.rename(path + ".tmp", path)
    os.rename(path + "-wal.tmp", path + "-wal")


if __name__ == "__main__":
    abook()
    wal()
