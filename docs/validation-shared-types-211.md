# 공유 프로필 타입 분리 검증 (#211)

`playterm-types`는 serde와 기존의 가벼운 i18n 크레이트만 참조한다. 프로토콜, 포워딩, 연결 유지, 시리얼 및 RDP 설정과 순수 파싱·표시 기능을 옮겼다. 네트워크 크레이트의 기존 공개 경로는 재수출로 유지한다. 실제 시리얼 포트 생성과 RDP 장치·파일 시스템 검사는 전송 구현에 남긴다.

Windows ARM64에서 다음을 확인했다.

- 전체 워크스페이스 테스트와 strict clippy, fmt.
- `cargo tree --locked -p playterm-config --edges normal`: playterm-net, tokio, IronRDP, OpenSSL 없음.
- `cargo test --locked -p playterm-config --target-dir target/config-only-211`: 별도의 빈 빌드 디렉터리에서 설정 테스트 수행. 전송 구현을 컴파일하지 않음.
- `sessions-v0.6.0.toml` 고정 입력으로 예전 프로필의 forward/keepalive/serial/rdp 테이블을 읽고 쓴 뒤 모든 저장 값과 테이블 이름이 유지됨을 검사한다.
- 실제 앱을 실행해 예전 프로필이 사이드바에 나타나고 로컬 Telnet 서버에 연결되어 프롬프트가 표시됨을 확인했다. [실제 화면](screenshots/validation/shared-types-legacy-profile.png)

공유 타입 분리만으로 설정 크레이트의 기존 터미널 문자셋 의존까지 제거하지는 않는다.
