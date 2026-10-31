# playterm

Windows용 Telnet/SSH 터미널. Rust(egui + alacritty_terminal + russh)로 작성.

## 구조

| 크레이트 | 역할 |
|---|---|
| `crates/playterm-term` | 터미널 에뮬레이션(alacritty_terminal), UTF-8/CP949 변환, 색상 팔레트 |
| `crates/playterm-net` | SSH(russh) / Telnet 전송, 세션 이벤트·명령 채널 |
| `crates/playterm` | GUI 앱(eframe/egui): 렌더링, 키보드/IME 입력, 대화상자 |

## 빌드

```powershell
cargo build --release
```

- Rust 1.90+ 와 MSVC 빌드 도구(Visual Studio 2022)가 필요합니다.
- **Windows ARM64에서는 clang도 필요합니다** (암호 라이브러리 `ring` 빌드용):
  `winget install LLVM.LLVM` 후 `C:\Program Files\LLVM\bin` 을 PATH에 추가.

## 실행

```powershell
playterm                                  # 연결 대화상자
playterm ssh://user@host:22               # 바로 접속
playterm --cp949 telnet://192.168.0.1     # CP949(EUC-KR) 구형 장비
```

SSH 호스트 키는 `%APPDATA%\playterm\config\known_hosts` 에 저장됩니다.
이미 알려진 호스트의 키가 바뀌면 접속을 거부합니다(중간자 공격 방지).

## 개발

```powershell
cargo test --workspace                              # 단위 + SSH 루프백 통합 테스트
cargo run -p playterm-net --example dev_sshd        # 127.0.0.1:2222 테스트 SSH 서버 (tester / secret)
cargo run -p playterm -- ssh://tester@127.0.0.1:2222
```

## 로드맵

1. ~~워크스페이스 뼈대~~
2. ~~터미널 렌더링 + SSH/Telnet 접속~~
3. 세션 관리자(저장, 마스터 비밀번호 볼트) · 탭 개선 · 텍스트 선택/복사
4. SSH 키 인증 · Pageant / Windows OpenSSH 에이전트
5. SFTP · 포트 포워딩
6. 시리얼(COM) · 세션 로깅
