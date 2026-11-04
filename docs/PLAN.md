# Dipper MVP 구현 계획

> 이 문서는 Dipper 첫 MVP의 기준 계획서입니다. 계획 변경은 이 파일을 수정하는 PR로 관리합니다.

- 저장소: https://github.com/funMango/Dipper (기본 브랜치 `main`)
- 확정 사항: 언어 **Rust**, 로컬 저장소 **DuckDB**. 다른 검색 엔진·서버형 DB는 쓰지 않으며, 전문(full-text) 검색은 MVP 이후로 미룹니다.
- 제품 원칙: 원본 DB와 그 인덱스는 읽기 전용으로만 다루고, 사용자는 SQL 없이 설정 파일과 명령어만으로 사용합니다. 제품은 특정 원본 DB에 묶이지 않습니다.

## 1. MVP 목표

설정 파일 하나에 "어느 원본 DB의 어느 테이블에서, 어떤 컬럼을, 어떤 조건으로" 가져올지 적으면, Dipper가 원본에 SELECT만 실행해 데이터를 퍼와(dip) 로컬 DuckDB 파일에 담고, 사용자는 SQL 없이 CLI로 조회·간단한 통계·내보내기를 할 수 있습니다. 또한 "최근 1년치를 유지하면서 매일 어제 데이터를 추가하고 1년이 지난 데이터는 지우는" 시계열 갱신(rolling window)을 지원합니다(5절의 제안 참고).

## 2. 범위

### 포함
- 원본 연결 추상화(`Source` 트레이트). 구현체 두 개:
  - SQLite: 테스트와 예제용 원본. 기본 feature.
  - Oracle: `oracle` cargo feature로 선택 빌드(`oracle` 크레이트, 실행 PC에 Oracle Instant Client 필요).
- 설정 파일 `dipper.toml`: 원본 목록, dip 목록(테이블, 컬럼, 구조화된 필터, 행 제한, 갱신 방식)
- 읽기 전용 추출: SELECT 문만 생성·실행, 값은 바인드 파라미터로 전달, 청크 단위로 읽어 DuckDB에 적재
- 갱신 방식 두 가지: `full`(매번 전체 교체), `rolling`(기간 유지형 시계열 갱신)
- 로컬 조회: 테이블 목록, 필터 조회, 기본 통계, CSV/Parquet 내보내기
- 실행 이력 메타데이터(언제, 어느 원본에서, 몇 행)

### 제외 (MVP 이후)
- GUI/웹 UI, 클라우드 배포 자동화(클라우드에서는 같은 실행 파일을 VM에서 실행하는 수준만)
- 내장 스케줄러(매일 실행은 OS 스케줄러로 호출, 문서로 안내)
- 전문 검색, 자연어 질의, 여러 원본 간 조인
- CDC/로그 기반 실시간 동기화, 원본에 쓰기, 다중 사용자/권한
- SQLite·Oracle 외 원본 구현(트레이트는 확장 가능하게 설계)

## 3. 기술 선택

| 영역 | 선택 | 이유 |
|---|---|---|
| 언어/빌드 | Rust stable, edition 2021, Cargo 단일 크레이트(`src/lib.rs` + `src/main.rs`) | 실행 파일 하나로 배포 |
| CLI | `clap` (derive) | 표준적 |
| 설정 | `serde` + `toml`, 파일명 `dipper.toml` | Rust 생태계에서 가장 안정적인 설정 형식 (결정 필요 4번) |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) | |
| 표 출력 | `comfy-table` | |
| 로컬 저장소 | `duckdb` 크레이트, `bundled` feature | 별도 설치 불필요 |
| 원본: SQLite | `rusqlite`, `bundled` feature, 읽기 전용 플래그로 열기 | 테스트·예제 |
| 원본: Oracle | `oracle` 크레이트, cargo feature `oracle` | Instant Client는 실행 시점에 로드 |
| 날짜 | `chrono` | rolling 계산 |
| 테스트 | `cargo test`, `tempfile`, `assert_cmd` + `predicates`(CLI 테스트) | |
| 품질 | `cargo fmt --check`, `cargo clippy -- -D warnings` | |
| CI | GitHub Actions (ubuntu-latest, windows-latest) | 결정 필요 6번에 따라 조정 |

비밀번호는 설정 파일에 직접 쓰지 않고 `${ENV_VAR}` 형식의 환경변수 치환으로만 받습니다.

### 핵심 구조
- `config`: `dipper.toml` 파싱·검증, 환경변수 치환
- `source`: `Source` 트레이트와 구현체(SQLite, Oracle). 트레이트 메서드는 `test_connection()`, `select(&SelectSpec) -> 행 반복자`만 둡니다. 임의 SQL을 실행하는 공개 메서드는 만들지 않습니다.
- `sql`: 구조화된 필터를 원본 방언별 SELECT 문 + 바인드 파라미터로 변환. 테이블·컬럼 이름은 `^[A-Za-z_][A-Za-z0-9_$#.]*$` 검증 후 방언별로 인용. 필터 연산자: `eq, ne, in, gt, gte, lt, lte, between, like, is_null, not_null`.
- `store`: DuckDB 적재(Appender 사용), 메타 테이블 `_dipper_runs`, `_dipper_partitions`
- `query`: 로컬 조회·통계·내보내기
- `audit`: 원본에 실행된 모든 SQL 문을 기록하는 `LoggingSource` 래퍼(테스트와 `--log-sql` 옵션에서 사용)

타입 매핑: 정수 → BIGINT, 실수/NUMBER(소수) → DOUBLE, 문자 → VARCHAR, 날짜 → DATE, 일시 → TIMESTAMP, NULL 유지, 그 외 → VARCHAR 문자열화.

### 설정 예시
```toml
store = ".dipper/store.duckdb"

[sources.sample]
kind = "sqlite"
path = "examples/sample.db"

[sources.company]
kind = "oracle"
connect = "host:1521/SERVICE"
user = "${DIPPER_ORA_USER}"
password = "${DIPPER_ORA_PASSWORD}"

[[dips]]
name = "orders_recent"
source = "sample"
table = "orders"
columns = ["order_id", "customer_id", "status", "amount", "order_date"]
filters = [{ column = "status", op = "in", value = ["PAID", "SHIPPED"] }]
mode = "rolling"
time_column = "order_date"
retention_days = 365
```

## 4. PR 단위 작업과 완료 기준

각 PR은 `main`에서 새 브랜치로 만들고, CI가 초록이어야 하며, 머지는 민호님 승인 후에만 합니다. 모든 PR 공통 기준: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test` 통과.

### PR1. 프로젝트 뼈대
- `Cargo.toml`, `src/main.rs`, `src/lib.rs`, clap 엔트리, `.gitignore`(`target/`, `.dipper/`), GitHub Actions 워크플로
- 완료 기준
  - [ ] `cargo build` 후 `dipper --version`이 `Cargo.toml` 버전을 출력하고 `dipper --help`에 서브커맨드 자리(`check`, `dip`, `tables`, `show`, `stats`, `export`)가 보인다(구현 전 명령은 "not implemented yet"과 종료코드 2).
  - [ ] CLI 테스트(`assert_cmd`)로 `--version` 출력을 확인한다.
  - [ ] PR에서 CI(fmt, clippy, test)가 통과한다.

### PR2. 설정 로더와 원본 연결 확인
- `dipper.toml` 스키마, 환경변수 치환, `Source` 트레이트, SQLite 구현, Oracle 구현(feature `oracle`), `dipper check`
- 완료 기준
  - [ ] 필수 필드 누락, 존재하지 않는 source 참조, 잘못된 식별자, 알 수 없는 `op`에 대해 필드 경로가 포함된 오류 메시지와 종료코드 1(각각 테스트).
  - [ ] 설정되지 않은 환경변수를 참조하면 변수 이름이 포함된 오류(값은 출력하지 않음).
  - [ ] SQLite 원본에 `dipper check` 성공 시 원본별 `OK` 출력, 종료코드 0.
  - [ ] 비밀번호 값이 어떤 출력·오류·로그에도 나타나지 않음을 테스트로 확인.
  - [ ] SQLite 원본은 읽기 전용 플래그로 열린다(쓰기 시도가 실패하는 테스트).
  - [ ] CI에서 `cargo build --features oracle`이 성공한다(Instant Client 없이 컴파일만 확인).

### PR3. 읽기 전용 추출 (`dipper dip`, full 모드)
- SELECT 생성, 바인드 파라미터, 청크 fetch(기본 10,000행), DuckDB 적재, `_dipper_runs`, `LoggingSource`, `--log-sql`
- 완료 기준
  - [ ] SQLite 테스트 원본(1만 행)에서 필터 결과와 DuckDB 테이블의 행 수·값이 일치.
  - [ ] **SELECT 전용 검증**: `LoggingSource`가 원본에 전달된 모든 SQL 문을 기록하고, 테스트가 기록된 문장이 1개 이상이며 전부 앞뒤 공백·주석 제거 후 `SELECT`로 시작하고 `;`를 포함하지 않음을 확인. `INSERT|UPDATE|DELETE|MERGE|CREATE|DROP|ALTER|TRUNCATE|GRANT` 단어가 문장 키워드로 나타나면 실패.
  - [ ] 실행 전후 SQLite 원본 파일의 SHA-256이 같다.
  - [ ] 필터 값은 모두 바인드 파라미터로 전달된다(값에 `' OR 1=1 --`를 넣은 테스트에서 결과 0행, SQL 문자열에 값이 없음).
  - [ ] 같은 dip을 두 번 실행해도 테이블이 교체되어 중복 행이 없다.
  - [ ] `_dipper_runs`에 실행 1회당 1행(dip 이름, 원본, 시작·종료 시각, 행 수, 성공 여부)이 기록된다.
  - [ ] 실패 시(예: 원본 테이블 없음) 기존 DuckDB 테이블이 그대로 남는다(트랜잭션).

### PR4. 로컬 조회·통계·내보내기
- `dipper tables`, `dipper show <table> --where col=val ... --limit N`, `dipper stats <table>`, `dipper export <table> --format csv|parquet --out PATH`
- 완료 기준
  - [ ] `tables`는 사용자 테이블만 나열(메타 테이블 제외, `--all`이면 포함).
  - [ ] `show`의 `--where`는 `=`, `!=`, `>`, `>=`, `<`, `<=`, `~`(포함 검색)만 허용하고 값은 바인드로 전달된다. 사용자가 SQL을 직접 입력하는 경로가 없다.
  - [ ] `stats`는 행 수, 컬럼별 null 수, 숫자·날짜 컬럼의 min/max를 출력한다.
  - [ ] export한 CSV·Parquet의 행 수가 DuckDB 테이블과 일치(테스트에서 다시 읽어 확인).
  - [ ] 없는 테이블·컬럼은 이름이 포함된 오류와 종료코드 1.

### PR5. 시계열 갱신 (`mode = "rolling"`)
- 설정: `time_column`, `retention_days`(기본 365), `lookback_days`(기본 0), 기준 시간대(`timezone`, 결정 필요 2번)
- 동작:
  1. 기준일 `as_of`(기본: 오늘, 테스트용 `--as-of YYYY-MM-DD` 옵션)를 정한다. 적재 대상 기간은 `[as_of - retention_days, as_of - 1]`(어제까지).
  2. `_dipper_partitions`(dip, 날짜, 행 수, 적재 시각)를 보고 대상 기간 중 아직 없는 날짜와 최근 `lookback_days`일을 찾는다. 처음 실행이면 기간 전체를 백필한다.
  3. 날짜별로 `time_column >= :start AND time_column < :end` 조건의 SELECT로 읽어, DuckDB 트랜잭션 안에서 그 날짜 행을 지우고 다시 넣는다(재실행해도 결과가 같음).
  4. 기간보다 오래된 날짜의 행과 파티션 기록을 지운다.
  5. 매일 실행은 OS 스케줄러(Windows 작업 스케줄러, cron)로 `dipper dip <name>`을 호출하도록 PR6 문서에서 안내한다.
- 완료 기준 (SQLite 원본에 400일치 날짜별 데이터를 넣고 테스트)
  - [ ] 첫 실행(`--as-of 2026-01-01`, `retention_days=365`) 후 DuckDB에 정확히 `2025-01-01`~`2025-12-31` 날짜만 있고 날짜별 행 수가 원본과 같다.
  - [ ] 같은 `--as-of`로 다시 실행하면 행 수가 변하지 않는다.
  - [ ] `--as-of`를 하루 늘려 실행하면 새 날짜 1일이 추가되고 가장 오래된 1일이 삭제된다.
  - [ ] 3일 동안 실행하지 않은 상황(`--as-of`를 3일 건너뜀)에서 빠진 3일이 모두 채워진다.
  - [ ] `lookback_days=1`이면 어제 이전 하루가 다시 적재되고, 그 사이 원본에서 바뀐 값이 반영된다.
  - [ ] 원본에 실행된 문장이 모두 SELECT이고 날짜 경계는 바인드 파라미터로 전달된다(PR3의 검증 재사용).
  - [ ] 원본 파일 SHA-256이 실행 전후 같다.

### PR6. 예제와 빠른 시작 문서
- `examples/`에 샘플 SQLite DB를 만드는 방법(Rust 예제 바이너리 또는 테스트 헬퍼)과 예제 `dipper.toml`, README에 Quickstart·설치·매일 실행 안내 추가
- 완료 기준
  - [ ] 깨끗한 환경에서 Quickstart 명령을 순서대로 실행하면 에러 없이 끝까지 동작하고, CI의 스모크 테스트가 같은 명령을 실행한다.
  - [ ] README의 소개 부분(한 줄 소개, Why, Features)에는 특정 DB 제품명이 없다. 설치·Quickstart에 DB 이름을 쓸지는 결정 필요 8번을 따른다.
  - [ ] Oracle 사용법(feature 빌드, Instant Client 필요, 읽기 전용 계정 권장)이 문서화되어 있다.

### PR7 (선택). Oracle 실연결 확인
- 클라우드 에이전트는 회사 Oracle에 접속할 수 없으므로 코드 변경 없이 `docs/ORACLE_CHECKLIST.md`에 수동 확인 절차를 적고, 민호님 PC에서 `dipper check`, `dipper dip`(full, rolling)를 실행해 결과를 기록합니다.
- 완료 기준: 체크리스트의 각 항목(연결, full dip 행 수 일치, rolling dip 날짜 범위, `--log-sql`에 SELECT만 있음)에 민호님 확인 결과가 기록된다.

## 5. 시계열 갱신을 MVP에 넣을지에 대한 제안

**MVP에 넣는 것을 제안합니다(PR5).**
- 민호님이 실제로 원하는 사용 방식이 "1년치 유지 + 매일 추가"라서, 이게 빠지면 MVP로 실사용을 확인하기 어렵습니다.
- 추가 로직은 대부분 DuckDB 쪽(날짜별 교체, 오래된 날짜 삭제)이고 원본에는 기간 조건 SELECT만 실행하므로 읽기 전용 원칙과 충돌하지 않습니다.
- SQLite와 `--as-of` 옵션으로 하루하루 지나가는 상황을 CI에서 재현할 수 있어 검증이 명확합니다.
- 범위를 지키기 위해 내장 스케줄러, 시간 단위 파티션, 원본 변경 감지(CDC)는 넣지 않습니다.

이 기능을 뒤로 미루기로 하면 PR5를 MVP 이후로 옮기고 PR6을 PR5로 당깁니다.

## 6. 결정 필요

1. **시계열 갱신을 MVP에 넣을까요?** 제안: 넣음(PR5).
2. **"어제"의 기준 시간대**: 원본 날짜 컬럼이 어느 시간대 기준인지, "어제"를 한국 시간(KST) 기준으로 계산해도 되는지. 제안: 기본값 `Asia/Seoul`, 설정으로 변경 가능.
3. **늦게 들어오거나 수정되는 데이터**: 원본에서 지난 날짜 데이터가 나중에 바뀌는 경우가 있나요? 있다면 매일 최근 며칠을 다시 읽을지(`lookback_days`). 제안: 기본 0, 필요 시 1~3.
4. **설정 파일 형식**: TOML(`dipper.toml`)로 해도 될까요? 제안: TOML.
5. **데이터 규모**: dip 하나에서 하루 몇 행, 1년치 몇 행 정도인가요? 수천만 행 이상이면 청크 크기와 날짜별 적재 방식을 조정합니다.
6. **실행 환경**: 주로 쓰실 OS가 Windows인가요? 회사 PC에 Oracle Instant Client를 설치할 수 있나요? 제안: CI에서 Linux와 Windows 둘 다 빌드.
7. **Oracle 실연결 확인(PR7)**을 민호님 PC에서 직접 해 주실 수 있나요? 회사 DB 접속 정보는 저장소나 봇에 저장하지 않는 것을 전제로 합니다.
8. **문서의 DB 이름 표기**: 소개 부분은 계속 "multiple databases"로 두고, 설치·Quickstart에서만 DuckDB와 Oracle 같은 실제 이름을 써도 될까요? 제안: 그렇게 함.
9. **사용 방식**: 필터를 설정 파일에 적는 방식이면 "SQL 몰라도 쓴다"는 목표에 충분한가요, 아니면 MVP에 테이블·컬럼을 대화형으로 고르는 기능이 필요한가요? 제안: MVP는 설정 파일 방식.
10. **라이선스**: MIT와 Apache-2.0 중 어느 쪽으로 할까요? 제안: MIT. (LICENSE 파일은 PR1에 추가)
