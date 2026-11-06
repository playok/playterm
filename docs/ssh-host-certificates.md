# OpenSSH 호스트 인증서

사용자가 신뢰한 CA 공개 키를 앱 설정 폴더의 `known_hosts`에 직접 등록한다. CA 개인 키는 앱에 넣지 않는다. OpenSSH의 [호스트 인증서/CA 방식](https://man.openbsd.org/ssh-keygen#CERTIFICATES)을 사용한다.

```text
@cert-authority *.example.com ssh-ed25519 AAAA...관리자가_확인한_CA_공개키
@cert-authority [host.example.com]:2222 ssh-ed25519 AAAA...관리자가_확인한_CA_공개키
```

22번 포트는 호스트 이름, 다른 포트는 `[host]:port`로 범위를 정한다. 쉼표 목록, `*`/`?`, `!` 제외 패턴과 OpenSSH 해시 이름을 지원한다. CA는 지정한 대상에만 적용한다. 관리 화면에서는 CA 마커와 지문을 보고 삭제할 수 있으며, CA를 자동으로 신뢰하는 질문은 띄우지 않는다.

서버가 인증서를 제공하면 일반 공개 키보다 먼저 선택한다. CA 미설정, 다른 CA, 잘못된 서명, 아직 유효하지 않거나 만료된 인증서, 호스트 이름 불일치, 사용자 인증서, 필수 옵션, 폐기된 CA/키는 거부한다. 인증서의 호스트 principal은 접속에 사용한 호스트 이름 또는 IP와 대소문자를 제외하고 정확히 일치해야 하며 빈 목록은 거부한다. 신뢰한 CA가 설정된 대상이 일반 공개 키로만 응답해도 거부한다. 인증서 검증 실패 후 일반 키 TOFU로 재시도하거나 파일을 덮어쓰지 않는다.

일반 키만 제공하고 CA가 설정되지 않은 서버의 기존 TOFU는 유지한다. 다른 알고리즘도 키 변경으로 보고 차단한다. `@revoked` 대상 키를 거부한다. KRL 파일, 사용자 인증서 로그인, 에이전트 포워딩은 이번 기능 범위에 포함하지 않는다. 직접 파일을 편집할 때는 다른 앱 창에서 신뢰 기록을 바꾸지 않는 것이 좋다.

## 검증

2026-11-06 Windows ARM64에서 locked 전체 워크스페이스 테스트, strict clippy와 fmt를 통과했다. CA 서명·유효 기간·principal·종류·필수 옵션·폐기·변조 서명과 CA 범위/부정/포트/해시 패턴을 단위 시험했다. 실제 루프백 SSH 협상은 정상 인증서 로그인, 다른 CA, 만료, 다른 호스트, CA 없음의 실패를 검사하며 파일이 변하지 않고 TOFU 질문으로 우회하지 않는 것을 확인한다. 일반 키로 다운그레이드하는 서버도 거부한다.

실제 Windows 앱으로 CA 인증서를 검증한 뒤 가짜 비밀번호로 [로그인 성공](screenshots/validation/ssh-host-certificate-connected.png)을 확인했다. 같은 서버에 잘못된 CA를 지정하면 [인증서 검증 실패로 종료](screenshots/validation/ssh-host-certificate-rejected.png)하며 새 키 신뢰 질문은 없었다. 기존 일반 키 TOFU 회귀도 통과했다.

Ubuntu CI의 `scripts/test-openssh.sh`는 추가 OpenSSH 데몬에서 `ssh-keygen -h`로 서명한 호스트 인증서를 제공하고 `openssh_trusted_host_certificate_without_tofu`를 실행한다. 그 결과는 PR CI에서 별도로 확인한다. 네이티브 화면 시험은 russh 루프백 서버이며 실제 OpenSSH 시험과 구분한다.
