# playterm

Windows용 Telnet/SSH 터미널. Rust(egui + alacritty_terminal + russh)로 작성.

## 구조

| 크레이트 | 역할 |
|---|---|
| `crates/playterm-term` | 터미널 에뮬레이션(alacritty_terminal), UTF-8/CP949 변환, 색상 팔레트 |
| `crates/playterm-net` | SSH(russh) / Telnet 전송, 세션 이벤트·명령 채널 |
| `crates/playterm-config` | 세션 목록(`sessions.toml`), 비밀번호 볼트(`vault.toml`) |
| `crates/playterm` | GUI 앱(eframe/egui): 렌더링, 키보드/IME 입력, 대화상자 |

## 빌드

```powershell
cargo build --release
```

- Rust 1.90+ 와 MSVC 빌드 도구(Visual Studio 2022)가 필요합니다.
- **Windows ARM64에서는 clang도 필요합니다** (암호 라이브러리 `ring` 빌드용):
  `winget install LLVM.LLVM` 후 `C:\Program Files\LLVM\bin` 을 PATH에 추가.

### 릴리스 패키지

```powershell
rustup target add x86_64-pc-windows-msvc     # 최초 1회
pwsh scripts\package.ps1                     # dist\playterm-<버전>-windows-{arm64,x64}.zip + SHA256SUMS.txt
```

- C 런타임을 정적으로 링크하므로(`.cargo/config.toml`) VC++ 재배포 패키지 없이 실행됩니다.
- 화면은 OpenGL(glow)로 그리고, OpenGL 을 쓸 수 없는 환경(GPU 없는 VM 등)에서는 자동으로 wgpu(DX12)로 전환합니다.
  환경변수 `PLAYTERM_RENDERER=wgpu` 또는 `glow` 로 강제할 수 있습니다.
- 아이콘은 `assets/make_icon.py` 로 생성합니다 (`playterm.png`, `playterm.ico`).

## 실행

```powershell
playterm                                  # 연결 대화상자
playterm ssh://user@host:22               # 바로 접속
playterm --cp949 telnet://192.168.0.1     # CP949(EUC-KR) 구형 장비
playterm -i ~/.ssh/id_ed25519 ssh://user@host   # 키 파일 지정
playterm -L 8080:db:5432 -D 1080 ssh://user@host   # 포트 포워딩 (OpenSSH 와 같은 형식, -R 도 지원)
playterm serial://COM3:115200             # 시리얼 콘솔 (8N1)
```

## 시리얼 · 로그

- 연결 창에서 프로토콜 **Serial** → COM 포트(목록/직접 입력), 속도, 데이터 비트, 패리티, 정지 비트, 흐름 제어.
- 탭 우클릭 → **BREAK 신호 보내기** (시리얼: 300ms line break, Telnet: `IAC BRK`) — 장비 ROMMON 진입·비밀번호 복구용.
- 세션 로그: 세션 편집 창의 "세션 로그" 에서 자동 기록을 켜거나, 탭 우클릭 → 로그 메뉴에서 수동으로 시작/중지.
  - 텍스트: 제어 문자·색상 코드를 뺀 출력 (UTF-8, 줄마다 시각 표시 선택)
  - 원본: 받은 바이트 그대로
  - 기본 폴더: `문서\playterm-logs`, 파일 이름: `세션이름_YYYYMMDD_HHMMSS.log`

## SFTP · 포트 포워딩

- SSH 탭에서 **Ctrl+Shift+F** (또는 탭 바의 [SFTP] / 탭 우클릭 메뉴) → 파일 브라우저.
  폴더 이동, 업로드(파일/폴더, 창에 끌어다 놓기), 다운로드(폴더는 통째로), 새 폴더, 이름 바꾸기, 삭제, 전송 진행률/취소.
- 포트 포워딩은 세션 편집 창의 "포트 포워딩" 에서 설정하며, 접속하면 자동으로 시작됩니다.
  - 로컬 (L): 이 PC 의 포트 → 서버에서 대상으로 연결
  - 원격 (R): 서버의 포트 → 이 PC 에서 대상으로 연결
  - 동적 (D): 이 PC 에 SOCKS5 프록시

## SSH 인증 순서

1. 세션에 지정한 키 파일 (OpenSSH 형식 / PuTTY `.ppk`, 암호가 걸려 있으면 passphrase 입력)
2. SSH 에이전트 — `SSH_AUTH_SOCK` 이 named pipe 이면 그것, 아니면 Windows OpenSSH 에이전트(`\\.\pipe\openssh-ssh-agent`), 그다음 Pageant
3. 기본 키 파일 `~/.ssh/id_ed25519`, `id_ecdsa`, `id_rsa` (있을 때만)
4. 저장된 비밀번호
5. keyboard-interactive / 비밀번호 입력

RSA 키는 서버가 알려주는 `rsa-sha2-512` / `rsa-sha2-256` 으로 서명합니다.

## 설정 파일

`%APPDATA%\playterm\config\` (환경변수 `PLAYTERM_CONFIG_DIR` 로 변경 가능 — 포터블 사용)

| 파일 | 내용 |
|---|---|
| `sessions.toml` | 저장된 세션 (비밀번호 없음) |
| `vault.toml` | 저장된 비밀번호. 마스터 비밀번호 → Argon2id → XChaCha20-Poly1305 로 암호화 |
| `known_hosts` | SSH 호스트 키. 알려진 호스트의 키가 바뀌면 접속을 거부합니다(중간자 공격 방지) |

## 단축키

| 키 | 동작 |
|---|---|
| Ctrl+Shift+T | 빠른 연결 |
| Ctrl+Shift+W / 휠 클릭 | 탭 닫기 |
| Ctrl+Shift+D | 탭 복제 |
| Ctrl+Tab / Ctrl+Shift+Tab | 다음 / 이전 탭 |
| Ctrl+Shift+B | 세션 목록 표시/숨김 |
| Ctrl+Shift+F | SFTP 파일 브라우저 |
| 드래그 / 더블클릭 / 트리플클릭 | 문자 / 단어 / 줄 선택 (Shift+클릭으로 확장) |
| Ctrl+C | 선택 영역이 있으면 복사, 없으면 ^C 전송 |
| Ctrl+Shift+C / Ctrl+Shift+V | 복사 / 붙여넣기 |
| Ctrl+V, Shift+Insert | 붙여넣기 |

## 개발

```powershell
cargo test --workspace                              # 단위 + SSH 루프백 통합 테스트
cargo run -p playterm-net --example dev_sshd        # 127.0.0.1:2222 테스트 SSH 서버 (tester / secret)
cargo run -p playterm-net --example dev_sshd -- --authorized-keys key.pub --sftp-root C:\temp   # 공개키 인증 / SFTP 루트
cargo run -p playterm -- ssh://tester@127.0.0.1:2222
```

## 로드맵

1. ~~워크스페이스 뼈대~~
2. ~~터미널 렌더링 + SSH/Telnet 접속~~
3. ~~세션 관리자(저장, 마스터 비밀번호 볼트) · 탭 개선 · 텍스트 선택/복사~~
4. ~~SSH 키 인증 · Pageant / Windows OpenSSH 에이전트~~
5. ~~SFTP · 포트 포워딩~~
6. ~~시리얼(COM) · 세션 로깅~~
