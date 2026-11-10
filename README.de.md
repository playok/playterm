# playterm

[한국어](README.md) | [English](README.en.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | **Deutsch** | [Français](README.fr.md) | [繁體中文](README.zh-TW.md) | [Русский](README.ru.md)

**SSH, Telnet, serielle Konsolen und Windows-Remotedesktop als Tabs in einem Fenster.**

playterm ist eine native Terminalanwendung in Rust (egui, alacritty_terminal, russh und IronRDP). Unter Windows x64 und ARM64 genügt eine portable ausführbare Datei; eine zusätzliche Laufzeitumgebung ist nicht erforderlich. Für macOS gibt es eine App für Apple silicon und Intel.

Diese deutsche Anleitung beschreibt den Entwicklungszweig `master`. Bereits veröffentlichte Funktionen stehen in den [Änderungen zu v0.7.0](docs/releases/v0.7.0.md) und den [Versionshinweisen](https://github.com/playok/playterm/releases). Die [ausführliche englische Referenz](README.en.md) enthält alle Einstellungen sowie die Entwicklungs- und Paketierungsverfahren.

## Funktionen

- SSH, Telnet, serielle Verbindungen und RDP in Tabs und geteilten Ansichten.
- Mehrere Sites mit Strg/Umschalt+Klick auswählen und gemeinsam verbinden; Sites und Anordnung als Vorlage speichern.
- SSH-Anmeldung mit Schlüsseln, Zertifikaten und externen Agenten; Sprunghosts, Portweiterleitungen `-L` / `-R` / `-D` und X11.
- SFTP-Seitenleiste, ZMODEM-Transfers (`sz` / `rz`), automatische Anmeldung, Makros, gleichzeitige Eingabe und Sitzungsprotokolle.
- Passworttresor mit Masterpasswort-Verschlüsselung (Argon2id + XChaCha20-Poly1305).
- Import von PuTTY-, Xshell- und SecureCRT-Sitzungen; Ordner und Tab-Farben.
- Oberfläche in zehn Sprachen, helle und dunkle Designs, Schriftwahl, Suche im Verlauf und internationale Zeichencodierungen.

## Bildschirmfotos

Die Bilder zeigen die koreanische Oberfläche. Deutsch lässt sich in den Einstellungen auswählen.

![SSH-Verbindung mit dunklem Design](docs/screenshots/main-dark.png)

| Verbindungen aus einer Vorlage nebeneinander | Startansicht eines nicht verbundenen Tabs |
| --- | --- |
| ![Zwei SSH-Sitzungen mit hellem Design](docs/screenshots/split-light.png) | ![Zuletzt verwendete Sites und Schnellverbindung](docs/screenshots/start-light.png) |

## Herunterladen und starten

Laden Sie unter [GitHub Releases](https://github.com/playok/playterm/releases) das Paket für Ihr Betriebssystem und Ihre CPU herunter.

- **Windows:** ZIP entpacken und `playterm.exe` starten. Das VC++ Redistributable muss nicht zusätzlich installiert werden. Ab der nächsten Version enthalten Standardpakete unkomprimierte Programme; UPX-Pakete sind optional. Das jeweilige Format ist in den Versionshinweisen beschrieben.
- **macOS ab Version 11:** DMG öffnen und `playterm.app` nach Applications ziehen; alternativ das ZIP verwenden. Informationen zu Signierung, Notarisierung und erstem Start stehen in den Versionshinweisen und der [englischen macOS-Anleitung](README.en.md#macos-app).
- Updates direkt in der Anwendung zu installieren ist nur unter Windows möglich. Unter macOS öffnet die Anwendung die Downloadseite.

Wählen Sie unter Extras → Einstellungen die Sprache **Deutsch**. Änderungen werden sofort angezeigt; Abbrechen stellt die vorherige Auswahl wieder her. Die gespeicherte Sprache bleibt nach einem Neustart erhalten. Oberflächensprache und Zeichencodierung einer Verbindung sind voneinander unabhängig.

## Verbinden

Speichern Sie Verbindungen in der Site-Verwaltung oder nutzen Sie die Schnellverbindung. Die Eingabeaufforderung `playterm>` eines nicht verbundenen Tabs akzeptiert `ssh user@host`, `telnet host`, `serial COM3 115200`, `rdp user@host` und `help`.

Verbindungen lassen sich auch über die Befehlszeile starten:

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

An der lokalen Eingabeaufforderung listet `sites` die gespeicherten Sites auf. `open Name` oder `open Nummer` verbindet eine Site. `ls` / `dir`, `cd` und `pwd` arbeiten mit lokalen Dateien. Nach dem Verbindungsende erscheint die Eingabeaufforderung erneut. [Befehlsreferenz auf Englisch](README.en.md#local-command-prompt).

## Tägliche Arbeit

- **Sites und geteilte Ansicht:** Ziehen Sie einen Tab an den Rand einer anderen Tab-Gruppe. Beim Löschen eines Site-Ordners wird dessen Inhalt in den übergeordneten Ordner verschoben; die Sites bleiben erhalten. Passwörter aus PuTTY, Xshell und SecureCRT werden nicht importiert.
- **Kopieren und Einfügen:** Strg+Umschalt+C / Strg+Umschalt+V. Strg+C kopiert eine Auswahl oder sendet sonst `^C`. Kopieren beim Markieren und Einfügen mit der rechten Maustaste lassen sich aktivieren. Mehrzeiliges Einfügen erfordert eine Bestätigung, wenn das entfernte Programm kein Bracketed Paste verwendet.
- **Suche und Darstellung:** Strg+Umschalt+F durchsucht auch den Verlauf. Strg+Mausrad und Strg+= / Strg+- / Strg+0 ändern die Schriftgröße. Die Menüs zeigen die Tastenkürzel des jeweiligen Betriebssystems.
- **Gleichzeitige Eingabe:** Strg+Umschalt+I sendet Tasten und eingefügten Text auch an die anderen ausgewählten Tabs. Achten Sie auf die orangefarbenen Markierungen; einzelne Tabs lassen sich im Kontextmenü ausschließen.
- **Befehlsleiste und Makros:** Strg+Umschalt+M blendet die Leiste ein; Enter sendet, Umschalt+Enter fügt eine Zeile ein. Makros unterstützen `@wait`, `@sleep`, `@if` und weitere Anweisungen. Verwenden Sie `{password}` statt eines ausgeschriebenen Passworts.
- **Automatische Anmeldung und Verbindungserhalt:** Hinterlegen Sie Schritte „Auf Eingabeaufforderung warten → Text senden“ mit `{user}` und `{password}`. Wählen Sie SSH/Telnet-Protokollnachrichten oder Texteingaben, um Eingabezeitlimits eines Geräts zu vermeiden.
- **Zeichencodierung:** Die automatische Erkennung unterscheidet nur UTF-8 und CP949. Shift_JIS, EUC-JP, GBK, Big5, Windows-Codepages und weitere Codierungen werden ausdrücklich ausgewählt. [Übersetzungs- und Codierungsstruktur](docs/i18n.md).

## Dateiübertragung und RDP

Die SFTP-Seitenleiste neben dem SSH-Terminal unterstützt Downloads, Uploads, neue Ordner, Umbenennen und Löschen. Auf Geräten ohne SFTP starten Sie serverseitig `sz file` zum Herunterladen oder `rz` zum Hochladen (lrzsz wird auf dem Server benötigt). ZMODEM funktioniert über SSH, Telnet und serielle Verbindungen; Strg+C bricht ab.

RDP zeigt den entfernten Desktop in einem Tab mit anpassbarer Auflösung, Ton und Unicode-Eingabe. Zwischenablage-, Ordner- und Druckerfreigabe sind **nur unter Windows** verfügbar. RD Gateway wird unterstützt, Mikrofoneingabe und Kerberos noch nicht. Prüfen Sie bei der ersten Verbindung den Zertifikatsfingerabdruck. Ändert sich das gespeicherte Zertifikat, wird die Verbindung abgelehnt. [RDP-Details auf Englisch](README.en.md#remote-desktop-rdp).

## Anmeldung und Konfigurationsdateien

Prüfen Sie bei der ersten SSH-Verbindung den Fingerabdruck des Hostschlüssels. Ein geänderter gespeicherter Schlüssel blockiert die Verbindung. Aktivieren Sie SSH-Agent-Weiterleitung (`-A`) und vertrauenswürdige X11-Weiterleitung (`-Y`) nur für vertrauenswürdige Server. Lokales Signieren mit privaten RSA-Schlüsseln ist eingeschränkt; siehe [Authentifizierung auf Englisch](README.en.md#ssh-authentication-order) und [RSA-Schutzmaßnahmen](docs/security-rsa-223.md).

Unter Windows liegt die Konfiguration in `%APPDATA%\playterm\config\`. Mit `PLAYTERM_CONFIG_DIR` können Sie für portablen Betrieb oder isolierte Tests ein anderes Verzeichnis angeben.

| Datei | Inhalt |
| --- | --- |
| `sessions.toml` | Sites und Vorlagen ohne Passwörter |
| `settings.toml` | Sprache, Darstellung, Tasten, Makros und Updates |
| `vault.toml` | Mit dem Masterpasswort verschlüsselte gespeicherte Passwörter |
| `known_hosts` | Vertrauenswürdige SSH-Hostschlüssel |
| `known_rdp_hosts` | Vertrauenswürdige RDP-Zertifikatsfingerabdrücke |

Ohne das Masterpasswort können gespeicherte Passwörter nicht wiederhergestellt werden. Makros werden im Klartext gespeichert: Tragen Sie dort keine Geheimnisse direkt ein.

## Bauen und entwickeln

Erforderlich ist **Rust ab Version 1.95**. Windows benötigt die MSVC-Buildtools von Visual Studio 2022, ARM64 zusätzlich LLVM/clang. macOS benötigt die Xcode command line tools. Linux-Abhängigkeiten sind im [Beitragsleitfaden](CONTRIBUTING.md) beschrieben.

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Unter Windows stehen `build.cmd` und `release.cmd` bereit; macOS-Pakete erstellt `scripts/package-macos.sh`. [Bauen und Signieren auf Englisch](README.en.md#building).

- [Kompatibilität](docs/compatibility.md) · [Validierungsergebnisse](docs/validation.md) · [Lasttests](docs/stress-testing.md)
- [Veröffentlichungsprozess](docs/release-process.md) · [Versionsänderungen](docs/releases/)
- [Übersetzungen hinzufügen](docs/i18n.md) · [Mitwirken](CONTRIBUTING.md)
- [Sicherheitslücke vertraulich melden](SECURITY.md)

Diese weiterführenden Dokumente sind überwiegend auf Englisch oder Koreanisch verfügbar.

## Lizenz

[MIT](LICENSE). Der verwendete Ausschnitt der Symbolschrift Material Symbols Rounded steht unter der [Apache License 2.0](assets/icons/LICENSE.txt). Die Lizenzen der Abhängigkeiten sind unter Hilfe → Open-Source-Lizenzen aufgeführt.
