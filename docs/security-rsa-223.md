# RSA 임시 완화와 남은 공급망 차단 (#223)

## 적용한 정책

로컬 RSA 개인 키 파일은 앱 내부에서 서명하지 않는다. 복호화·디코딩 후 **실제 알고리즘**을 검사해 키 제안/서명 전에 건너뛰고 사유와 대안을 표시한다. 파일명이나 확장자로 판단하지 않는다. 명시적 파일, 기본 키, 에이전트 실패 후 fallback, publickey partial success, 점프 호스트와 최종 호스트가 같은 함수를 사용한다. 암호화된 RSA 파일은 암호를 입력해 해독한 뒤에도 서명하지 않는다. 키 파일을 삭제·변환하지 않는다.

이는 SSH 파일 키 서명 경로의 **임시 완화**다. [RUSTSEC-2023-0071](https://rustsec.org/advisories/RUSTSEC-2023-0071.html)의 공식 패치 판정이나 앱 전체의 RSA 제거가 아니다. 개인 키 형식을 확인하는 디코딩은 여전히 수행한다. 원격 키 복구 공격을 재현하거나 상수 시간 동작을 증명한 시험도 아니다.

Ed25519/ECDSA 파일 키는 계속 사용할 수 있다. 외부 SSH 에이전트의 RSA는 에이전트에 서명을 요청하는 별도 경로이며, 앱이 그 RSA 개인 키를 갖지 않는다. 에이전트 구현의 보안 상태는 별도다. 이 변경 후에는 에이전트가 실패해도 **로컬 RSA 파일 서명으로 넘어가지 않는다**. “에이전트만 켜면 모든 RSA 문제가 해결된다”는 의미는 아니다. 비밀번호 인증은 서버가 허용할 때만 기존 절차로 진행하며 MFA·known_hosts 검증은 유지한다.

## 의존성과 공식 패치 상태

2026-11-06 확인. 선택된 RSA는 `0.10.0-rc.18`이며 이 변경에서는 lockfile을 바꾸지 않는다. 검사한 lockfile의 Git blob은 `08b57a6f6ebe57c3943c30ac5050be3fc9a89911`이다.

| 소비자 | RSA 요구사항 | 판단 |
| --- | --- | --- |
| russh 0.65.0 | `=0.10.0-rc.18` | SSH 개인 파일 서명은 위 정책으로 차단 |
| ssh-key 0.7.0-rc.11 | `0.10.0-rc.18` | 파일 서명과 에이전트 요청을 구분 |
| picky 7.0.0-rc.26 | `=0.10.0-rc.18` | RDP 의존 경로에 잔존 |
| sspi 0.23.0 | `=0.10.0-rc.18`, default features off | RDP 의존 경로에 잔존 |
| winscard 0.4.0 | `=0.10.0-rc.18`, hazmat, default features off | RDP 의존 경로에 잔존 |

레지스트리에서 russh 0.65.1도 확인했지만 동일한 RSA exact pin이다. RSA rc.21을 단독 지정한 `cargo update --dry-run`은 이 제약 때문에 해결되지 않는다. lockfile을 손으로 바꾸거나 root 의존성만 추가해 우회하지 않는다. 현 단계는 호환되는 소비자 업데이트를 기다리고 로컬 파일 서명을 차단하는 선택이다. 암호 구현을 임의로 포크/백포트하지 않는다. 의존성/MSRV 변경도 없다.

rc.21의 기본 경로 블라인딩 개선은 공식 advisory 해결과 구분한다. 확인 시 RustSec에는 patched version이 없으며, 버전 번호 변경만으로 해결 또는 검사 통과를 주장하지 않는다. rc.21은 이번 선택 그래프에 없으므로 해당 RNG 경로가 앱에 적용됐다는 주장도 하지 않는다.

다음 명령으로 전체 feature/target 및 배포 기본 feature를 구분해 기록했다. Windows x64/ARM64, macOS x64/ARM64와 Linux x64의 기본 feature 그래프 모두 RSA가 남는다.

```sh
cargo tree --locked --target all --all-features -i rsa@0.10.0-rc.18
cargo tree --locked --target all --all-features -e features -i rsa@0.10.0-rc.18
# 각 배포 대상에 반복 / repeat for each release target
cargo tree --locked --target aarch64-pc-windows-msvc -e features -i rsa@0.10.0-rc.18
cargo update -p rsa@0.10.0-rc.18 --precise 0.10.0-rc.21 --dry-run
```

RDP는 현재 UsernamePassword, TLS/CredSSP를 사용하고 standard RDP security는 비활성화한다. KDC 네트워크 처리는 미지원으로 반환한다. 이것은 스마트카드/개인 키 기능이 선택되지 않았다는 소스상의 범위 설명이며, 모든 OS·gateway·동적 라이브러리 경로의 RSA 개인 연산 미도달을 증명한 결과가 아니다. RDP의 실제 RSA 개인 연산 도달성은 아직 미검증이며, 의존성 포함 및 공개 키/인증서 검증과 구분해 남긴다. SSH 차단으로 RDP까지 영향 없다고 판정하지 않는다.

## 재검사 결과와 남은 작업

#221의 `deny.toml` 정책과 cargo-deny 0.20.2로 라이선스와 advisory를 재검사했다. RustSec DB revision은 `26fd2d43f7280a1fb925a3743ec463017afe7119`(2026-11-05 커밋), 실행일은 2026-11-06이다. **라이선스 통과, advisory 실패(exit 1)**이며 RSA 취약점과 현재 master의 yanked async-executor가 검출된다. 후자는 #221의 lockfile 갱신에 포함되어 있고, RSA 차단은 의존성 검사에서 취약한 패키지를 제거하지 않는다. #221을 최신 master와 합친 뒤에도 검사 결과를 다시 확인한다.

```sh
# #221 브랜치의 검토 정책을 사용 / use the reviewed policy from #221
cargo deny --all-features check --warn unmaintained licenses advisories
```

포괄 ignore, advisory 경고 강등, 검사 비활성화나 위험 예외를 추가하지 않았다. #207과 Draft PR #221은 차단 상태를 유지한다. #223도 임시 완화만으로 닫지 않는다. 호환 가능한 전체 소비자 업데이트, RDP 범위 확인, RustSec의 공식 패치 또는 별도 승인된 제한적 위험 정책이 남아 있다. 위험 수용은 이 문서나 이슈 등록으로 승인되지 않는다.

## 기능 검증

Windows ARM64에서 locked 전체 워크스페이스 테스트, strict clippy와 fmt를 통과했다. 이름을 바꾼 RSA, 기본 파일 fallback, Ed25519 실패/partial success 뒤 RSA, 암호화 파일, 에이전트 없음·서명 거부·인증 실패, 점프/최종 호스트와 재접속을 시험했다. mock 에이전트의 RSA 성공은 외부 signer 경로를 구분하기 위한 시험이며, 에이전트의 암호 안전성을 증명하지 않는다. Ed25519/ECDSA, 여러 키와 MFA, 키 암호 취소, TOFU, SFTP/포워딩 및 기존 RDP TLS 시험을 함께 실행했다.

실제 Windows 앱에서도 폐기 가능한 RSA 파일을 지정해 [서명 차단 사유와 비밀번호 입력](screenshots/validation/rsa-file-blocked.png)을 확인했다. 서버가 허용한 가짜 비밀번호로 [연결 성공](screenshots/validation/rsa-password-fallback.png) 후 서버 기록은 keyboard-interactive/password뿐이었으며 RSA publickey 인증은 없었다. 키 파일의 해시는 변하지 않았다. 실제 사용자 키와 에이전트를 사용하지 않았다. 지원 OS의 CI 결과는 PR에서 별도로 확인한다.
