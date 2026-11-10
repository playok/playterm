# playterm

[한국어](README.md) | [English](README.en.md) | [日本語](README.ja.md) | [简体中文](README.zh-CN.md) | **Español** | [Português (Brasil)](README.pt-BR.md) | [Deutsch](README.de.md) | [Français](README.fr.md) | [繁體中文](README.zh-TW.md) | [Русский](README.ru.md)

**SSH, Telnet, consolas serie y Escritorio remoto de Windows, en pestañas de una sola ventana.**

playterm es una aplicación de terminal nativa escrita en Rust (egui, alacritty_terminal, russh e IronRDP). En Windows x64 y ARM64 se distribuye como un ejecutable portátil, sin instalar un entorno de ejecución. También ofrece una aplicación para macOS en Apple silicon e Intel.

Esta guía en español describe la rama de desarrollo `master`. Para las funciones publicadas, consulta los [cambios de v0.7.0](docs/releases/v0.7.0.md) y las [notas de las versiones](https://github.com/playok/playterm/releases). La [referencia detallada en inglés](README.en.md) incluye todas las opciones y los procedimientos de desarrollo y distribución.

## Funciones principales

- SSH, Telnet, serie y RDP en pestañas y paneles divididos.
- Selección de varios sitios con Ctrl/Mayús+clic; conexión conjunta y plantillas con sitios y disposición.
- Autenticación SSH con claves, certificados y agentes externos; hosts de salto, reenvío `-L` / `-R` / `-D` y X11.
- Panel SFTP, transferencias ZMODEM (`sz` / `rz`), inicio de sesión automático, macros, entrada simultánea y registros de sesión.
- Bóveda de contraseñas cifrada con una contraseña maestra (Argon2id + XChaCha20-Poly1305).
- Importación de sesiones de PuTTY, Xshell y SecureCRT; carpetas y colores de pestañas.
- Interfaz en diez idiomas, temas oscuro y claro, fuentes configurables, búsqueda en el historial y codificaciones internacionales.

## Capturas

Las imágenes muestran la interfaz en coreano. Puedes seleccionar español en los ajustes.

![Conexión SSH con tema oscuro](docs/screenshots/main-dark.png)

| Conexiones abiertas en paralelo desde una plantilla | Inicio de una pestaña sin conexión |
| --- | --- |
| ![Dos sesiones SSH con tema claro](docs/screenshots/split-light.png) | ![Sitios recientes y conexión rápida](docs/screenshots/start-light.png) |

## Descargar y ejecutar

Descarga el paquete adecuado para tu sistema operativo y CPU desde [GitHub Releases](https://github.com/playok/playterm/releases).

- **Windows:** descomprime el ZIP y ejecuta `playterm.exe`. No necesitas instalar el redistribuible de VC++. A partir de la próxima versión, los paquetes estándar llevarán ejecutables sin comprimir y habrá paquetes UPX opcionales; comprueba el formato en las notas de cada versión.
- **macOS 11 o posterior:** abre el DMG y arrastra `playterm.app` a Applications; también puedes usar el ZIP. Consulta las notas de la versión y la [guía de macOS en inglés](README.en.md#macos-app) para la firma, la notarización y el primer inicio.
- La instalación de actualizaciones desde la aplicación solo está disponible en Windows. En macOS se abre la página de descarga.

Selecciona **Español** en el idioma de Herramientas → Configuración. Los cambios se previsualizan al instante; Cancelar restaura la selección anterior. El idioma guardado se conserva al reiniciar. El idioma de la interfaz y la codificación de la conexión son ajustes independientes.

## Conectarse

Guarda conexiones en el gestor de sitios o usa la conexión rápida. En el indicador `playterm>` de una pestaña sin conexión puedes escribir `ssh user@host`, `telnet host`, `serial COM3 115200`, `rdp user@host` o `help`.

También puedes conectar desde la línea de comandos:

```sh
playterm ssh://user@host:22
playterm -i ~/.ssh/id_ed25519 ssh://user@host
playterm -L 8080:db:5432 -D 1080 ssh://user@host
playterm --cp949 telnet://192.168.0.1
playterm serial://COM3:115200
playterm rdp://user@host
```

En el indicador local, `sites` lista los sitios guardados y `open nombre` o `open número` conecta a uno de ellos. `ls` / `dir`, `cd` y `pwd` trabajan con archivos locales. Al finalizar la conexión reaparece el indicador. Consulta la [lista de comandos en inglés](README.en.md#local-command-prompt).

## Uso diario

- **Sitios y paneles:** arrastra una pestaña al borde de otro grupo para dividir la vista. Eliminar una carpeta de sitios mueve su contenido a la carpeta superior; no elimina los sitios. Las contraseñas de PuTTY, Xshell y SecureCRT no se importan.
- **Copiar y pegar:** Ctrl+Mayús+C / Ctrl+Mayús+V. Ctrl+C copia si hay una selección; de lo contrario envía `^C`. Puedes activar la copia al seleccionar y el pegado con el botón derecho. El pegado de varias líneas pide confirmación cuando el programa remoto no usa bracketed paste.
- **Buscar y mostrar:** Ctrl+Mayús+F busca también en el historial. Ctrl+rueda y Ctrl+= / Ctrl+- / Ctrl+0 cambian el tamaño de letra. Los menús muestran los atajos correspondientes al sistema operativo.
- **Entrada simultánea:** Ctrl+Mayús+I envía las teclas y el texto pegado también a las otras pestañas destinatarias. Comprueba las marcas naranjas; puedes excluir una pestaña desde su menú contextual.
- **Barra de comandos y macros:** Ctrl+Mayús+M muestra la barra; Enter envía y Mayús+Enter inserta una línea. Las macros admiten `@wait`, `@sleep`, `@if` y otras instrucciones. Usa `{password}` en lugar de escribir una contraseña.
- **Inicio automático y mantenimiento de conexión:** configura pasos «esperar indicador → enviar texto» con `{user}` y `{password}`. Elige mantenimiento mediante el protocolo SSH/Telnet o envío de texto para evitar el tiempo de espera de entrada del dispositivo.
- **Codificación:** la detección automática solo distingue UTF-8 y CP949. Selecciona explícitamente Shift_JIS, EUC-JP, GBK, Big5, páginas de códigos de Windows u otras codificaciones. Consulta la [estructura de traducciones y codificaciones](docs/i18n.md).

## Transferencias y RDP

El panel SFTP junto al terminal SSH permite descargar, subir, crear carpetas, renombrar y eliminar. En dispositivos sin SFTP, ejecuta `sz file` en el servidor para descargar o `rz` para subir (requiere lrzsz en el servidor). ZMODEM funciona con SSH, Telnet y serie; Ctrl+C cancela.

RDP muestra el escritorio remoto dentro de una pestaña, con resolución adaptable, sonido y entrada Unicode. Compartir portapapeles, carpetas e impresoras está disponible **solo en Windows**. Se admite RD Gateway; todavía no se admiten micrófono ni Kerberos. Verifica la huella del certificado en la primera conexión: si cambia después de guardarlo, se rechaza la conexión. [Detalles de RDP en inglés](README.en.md#remote-desktop-rdp).

## Autenticación y configuración

Verifica la huella de la clave de host SSH al conectar por primera vez. Un cambio de clave guardada bloquea la conexión. Activa el reenvío del agente SSH (`-A`) y X11 de confianza (`-Y`) únicamente para servidores fiables. La firma local con claves privadas RSA está restringida; consulta la [autenticación en inglés](README.en.md#ssh-authentication-order) y el [registro de mitigación RSA](docs/security-rsa-223.md).

En Windows, la configuración está en `%APPDATA%\playterm\config\`. La variable `PLAYTERM_CONFIG_DIR` permite cambiarla para un uso portátil o pruebas aisladas.

| Archivo | Contenido |
| --- | --- |
| `sessions.toml` | Sitios y plantillas, sin contraseñas |
| `settings.toml` | Idioma, aspecto, teclas, macros y actualizaciones |
| `vault.toml` | Contraseñas guardadas, cifradas con la contraseña maestra |
| `known_hosts` | Claves de host SSH de confianza |
| `known_rdp_hosts` | Huellas de certificados RDP de confianza |

Si olvidas la contraseña maestra, no podrás recuperar las contraseñas guardadas. Las macros se almacenan en texto sin cifrar: no escribas secretos directamente en ellas.

## Compilación y desarrollo

Se necesita **Rust 1.95 o posterior**. En Windows, instala las herramientas MSVC de Visual Studio 2022; ARM64 requiere además LLVM/clang. En macOS se necesitan las Xcode command line tools. Las dependencias de Linux están en la [guía de contribución](CONTRIBUTING.md).

```sh
cargo build --release --locked
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

Windows dispone de `build.cmd` y `release.cmd`; macOS usa `scripts/package-macos.sh` para crear paquetes. [Compilación y firma en inglés](README.en.md#building).

- [Compatibilidad](docs/compatibility.md) · [Resultados de validación](docs/validation.md) · [Pruebas de carga](docs/stress-testing.md)
- [Proceso de publicación](docs/release-process.md) · [Cambios por versión](docs/releases/)
- [Añadir traducciones](docs/i18n.md) · [Contribuir](CONTRIBUTING.md)
- [Informar de una vulnerabilidad en privado](SECURITY.md)

Estos documentos de referencia se ofrecen principalmente en inglés o coreano.

## Licencia

[MIT](LICENSE). El subconjunto de Material Symbols Rounded usado para los iconos de botones se distribuye bajo [Apache License 2.0](assets/icons/LICENSE.txt). Las licencias de dependencias aparecen en Ayuda → Licencias de código abierto.
