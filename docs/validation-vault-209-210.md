# 볼트와 비밀번호 수명 검증 (#209, #210)

2026-11-06, Windows ARM64 개발 머신의 디버그 빌드로 검증했다. 모든 네이티브 창은 별도 `PLAYTERM_CONFIG_DIR`와 가짜 비밀번호만 사용했다.

## 실제 Windows 창

- 새 볼트 생성 후 잠금 해제 상태를 확인했다. 파일은 Argon2id m=65536 KiB, t=3, p=1이었다.
- 19 MiB/t=2의 가짜 기존 볼트를 실제 창에서 열었다. 잠금 해제에 성공했고 파일은 64 MiB/t=3으로 전환됐다. [전환 후 화면](screenshots/validation/vault-kdf-rehashed.png)
- 취소를 충분히 관찰하려고 유효 범위 안의 512 MiB/t=16 가짜 볼트를 사용했다. [진행 화면](screenshots/validation/vault-kdf-working.png)에서 프로세스 `Responding=true`를 확인했고 Esc로 취소했다. [취소 후 화면](screenshots/validation/vault-kdf-cancelled.png)은 잠긴 상태다. 계산이 끝날 때까지 기다린 뒤에도 볼트 SHA-256은 같았다. 취소는 결과 채택과 파일 쓰기를 막으며, 진행 중인 Argon2 호출 자체를 강제 종료하지 않는다. 그동안 추가 계산은 시작하지 않는다.
- 실제 로컬 Telnet 연결에서 `{password}` 매크로를 실행했다. 전송 전에 [대상과 위험 확인 창](screenshots/validation/password-macro-confirmation.png)이 나타났고 Esc로 닫혔다. 실제 비밀번호의 전송 전/취소 후 바이트 불변과 확인 후 전송은 아래 UI 회귀 시험에서 검증했다.

## 자동 검증

- `cargo test --locked --workspace`
- `cargo clippy --locked --workspace --all-targets -- -D warnings`
- 기존 볼트의 비밀·대상 보존, 잘못된 비밀번호의 파일 불변, 취소된 생성·재해시의 불변, 더 강한 설정 유지, KDF 상한과 AAD 변조 검증.
- 작업 취소/커밋 경합, 계산 중 추가 작업 차단, 복원·재접속의 비밀번호 타입, 복제·소유권 이전·Debug 가림, 매크로 치환과 자동 실행 차단.
- `password_macro_requires_confirmation_before_any_bytes_are_sent`: UI 메뉴로 매크로를 만들고 실행, 취소와 확인을 조작한다. 확인 전과 취소 후 전송량은 그대로이며 확인 후에만 가짜 비밀번호가 서버에 전송된다. 설정에는 실제 비밀번호를 쓰지 않는다.

새 기본 KDF 단독 측정은 이 머신에서 약 105 ms였다. 전용 벤치마크가 아니며 하드웨어에 따라 다르다. 앱 소유 비밀번호 사본은 지우지만, SSH/RDP 라이브러리가 요구하는 소유 문자열과 네트워크 송신 버퍼까지 모두 지운다는 보장은 하지 않는다.

가짜 네이티브 시험 볼트는 `PLAYTERM_VAULT_GUI_DIR`를 지정한 뒤 `cargo test -p playterm-config native_vault_fixture -- --ignored --nocapture`로 만들 수 있다. 느린 취소 시험에만 `PLAYTERM_VAULT_GUI_SLOW=1`을 추가한다. 테스트 마스터 비밀번호는 `test-master`다.
