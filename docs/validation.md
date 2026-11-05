# 이슈 189부터 196까지 검증 기록

2026-11-05, Windows build 26300 ARM64에서 번호 순서로 조사하고 수정했다. 개발 기준은 master `25a9bcb40b55ceda091efdc7cdb291268d4cb415`에 이 작업의 변경을 더한 트리다. 앱 버전 문자열은 아직 0.5.0이며, 이번 개발 패키지는 공식 v0.5.0 배포 파일과 다르다. 검증 실행자는 이 작업의 Codex 로컬 세션이다.

공통 검사: `cargo fmt --all -- --check`, `cargo clippy --locked --workspace --all-targets -- -D warnings`, `cargo test --locked --workspace` 통과. 전체 테스트 로그는 로컬 `target/workspace-tests-full.log`에 있다. 첫 샌드박스 실행에서 기존 홈 경로 테스트 2개가 실패했으며, 사용자 환경에서 다시 실행한 전체 테스트는 통과했다. `python scripts/test-release-notes.py`의 태그 범위·초안 차단·패키지 안내 보존 테스트 3개도 통과했다.

## 이슈별 결과

| 이슈 | 구현과 확인한 결과 | 남은 완료 조건 |
|---|---|---|
| [189](https://github.com/playok/playterm/issues/189) | 키 암호 취소를 전체 연결 종료로 전파. 별도 ‘이 키 건너뛰기’만 후속 인증 허용. 잘못된 암호 3회 뒤도 종료. 루프백에서 에이전트·기본 키·저장 비밀번호·점프호스트가 취소 뒤 사용되지 않는지 확인. 실제 창에서 취소·Esc·재시도·키 로그인·건너뛰기 후 비밀번호 로그인·재접속 버튼·`r` 명령을 검증 | 이번 환경은 Windows ARM64. 다른 플랫폼의 GUI 확인은 호환성 표에서 별도 관리 |
| [190](https://github.com/playok/playterm/issues/190) | 공개 키 파일과 Windows 에이전트의 partial success 보존. 저장 비밀번호를 OTP로 보내지 않음, OTP는 account_password=false. 공개 키→OTP 오답·재시도·취소·새 연결과 기존 비밀번호→OTP 회귀 통과. 실제 창에서 OTP 재시도·취소·재접속·로그인 확인. 모호한 hidden 질문은 자동 채움/저장 대상에서 제외 | 실제 OpenSSH MFA와 macOS 에이전트 실행 검증은 남음 |
| [191](https://github.com/playok/playterm/issues/191) | `vault.toml.lock` OS 잠금으로 create/reset/read-modify-write 직렬화. 대기 750 ms 제한, 실패 시 메모리 변경 미반영, reset 뒤 이전 창의 자격 증명 재생성 방지. 실제 별도 Windows 프로세스로 추가/추가·추가/삭제·초기화·동시 생성·비정상 종료 후 잠금 해제 통과. 실제 UI에서 잠금 오류 표시→잠금 해제 후 볼트 생성 성공 | macOS의 별도 프로세스 실행 필요. 기존 Windows/macOS CI가 같은 테스트를 실행하도록 포함됨 |
| [192](https://github.com/playok/playterm/issues/192) | 공식 v0.5.0 Windows ZIP 두 개를 다운로드해 체크섬·Authenticode·Ed25519 검사. 최종 ZIP 추출 후 서명 검증 단계를 배포 스크립트에 추가. 서명 필수 모드에서 unsigned ZIP 거부 확인 | 정식 Windows 인증서와 Apple Developer ID/공증 시크릿 없음. macOS 앱·깨끗한 OS 첫 실행 검증 필요. 미완료 |
| [193](https://github.com/playok/playterm/issues/193) | 같은 소스의 x64·ARM64 빌드에서 미압축 기본 ZIP과 `-upx.zip` 네 개 생성·체크섬 검사. 기존 업데이트 이름 유지, UPX 사용자도 다음 업데이트에서 기본 미압축판 선택. 최종 ZIP의 실행 파일을 실제 Windows ARM64에서 실행해 SSH 연결·종료 확인 | 공식 다운로드 제공은 다음 태그 릴리스에서 적용. x64 시험은 ARM64 에뮬레이션이며 네이티브 x64 실행은 별도 필요 |
| [194](https://github.com/playok/playterm/issues/194) | [호환성 표](compatibility.md)에 필수 조합·절차·결과 필드·승인/보류 기준·미지원/미검증 구분 추가. 실제 OpenSSH 키 로그인·셸·한글 경로 SFTP용 하네스와 Ubuntu CI 단계 추가. 하네스 Rust 코드는 로컬 컴파일 확인 | 현재 장비에 sshd·테스트 네트워크 장비·RDP 대상·macOS가 없음. 실제 OpenSSH CI와 실장비 결과 필요. 미완료 |
| [195](https://github.com/playok/playterm/issues/195) | [부하 시험 절차](stress-testing.md), 가짜 출력 서버·자원 샘플러 추가. 실제 1/10/30탭 출력·입력·종료 예비 시험 수행. 시작 시 여러 URL이 좁은 분할 화면으로 열리는 결함을 발견해 첫 그룹을 재사용하도록 수정, 30세션 회귀 테스트와 실제 30탭 창으로 확인 | 8~24시간, 100회 반복 후 휴지 기준선, UI p95 지연, 실서버 SFTP 장애·RDP·절전 복귀는 미검증. 미완료 |
| [196](https://github.com/playok/playterm/issues/196) | [다음 릴리스 초안](releases/next.md), [릴리스 절차](release-process.md), 커밋 기반 초안 생성기 추가. 게시 설명은 태그에 들어 있는 검토 문서만 사용하며 실제 서명·패키지·체크섬 안내 보존. README 개발/배포 버전 구분. 실제 업데이트 UI에서 한글·링크 정리·스크롤·나중에 닫기 확인 | 다음 버전의 `docs/releases/vX.Y.Z.md`를 검토·태그에 포함하고 실제 게시 결과 확인 필요 |

## 공식 v0.5.0 검사

공식 태그 커밋은 `3943e8073a091a0d6aad5a79b5f83541e3f41ba7`이다. 개발 트리의 새 기능을 이 버전의 출시 기능으로 표시하지 않는다.

| 공식 Windows 파일 | SHA-256 | Authenticode |
|---|---|---|
| playterm-0.5.0-windows-arm64.zip | `0b7c00a701dcf95bbbd59d1a0ec599058270f25b7c0c12b4c681840cb8cd1e80` | NotSigned, 게시자·타임스탬프 없음 |
| playterm-0.5.0-windows-x64.zip | `af6b442a32bee216237a528e0775f3702c83803172399a7ac1a20c7aaa6eb09c` | NotSigned, 게시자·타임스탬프 없음 |

두 해시 모두 공식 체크섬과 일치했고, `playterm-sign verify`로 공식 `SHA256SUMS.txt.sig`를 앱 공개 키와 확인하여 `signature OK`를 얻었다. 저장소 시크릿 이름만 조회한 결과 `PLAYTERM_SIGNING_KEY`만 등록되어 있었으며 Windows 코드 서명 및 Apple 인증서/공증용 이름은 없었다. 시크릿 값은 조회하지 않았다. 원본·검사 JSON은 로컬 `target/release-validation/v0.5.0/`에 있다. 인증서 준비 일정은 아직 미확정이며 다음 서명 릴리스 후보 전에 담당자가 확정해야 한다.

## 실제 화면 증거

대표 캡처는 저장소에서도 확인할 수 있다: [SSH 취소](screenshots/validation/ssh-cancel.png), [공개 키 뒤 OTP 로그인](screenshots/validation/ssh-otp.png), [볼트 잠금 오류](screenshots/validation/vault-busy.png), [30탭 출력](screenshots/validation/tabs-30.png), [업데이트 변경 내역 스크롤](screenshots/validation/release-notes.png).

아래 파일은 검증 머신의 `target/`에 있는 로컬 산출물이며 GitHub에 게시된 증거가 아니다. 테스트 암호·키·서버는 모두 가짜 loopback 전용이다.

| 경로 | 내용 |
|---|---|
| `target/gui-validation/189/` | 키 암호 창, 취소, 재접속, 건너뛰기, 비밀번호·키 로그인 |
| `target/gui-validation/auth-final/r-reconnect.png`, `r-cancelled.png`, `auth-calls.txt` | 최종 릴리스 빌드의 실제 `r` 명령 재접속과 Esc 취소. 패키지 시험 전에 인증 요청 기록이 비어 있음을 확인 |
| `target/gui-validation/190/` | 공개 키 뒤 OTP 창·오답·취소·재접속 후 정상 로그인 |
| `target/gui-validation/191/` | 다른 프로세스 잠금에 따른 오류, 잠금 해제 후 볼트 생성 |
| `target/gui-validation/193-final/` | 최종 ZIP 네 개의 실제 SSH 연결 화면. x64는 Windows ARM64 에뮬레이션 |
| `target/gui-validation/195-tabs/` | 수정 후 1/10/30탭별 창 캡처, CSV, 서버 연결·전송량·입력 수와 종료 결과 |
| `target/gui-validation/196/02-notes-final.png`, `03-notes-scrolled.png` | 실제 업데이트 대화상자의 한글 변경 내역과 스크롤. 테스트 전용 99.0.0 미리보기이며 실제 업데이트를 설치하지 않음 |

## 짧은 부하 시험 측정

Windows ARM64, ASUS Vivobook S 15 S5507QA, RAM 약 32 GB, 릴리스 빌드, 기본 렌더러와 스크롤백으로 실행했다. 각 조건에서 출력과 실제 키 입력 후 20초 동안 1초 간격으로 수집했다. 다른 빌드 작업이 실행된 개발 머신이므로 전용 장비의 장시간 기준선으로 쓰지 않는다. CPU는 모든 논리 프로세서 용량 대비 앱 사용률이다.

| 탭 수 | 최대 private MiB | 최대 작업 집합 MiB | 최대 CPU % | 최대 스레드 | 최대 핸들 | 앱 종료 초 | 종료 후 서버 연결 |
|---|---:|---:|---:|---:|---:|---:|---:|
| 1 | 110.9 | 139.4 | 0.39 | 12 | 419 | 0.378 | 0 |
| 10 | 300.9 | 309.3 | 0.77 | 12 | 426 | 0.420 | 0 |
| 30 | 727.3 | 690.8 | 1.41 | 12 | 449 | 0.498 | 0 |

서버는 각 단계의 1/10/30 동시 연결을 확인했고 각 단계에서 실제 키 입력 13건을 받았다. 60개 자원 샘플의 Responding 값은 모두 true였다. 이는 입력 지연 p95나 8시간 누수 시험의 대체 결과가 아니다. 예열·휴지 뒤 메모리 회복과 출력 버퍼 상한은 장시간 시험에서 별도로 확인한다. 수정 전 분할 화면 측정은 비교 기록 `target/gui-validation/195-final/`로 남기고 위 탭 기준선에 섞지 않았다.
