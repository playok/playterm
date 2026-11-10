# playterm

[한국어](README.md) | [English](README.en.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | [Español](README.es.md) | [Português (Brasil)](README.pt-BR.md) | [Deutsch](README.de.md) | **Français** | [繁體中文](README.zh-TW.md) | [Русский](README.ru.md)

**SSH, Telnet, consoles série et Bureau à distance Windows dans les onglets d’une seule fenêtre.**

playterm est une application de terminal native écrite en Rust (egui, alacritty_terminal, russh et IronRDP). Sous Windows x64 et ARM64, elle se présente sous la forme d’un exécutable portable, sans environnement d’exécution à installer. Une application macOS est également disponible pour Apple silicon et Intel.

Ce guide français décrit la branche de développement `master`. Pour les fonctions déjà publiées, consultez les [changements de la v0.8.0](docs/releases/v0.8.0.md) et les [notes de version](https://github.com/playok/playterm/releases). La [référence détaillée en anglais](README.en.md) couvre toutes les options ainsi que le développement et la distribution.

## Fonctionnalités

- SSH, Telnet, série et RDP dans des onglets et des vues divisées.
- Sélection de plusieurs sites avec Ctrl/Maj+clic, connexion groupée et modèles enregistrant les sites et leur disposition.
- Authentification SSH par clés, certificats et agents externes ; hôtes de rebond, redirections `-L` / `-R` / `-D` et X11.
- Panneau SFTP, transferts ZMODEM (`sz` / `rz`), connexion automatique, macros, saisie simultanée et journaux de session.
- Coffre de mots de passe chiffré avec un mot de passe principal (Argon2id + XChaCha20-Poly1305).
- Importation des sessions PuTTY, Xshell et SecureCRT ; dossiers et couleurs d’onglets.
- Interface en dix langues, thèmes clair et sombre, choix des polices, recherche dans l’historique et encodages internationaux.

## Captures d’écran

Les images montrent l’interface coréenne. Le français se sélectionne dans les paramètres.

![Connexion SSH avec le thème sombre](docs/screenshots/main-dark.png)

| Connexions côte à côte depuis un modèle | Accueil d’un onglet non connecté |
| --- | --- |
| ![Deux sessions SSH avec le thème clair](docs/screenshots/split-light.png) | ![Sites récents et connexion rapide](docs/screenshots/start-light.png) |

## Téléchargement et lancement

Téléchargez le paquet correspondant à votre système et à votre processeur depuis [GitHub Releases](https://github.com/playok/playterm/releases).

- **Windows :** décompressez le ZIP et lancez `playterm.exe`. Le redistribuable VC++ n’est pas nécessaire. À partir de la prochaine version, les paquets standard contiendront des exécutables non compressés, avec des paquets UPX facultatifs ; vérifiez le format dans les notes de chaque version.
- **macOS 11 ou ultérieur :** ouvrez le DMG et glissez `playterm.app` dans Applications ; le ZIP est aussi disponible. Pour la signature, la notarisation et le premier lancement, consultez les notes de version et le [guide macOS en anglais](README.en.md#macos-app).
- L’installation des mises à jour depuis l’application fonctionne uniquement sous Windows. Sous macOS, l’application ouvre la page de téléchargement.

Sélectionnez **Français** dans le champ de langue d’Outils → Paramètres. Les changements sont prévisualisés immédiatement ; Annuler rétablit la sélection précédente. La langue enregistrée est conservée au redémarrage. La langue de l’interface et l’encodage de la connexion sont indépendants.

## Se connecter

Enregistrez une connexion dans le gestionnaire de sites ou utilisez la connexion rapide. L’invite `playterm>` d’un onglet non connecté accepte `ssh user@host`, `telnet host`, `serial COM3 115200`, `rdp user@host` et `help`.

Vous pouvez également vous connecter depuis la ligne de commande :

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

À l’invite locale, `sites` affiche les sites enregistrés et `open nom` ou `open numéro` ouvre une connexion. `ls` / `dir`, `cd` et `pwd` travaillent sur les fichiers locaux. L’invite revient lorsque la connexion se termine. [Liste des commandes en anglais](README.en.md#local-command-prompt).

## Utilisation courante

- **Sites et vues divisées :** glissez un onglet vers le bord d’un autre groupe pour diviser la vue. Supprimer un dossier de sites déplace son contenu dans le dossier parent sans supprimer les sites. Les mots de passe PuTTY, Xshell et SecureCRT ne sont pas importés.
- **Copier et coller :** Ctrl+Maj+C / Ctrl+Maj+V. Ctrl+C copie la sélection ou envoie `^C` s’il n’y en a pas. La copie à la sélection et le collage par clic droit peuvent être activés. Le collage de plusieurs lignes demande confirmation si le programme distant n’utilise pas le mode bracketed paste.
- **Recherche et affichage :** Ctrl+Maj+F recherche aussi dans l’historique. Ctrl+molette et Ctrl+= / Ctrl+- / Ctrl+0 règlent la taille des caractères. Les menus indiquent les raccourcis propres au système.
- **Saisie simultanée :** Ctrl+Maj+I transmet aussi les touches et le texte collé aux autres onglets destinataires. Vérifiez les repères orange ; un onglet peut être exclu depuis son menu contextuel.
- **Barre de commandes et macros :** Ctrl+Maj+M affiche la barre ; Entrée envoie et Maj+Entrée ajoute une ligne. Les macros acceptent notamment `@wait`, `@sleep` et `@if`. Utilisez `{password}` au lieu d’écrire un mot de passe.
- **Connexion automatique et maintien de connexion :** configurez des étapes « attendre une invite → envoyer du texte », avec `{user}` et `{password}`. Choisissez les messages du protocole SSH/Telnet ou l’envoi de texte pour éviter le délai d’inactivité de saisie des équipements.
- **Encodage :** la détection automatique distingue uniquement UTF-8 et CP949. Sélectionnez explicitement Shift_JIS, EUC-JP, GBK, Big5, les pages de codes Windows ou les autres encodages. [Structure des traductions et des encodages](docs/i18n.md).

## Transferts de fichiers et RDP

Le panneau SFTP à côté du terminal SSH permet de télécharger, téléverser, créer des dossiers, renommer et supprimer. Sur les équipements sans SFTP, lancez `sz file` sur le serveur pour télécharger ou `rz` pour téléverser (lrzsz doit être installé sur le serveur). ZMODEM fonctionne sur SSH, Telnet et série ; Ctrl+C annule.

RDP affiche le bureau distant dans un onglet avec résolution adaptable, son et saisie Unicode. Le partage du presse-papiers, des dossiers et des imprimantes est disponible **uniquement sous Windows**. RD Gateway est pris en charge, mais pas encore le microphone ni Kerberos. Vérifiez l’empreinte du certificat à la première connexion : si le certificat enregistré change, la connexion est refusée. [Détails RDP en anglais](README.en.md#remote-desktop-rdp).

## Authentification et configuration

Vérifiez l’empreinte de la clé d’hôte SSH à la première connexion. Une modification de la clé enregistrée bloque la connexion. N’activez le transfert d’agent SSH (`-A`) et le transfert X11 de confiance (`-Y`) que pour des serveurs fiables. La signature locale avec des clés privées RSA est restreinte ; consultez l’[authentification en anglais](README.en.md#ssh-authentication-order) et les [mesures d’atténuation RSA](docs/security-rsa-223.md).

Sous Windows, la configuration se trouve dans `%APPDATA%\playterm\config\`. La variable `PLAYTERM_CONFIG_DIR` permet de choisir un autre dossier pour un usage portable ou des tests isolés.

| Fichier | Contenu |
| --- | --- |
| `sessions.toml` | Sites et modèles, sans mots de passe |
| `settings.toml` | Langue, affichage, touches, macros et mises à jour |
| `vault.toml` | Mots de passe enregistrés, chiffrés avec le mot de passe principal |
| `known_hosts` | Clés d’hôte SSH approuvées |
| `known_rdp_hosts` | Empreintes de certificats RDP approuvés |

Si vous oubliez le mot de passe principal, les mots de passe enregistrés ne peuvent pas être récupérés. Les macros sont stockées en clair : n’y écrivez pas directement de secrets.

## Compilation et développement

**Rust 1.95 ou ultérieur** est requis. Windows nécessite les outils MSVC de Visual Studio 2022, ainsi que LLVM/clang pour ARM64. macOS nécessite les Xcode command line tools. Les dépendances Linux sont décrites dans le [guide de contribution](CONTRIBUTING.md).

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Windows dispose de `build.cmd` et `release.cmd` ; macOS utilise `scripts/package-macos.sh` pour créer les paquets. [Compilation et signature en anglais](README.en.md#building).

- [Compatibilité](docs/compatibility.md) · [Résultats de validation](docs/validation.md) · [Tests de charge](docs/stress-testing.md)
- [Processus de publication](docs/release-process.md) · [Changements par version](docs/releases/)
- [Ajouter des traductions](docs/i18n.md) · [Contribuer](CONTRIBUTING.md)
- [Signaler une vulnérabilité en privé](SECURITY.md)

Ces documents de référence sont principalement disponibles en anglais ou en coréen.

## Licence

[MIT](LICENSE). Le sous-ensemble de Material Symbols Rounded utilisé pour les icônes des boutons relève de la [licence Apache 2.0](assets/icons/LICENSE.txt). Les licences des dépendances sont affichées dans Aide → Licences open source.
