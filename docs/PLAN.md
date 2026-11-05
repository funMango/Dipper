# Dipper MVP 구현 계획

> 이 문서는 Dipper 첫 MVP의 기준 계획서입니다. 계획 변경은 이 파일을 수정하는 PR로 관리합니다.

- 저장소: https://github.com/funMango/Dipper (기본 브랜치 `main`)
- 확정 사항: 언어 **Rust**, 로컬 저장소 **DuckDB**, 주 사용 환경 **Windows**(단일 `dipper.exe` 배포). 다른 검색 엔진·서버형 DB는 쓰지 않으며, 전문(full-text) 검색은 MVP 이후로 미룹니다.
- 제품 원칙: 원본 DB와 그 인덱스는 읽기 전용으로만 다루고, 사용자는 SQL 없이 설정 파일과 명령어만으로 사용합니다. 제품은 특정 원본 DB에 묶이지 않습니다.
- 전제: 원본 데이터는 **매우 크고 테이블이 여러 개**입니다(정확한 행 수는 미상). 모든 설계는 메모리에 다 올리지 않는 스트리밍 적재를 기본으로 합니다.

## 1. MVP 목표

설정 파일 하나에 "어느 원본 DB의 어느 테이블들에서, 어떤 컬럼을, 어떤 조건으로" 가져올지 적으면, Dipper가 원본에 SELECT만 실행해 데이터를 청크 단위로 스트리밍하며 퍼와(dip) 로컬 DuckDB 파일에 담습니다. 대용량에서도 정해진 메모리 상한 안에서 동작하고 진행률을 보여 줍니다. 사용자는 SQL 없이 CLI로 조회·간단한 통계·내보내기를 할 수 있습니다. 결과물은 Windows에서 바로 실행되는 `dipper.exe` 하나입니다.

## 2. 범위

### 포함
- 원본 연결 추상화(`Source` 트레이트). 구현체 두 개:
  - SQLite: 테스트와 예제용 원본. 기본 feature.
  - Oracle: `oracle` cargo feature(`oracle` 크레이트). 실행 PC에 Oracle Instant Client 필요.
- 설정 파일 `dipper.toml`: 원본 여러 개, dip 여러 개(테이블, 컬럼, 구조화된 필터, 행 제한), 성능 설정(청크 크기, 메모리 상한)
- 읽기 전용 전체 추출(full 모드): SELECT 문만 생성·실행, 값은 바인드 파라미터로 전달, 청크 스트리밍 적재, 스테이징 테이블에 적재 후 교체
- 한 설정의 여러 테이블을 한 번에 실행(`dipper dip` = 전체, `dipper dip <name>...` = 일부)
- 메모리 상한, 진행률 표시, 실행 요약
- 로컬 조회: 테이블 목록, 필터 조회, 기본 통계, CSV/Parquet 내보내기
- 실행 이력 메타데이터(언제, 어느 원본에서, 몇 행, 걸린 시간)
- Windows 단일 exe 빌드와 배포, Linux·Windows CI, Oracle Instant Client 설치 안내
- 대용량 성능 테스트

### 제외 (MVP 이후)
- **시계열 갱신(1년치 유지 + 매일 어제 데이터 추가 + 1년 지난 데이터 삭제)**: MVP 이후 1순위. 설계 초안은 7절.
- GUI/웹 UI, 웹 서버/WAS(`dipper serve` 같은 서버 모드는 MVP 이후. 라이브러리(`lib.rs`)와 CLI(`main.rs`)를 분리해 두어 나중에 붙일 수 있게 함)
- 클라우드 배포 자동화(클라우드에서는 같은 실행 파일을 VM에서 실행하는 수준만)
- 내장 스케줄러, 전문 검색, 자연어 질의, 여러 원본 간 조인
- CDC/로그 기반 실시간 동기화, 원본에 쓰기, 다중 사용자/권한
- 테이블 간 병렬 추출(MVP는 테이블을 순서대로 처리)
- SQLite·Oracle 외 원본 구현(트레이트는 확장 가능하게 설계)

## 3. 기술 선택

| 영역 | 선택 | 이유 |
|---|---|---|
| 언어/빌드 | Rust stable, edition 2021, Cargo 단일 크레이트(`src/lib.rs` + `src/main.rs`) | 실행 파일 하나로 배포 |
| Windows 타깃 | `x86_64-pc-windows-msvc`, 정적 CRT(`.cargo/config.toml`에 `-C target-feature=+crt-static`) | 별도 런타임 설치 없이 `dipper.exe` 단독 실행 |
| CLI | `clap` (derive) | 표준적 |
| 설정 | `serde` + `toml`, 파일명 `dipper.toml` | Rust 생태계에서 가장 안정적인 설정 형식 (결정됨, 5절) |
| 오류 | `thiserror`(라이브러리), `anyhow`(바이너리) | |
| 표 출력 | `comfy-table` | |
| 진행률 | `indicatif` | 터미널 진행 막대, 비대화형 환경에서는 주기적 로그 줄 |
| 로그 | `tracing` + `tracing-subscriber` | `--verbose`, `--log-sql` |
| 로컬 저장소 | `duckdb` 크레이트, `bundled` feature | 별도 설치 불필요, exe에 포함 |
| 원본: SQLite | `rusqlite`, `bundled` feature, 읽기 전용 플래그로 열기 | 테스트·예제 |
| 원본: Oracle | `oracle` 크레이트, cargo feature `oracle` | Instant Client는 실행 시점에 로드 |
| 날짜 | `chrono` | |
| 테스트 | `cargo test`, `tempfile`, `assert_cmd` + `predicates`(CLI 테스트) | |
| 품질 | `cargo fmt --check`, `cargo clippy -- -D warnings` | |
| CI | GitHub Actions, `ubuntu-latest`와 `windows-latest` 매트릭스 | 회사 PC가 Windows |

비밀번호는 설정 파일에 직접 쓰지 않고 `${ENV_VAR}` 형식의 환경변수 치환으로만 받습니다.

### 핵심 구조
- `config`: `dipper.toml` 파싱·검증, 환경변수 치환
- `source`: `Source` 트레이트와 구현체(SQLite, Oracle). 트레이트 메서드는 `test_connection()`, `select(&SelectSpec) -> 청크 반복자`만 둡니다. 임의 SQL을 실행하는 공개 메서드는 만들지 않습니다.
- `sql`: 구조화된 필터를 원본 방언별 SELECT 문 + 바인드 파라미터로 변환. 테이블·컬럼 이름은 `^[A-Za-z_][A-Za-z0-9_$#.]*$` 검증 후 방언별로 인용. 필터 연산자: `eq, ne, in, gt, gte, lt, lte, between, like, is_null, not_null`.
- `store`: DuckDB 적재(Appender 사용), 스테이징 테이블 교체, 메타 테이블 `_dipper_runs`
- `pipeline`: 원본 청크 → DuckDB 적재 스트리밍 루프, 진행률, 메모리 상한 적용
- `query`: 로컬 조회·통계·내보내기
- `source::logging`: 원본에 실행된 모든 SQL 문을 기록하는 `LoggingSource` 래퍼(테스트와 `--log-sql` 옵션에서 사용)
- `cli`, `commands`: clap 정의와 명령별 처리. 명령 처리는 라이브러리 함수를 호출하고 출력만 담당하므로, 나중에 서버 모드가 같은 라이브러리를 그대로 쓸 수 있습니다.
- `error`: 공통 오류 타입과 종료코드 매핑(0 성공, 1 설정·실행 오류, 2 사용법 오류·미구현)

### 폴더·파일 구조 (확정)

규칙
- 구현이 여러 개이거나 하위 관심사가 둘 이상인 모듈은 폴더(`모듈/mod.rs` + 하위 파일), 하나뿐인 모듈은 단일 파일(`모듈.rs`)로 둡니다. 폴더 모듈은 모두 `mod.rs` 방식으로 통일합니다.
- 단위 테스트는 각 파일 안의 `#[cfg(test)] mod tests`, CLI·통합 테스트는 루트 `tests/`에 둡니다. 테스트 공용 도우미는 `tests/common/mod.rs`.
- 각 파일은 오른쪽에 적은 PR에서 처음 만듭니다. 그 PR 전에 빈 파일이나 자리만 잡는 파일을 미리 만들지 않습니다.

```text
Dipper/
├── .cargo/
│   └── config.toml              # Windows MSVC 정적 CRT                    PR1
├── .github/
│   └── workflows/
│       ├── ci.yml               # fmt·clippy·test, ubuntu+windows, exe 의존성 검사   PR1
│       ├── perf.yml             # 대용량 성능 테스트(수동 실행)                 PR5
│       └── release.yml          # v* 태그 → dipper.exe zip 릴리스            PR6
├── .gitignore                                                          PR1
├── Cargo.toml                                                          PR1
├── Cargo.lock                   # 바이너리 프로젝트라 커밋                       PR1
├── rust-toolchain.toml          # stable + rustfmt, clippy                  PR1
├── LICENSE                      # MIT, Copyright (c) 2026 funMango          PR1
├── README.md                                                           (기존, PR6에서 갱신)
├── docs/
│   ├── PLAN.md                                                         (기존)
│   ├── PERFORMANCE.md                                                  PR5
│   ├── ORACLE_SETUP.md                                                 PR6
│   └── ORACLE_CHECKLIST.md                                             PR7
├── examples/
│   ├── gen_large.rs             # 대용량 테스트 데이터 생성                     PR5
│   ├── make_sample.rs           # Quickstart용 샘플 SQLite DB 생성             PR6
│   └── dipper.example.toml                                             PR6
├── src/
│   ├── main.rs                  # 얇은 진입점: cli 실행 → 종료코드               PR1
│   ├── lib.rs                   # 모듈 선언, 공개 API                         PR1
│   ├── cli.rs                   # clap 정의와 명령 분기                        PR1
│   ├── error.rs                 # DipperError, 종료코드 매핑                   PR1
│   ├── commands/
│   │   ├── mod.rs                                                      PR2
│   │   ├── check.rs                                                    PR2
│   │   ├── dip.rs                                                      PR3
│   │   ├── tables.rs                                                   PR4
│   │   ├── show.rs                                                     PR4
│   │   ├── stats.rs                                                    PR4
│   │   └── export.rs                                                   PR4
│   ├── config/
│   │   ├── mod.rs               # 로드·검증 진입점                           PR2
│   │   ├── schema.rs            # serde 구조체(sources, dips, performance)   PR2
│   │   └── env.rs               # ${VAR} 치환                              PR2
│   ├── source/
│   │   ├── mod.rs               # Source 트레이트, SelectSpec, Chunk        PR2
│   │   ├── sqlite.rs                                                   PR2
│   │   ├── oracle.rs            # #[cfg(feature = "oracle")]             PR2
│   │   └── logging.rs           # LoggingSource                          PR3
│   ├── sql/
│   │   ├── mod.rs                                                      PR2
│   │   ├── ident.rs             # 식별자 검증·인용                           PR2
│   │   ├── filter.rs            # 필터 → WHERE + 바인드                      PR3
│   │   └── dialect.rs           # 방언별 인용·플레이스홀더                     PR3
│   ├── store/
│   │   ├── mod.rs               # DuckDB 열기, memory_limit, temp_directory PR3
│   │   ├── load.rs              # Appender 적재, 스테이징 교체                PR3
│   │   └── meta.rs              # _dipper_runs                           PR3
│   ├── pipeline/
│   │   ├── mod.rs               # 원본 청크 → 적재 루프, 여러 dip 실행·요약     PR3
│   │   └── progress.rs          # indicatif / 비대화형 로그                  PR3
│   └── query/
│       ├── mod.rs                                                      PR4
│       ├── show.rs                                                     PR4
│       ├── stats.rs                                                    PR4
│       └── export.rs                                                   PR4
└── tests/
    ├── cli.rs                   # --version, --help, 미구현 명령             PR1
    ├── common/
    │   └── mod.rs               # SQLite 테스트 원본·설정 생성 도우미           PR2
    ├── config.rs                                                       PR2
    ├── check.rs                                                        PR2
    ├── dip.rs                                                          PR3
    ├── readonly.rs              # SELECT 전용·SHA-256·바인드 검증             PR3
    ├── query.rs                                                        PR4
    └── perf.rs                  # 20만 행 스모크 + #[ignore] 500만 행          PR5
```

타입 매핑: 정수 → BIGINT, 실수/NUMBER(소수) → DOUBLE, 문자 → VARCHAR, 날짜 → DATE, 일시 → TIMESTAMP, NULL 유지, 그 외 → VARCHAR 문자열화.

### 대용량 처리 방식
- **스트리밍**: 원본 커서에서 `batch_rows`(기본 10,000)행씩 읽어 바로 DuckDB Appender로 넘기고, 청크를 버린 뒤 다음 청크를 읽습니다. 결과 전체를 메모리에 모으는 코드 경로는 없습니다.
- **Oracle 페치 설정**: 문장 실행 시 fetch array size와 prefetch rows를 `batch_rows`에 맞춰 왕복 횟수를 줄입니다.
- **메모리 상한**: 설정 `memory_limit`(기본 `"2GB"`)을 DuckDB `SET memory_limit`에 적용하고, `temp_directory`(기본 `.dipper/tmp`)로 넘치는 데이터를 디스크에 내립니다. Dipper 자체 버퍼는 청크 하나(+다음 청크 하나)로 제한합니다.
- **실패 안전**: 각 테이블은 `<name>__staging`에 적재한 뒤, 끝나면 짧은 트랜잭션 안에서 기존 테이블과 교체합니다. 중간에 실패하거나 Ctrl+C로 멈추면 기존 테이블은 그대로 남고 스테이징 테이블은 다음 실행 때 정리됩니다.
- **진행률**: 테이블마다 적재 행 수, 초당 행 수, 경과 시간을 표시합니다. 설정 `count_first = true`이면 먼저 `SELECT COUNT(*)`로 전체 행 수를 구해 퍼센트와 남은 시간을 보여 줍니다(기본 false, 대형 테이블에서 COUNT 자체가 느릴 수 있어서). 터미널이 아니면 `progress_every_rows`(기본 1,000,000)마다 로그 한 줄을 출력합니다.
- **여러 테이블**: `dipper dip`은 설정의 모든 dip을 순서대로 실행하고, 끝나면 테이블별 행 수·시간·성공 여부 요약표를 출력합니다. 기본은 하나가 실패해도 나머지를 계속 진행하고(`--fail-fast`로 즉시 중단), 하나라도 실패하면 종료코드 1.

### 설정 예시
```toml
store = ".dipper/store.duckdb"

[performance]
batch_rows = 10000
memory_limit = "2GB"
temp_directory = ".dipper/tmp"
progress_every_rows = 1000000

[sources.sample]
kind = "sqlite"
path = "examples/sample.db"

[sources.company]
kind = "oracle"
connect = "host:1521/SERVICE"
user = "${DIPPER_ORA_USER}"
password = "${DIPPER_ORA_PASSWORD}"

[[dips]]
name = "orders"
source = "company"
table = "SALES.ORDERS"
columns = ["ORDER_ID", "CUSTOMER_ID", "STATUS", "AMOUNT", "ORDER_DATE"]
filters = [{ column = "STATUS", op = "in", value = ["PAID", "SHIPPED"] }]

[[dips]]
name = "customers"
source = "company"
table = "SALES.CUSTOMERS"
columns = ["CUSTOMER_ID", "REGION", "CREATED_AT"]
count_first = true
```

## 4. PR 단위 작업과 완료 기준

각 PR은 `main`에서 새 브랜치로 만들고, CI가 Linux·Windows 모두에서 초록이어야 하며, 머지는 민호님 승인 후에만 합니다. 모든 PR 공통 기준: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, `cargo test`가 `ubuntu-latest`와 `windows-latest`에서 통과.

### PR1. 프로젝트 뼈대
- 3절 구조 중 PR1 표시 파일: `.cargo/config.toml`, `.github/workflows/ci.yml`, `.gitignore`(`/target`, `/.dipper`, `*.duckdb`, `*.duckdb.wal`), `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`, `LICENSE`, `src/main.rs`, `src/lib.rs`, `src/cli.rs`, `src/error.rs`, `tests/cli.rs`, 그리고 이 계획서(`docs/PLAN.md`) 갱신
- 완료 기준
  - [ ] 루트에 MIT `LICENSE` 파일이 있고 저작권 줄이 `Copyright (c) 2026 funMango`이며, `Cargo.toml`의 `license = "MIT"`.
  - [ ] PR이 추가하는 파일이 3절 구조에서 PR1로 표시된 파일과 정확히 같다(그 외 파일·빈 폴더·자리만 잡는 모듈 없음, 수정 파일은 `docs/PLAN.md`만).
  - [ ] `cargo build` 후 `dipper --version`이 `Cargo.toml` 버전을 출력하고 `dipper --help`에 서브커맨드 자리(`check`, `dip`, `tables`, `show`, `stats`, `export`)가 보인다(구현 전 명령은 "not implemented yet"과 종료코드 2).
  - [ ] CLI 테스트(`assert_cmd`)로 `--version` 출력을 확인한다.
  - [ ] PR에서 CI(fmt, clippy, test)가 Linux와 Windows 둘 다 통과한다.
  - [ ] Windows CI에서 빌드한 `dipper.exe`가 VC++ 런타임 DLL에 의존하지 않는다(`dumpbin /dependents` 결과에 `VCRUNTIME*.dll`이 없음).

### PR2. 설정 로더와 원본 연결 확인
- 범위: `dipper.toml` 로더(TOML, `serde` 스키마: 여러 source, 여러 dip, `[performance]`), `${VAR}` 환경변수 치환, 설정 검증(식별자 `^[A-Za-z_][A-Za-z0-9_$#.]*$` 포함), `Source` 트레이트(`test_connection()`, `select(&SelectSpec)` → 청크 반복자. 임의 SQL을 실행하는 공개 API 없음), 읽기 전용 SQLite 구현, Oracle 구현(feature `oracle`), `dipper check`.
- 데이터 추출(`dipper dip`)은 PR3입니다. PR2에서 각 원본의 `select`는 시그니처만 갖추고 `NotImplemented("select")`를 반환합니다.
- 파일: 3절 구조에서 PR2로 표시된 13개를 추가합니다(`src/commands/{mod,check}.rs`, `src/config/{mod,schema,env}.rs`, `src/source/{mod,sqlite,oracle}.rs`, `src/sql/{mod,ident}.rs`, `tests/common/mod.rs`, `tests/config.rs`, `tests/check.rs`). 수정은 `docs/PLAN.md`, `Cargo.toml`, `Cargo.lock`, `src/cli.rs`, `src/error.rs`, `src/lib.rs`, `tests/cli.rs`, `.github/workflows/ci.yml` 8개만 합니다.
- 의존성: `serde`(derive), `toml`, `serde_path_to_error`(오류에 필드 경로 표시), `rusqlite`(bundled), `oracle`(optional, `oracle = ["dep:oracle"]`), `tracing`, `tracing-subscriber`(fmt). 개발용으로 `tempfile`, `sha2`를 추가합니다. `duckdb`는 PR3에서 추가합니다. `rust-version`은 추가한 의존성의 최소 버전에 맞춰 올립니다(`rusqlite` 0.41 기준 1.89).
- 동작 규약
  - 전역 옵션: `-c, --config <PATH>`(기본 `dipper.toml`), `-v, --verbose`(stderr에 debug 로그).
  - 상대 경로(`store`, `temp_directory`, SQLite `path`)는 설정 파일이 있는 폴더를 기준으로 풉니다.
  - 기본값: `store = ".dipper/store.duckdb"`, `batch_rows = 10000`, `memory_limit = "2GB"`, `temp_directory = ".dipper/tmp"`, `progress_every_rows = 1000000`, `count_first = false`. `sources`는 1개 이상 필요하고 `dips`는 비어 있어도 됩니다.
  - 알 수 없는 키는 오류입니다(`deny_unknown_fields`).
  - 환경변수 치환: source의 문자열 필드(`path`, `connect`, `user`, `password`)에서 `${NAME}`(NAME은 `[A-Za-z_][A-Za-z0-9_]*`)을 치환합니다. 설정되지 않은 변수는 필드 경로와 변수 이름을 담은 오류가 됩니다. `password`는 값 전체가 `${NAME}` 하나여야 하며, 직접 적은 값은 오류입니다(오류에 그 값을 넣지 않음).
  - 비밀번호는 `Secret` 타입에 담습니다. `Debug`는 `***`로 표시하고 `Display`는 구현하지 않습니다.
  - 설정 오류 메시지에는 설정 파일의 원문 줄(snippet)을 넣지 않고 `<파일>[:<줄>:<열>]: <필드 경로>: <메시지>` 형식만 씁니다. 원문 줄에 비밀번호가 있을 수 있어서입니다.
  - 검증 항목: 필수 필드, dip의 `source`가 `sources`에 있는지, dip 이름 중복(대소문자 무시), dip 이름 형식(`^[A-Za-z_][A-Za-z0-9_]*$`, `_dipper`로 시작하거나 `__staging`으로 끝나면 안 됨), `table`·`columns`·필터 `column` 식별자(위 정규식 + 점으로 나눈 각 부분이 비어 있지 않음), `columns` 1개 이상·중복 없음, 필터 `op`(11종)와 값 모양(`is_null`·`not_null`은 값 없음, `in`은 비어 있지 않은 배열, `between`은 원소 2개 배열, 나머지는 단일 값), `limit`·`batch_rows`·`progress_every_rows`는 0보다 큼, `memory_limit`은 숫자(소수 가능) + 선택적 공백 + 단위 `KB|MB|GB|TB|KiB|MiB|GiB|TiB`(대소문자 무시)이고 0보다 큼.
  - `dipper check` 출력(stdout): 원본 이름순으로 한 줄씩 `<name> (<kind>): OK` 또는 `<name> (<kind>): FAIL - <사유>`, 마지막 줄 `<n> of <m> sources OK`. 실패가 있으면 stderr에 `dipper: check failed: <k> of <m> sources failed`.
  - 종료코드: 0 모든 원본 OK, 1 설정 오류(설정 파일 없음, 환경변수 미설정 포함) 또는 원본 하나 이상 실패, 2 사용법 오류·미구현 명령(`dip`, `tables`, `show`, `stats`, `export`). 설정 오류가 있으면 어떤 원본에도 접속하지 않습니다.
  - SQLite: 생성 플래그 없이 `SQLITE_OPEN_READ_ONLY`로 열고 `SELECT 1 FROM sqlite_master LIMIT 1`로 확인합니다. 원본에는 SELECT 외의 문장(PRAGMA 포함)을 실행하지 않습니다.
  - Oracle: 접속 후 `SELECT 1 FROM DUAL`로 확인합니다. 오류에 `DPI-1047`(Instant Client를 찾을 수 없음)이 있으면 원래 오류 뒤에 `Oracle Instant Client not found; see docs/ORACLE_SETUP.md`를 붙입니다. feature 없이 빌드한 경우 `kind = "oracle"` 원본은 설정 검증은 통과하고 check에서 `FAIL - oracle support is not built in (build with --features oracle)`가 됩니다.
- PR1 후속 수정
  - (a) `tests/cli.rs`의 `help_lists_subcommands`가 `contains("dip")`로 검사해서 `dipper`에도 걸립니다. 그래서 `dip` 서브커맨드가 빠져도 통과합니다. help 출력의 `Commands:` 절에서 각 줄의 첫 단어를 모아 `check, dip, tables, show, stats, export, help`와 순서까지 정확히 같은지 비교하도록 바꿉니다. 같은 파일의 미구현 검사는 `check`를 빼고 5개 명령만 확인합니다.
  - (b) `.gitignore` 표기: main의 파일은 `/target`, `/.dipper`, `*.duckdb`, `*.duckdb.wal`로 PR1 작업 지시와 같고, 이 계획서 PR1 절만 `target/`, `.dipper/`로 달랐습니다. 계획서를 `/target`, `/.dipper`로 고쳐 파일과 맞춥니다(이 PR의 `docs/PLAN.md` 갱신에 포함, `.gitignore`는 수정하지 않음).
- CI: `test` job(ubuntu·windows)에 기본 빌드 의존성 트리에 `oracle`·`odpic-sys`가 없는지 확인하는 단계, `cargo clippy --all-targets --features oracle --locked -- -D warnings`, `cargo test --features oracle --locked`를 추가합니다. `windows-exe` job은 기본 빌드와 `--features oracle` 빌드 둘 다 `dumpbin` 검사를 합니다(PR6 릴리스가 oracle feature로 빌드하므로).
- 완료 기준 (검증봇이 명령으로 확인)
  - [ ] `git diff --name-status origin/main...HEAD` 결과가 위 13개 파일 `A`와 8개 파일 `M`뿐이다. `src/main.rs`, `.gitignore`, `README.md`는 바뀌지 않는다.
  - [ ] `docs/PLAN.md`가 PR2 작업 지시서에 적힌 sha256과 같다.
  - [ ] `Cargo.toml`의 `[dependencies]`가 `anyhow`, `clap`, `thiserror`, `serde`, `toml`, `serde_path_to_error`, `rusqlite`(bundled), `oracle`(optional), `tracing`, `tracing-subscriber`뿐이고, `[dev-dependencies]`가 `assert_cmd`, `predicates`, `tempfile`, `sha2`뿐이며, `[features]`가 `default = []`, `oracle = ["dep:oracle"]`이다. `duckdb`가 없다.
  - [ ] 기본 빌드에 Oracle 의존성이 없다: `cargo tree -e normal --locked --prefix none`에 `oracle ` 또는 `odpic-sys `로 시작하는 줄이 없고, `--features oracle`을 붙이면 `oracle `로 시작하는 줄이 있다(CI 단계).
  - [ ] CI `test` job이 ubuntu·windows 둘 다 통과한다: fmt, 기본·oracle 각각 clippy `-D warnings`와 test, 의존성 트리 검사.
  - [ ] CI `windows-exe` job이 기본·oracle 빌드 둘 다 통과한다: `dumpbin /dependents`에 `VCRUNTIME`, `api-ms-win-crt`가 없고 `dipper.exe --version`이 성공한다.
  - [ ] `help_lists_subcommands`가 `Commands:` 절의 명령 목록을 정확히 비교한다. 로컬에서 `Command::Dip`에 `#[command(name = "dipx")]`를 임시로 붙이면 이 테스트가 실패한다(확인 후 되돌림).
  - [ ] `dipper dip`, `tables`, `show`, `stats`, `export`는 종료코드 2, stderr에 `not implemented yet`.
  - [ ] 설정 오류는 모두 종료코드 1이고 stderr에 다음 문자열이 들어 있다(각각 `tests/config.rs`의 테스트):
    - 설정 파일 없음: `config file not found`와 경로
    - dip에 `table` 누락: `dips[0]`, `table`
    - 없는 source 참조: `dips[0].source`와 그 이름
    - 중복된 dip 이름: `dips[1].name`, `duplicate`
    - 잘못된 식별자(`table = "ORDERS; DROP"`): `dips[0].table`, `invalid identifier`
    - 알 수 없는 `op`(`"contains"`): `dips[0].filters[0].op`, `contains`
    - 잘못된 `memory_limit`(`"2 gigs"`): `performance.memory_limit`
    - 알 수 없는 키(`colums`): `dips[0]`, `colums`
    - 설정되지 않은 환경변수: `sources.company.password`, `DIPPER_TEST_UNSET_VAR`
    - 직접 적은 비밀번호: `sources.company.password`, `${`
  - [ ] 비밀번호 비노출: 비밀번호 환경변수 값을 `PR2-canary-7f1d9c`로 두고 `dipper --verbose check`를 실행하면 stdout·stderr 어디에도 이 문자열이 없다(기본·oracle 빌드 둘 다). 직접 적은 비밀번호 값과, 문법 오류가 있는 줄의 비밀번호 값도 오류 출력에 나타나지 않는다. 설정 구조체 `Debug` 출력에 비밀번호 값이 없다(단위 테스트).
  - [ ] 원본 여러 개: 정상 SQLite 원본 2개면 종료코드 0, 두 줄 모두 `: OK`, 마지막 줄 `2 of 2 sources OK`. 정상 1개 + 없는 파일 1개면 종료코드 1, 없는 쪽 줄에 `FAIL -`와 그 경로, 마지막 줄 `1 of 2 sources OK`, stderr에 `check failed`. 실행 후에도 없는 파일은 생기지 않는다.
  - [ ] SQLite가 아닌 파일을 원본으로 주면 그 원본은 `FAIL -`, 종료코드 1.
  - [ ] 읽기 전용: `check` 전후로 SQLite 원본 파일의 SHA-256이 같고, 원본 폴더에 `-journal`, `-wal`, `-shm` 파일과 `.dipper/` 폴더가 생기지 않는다. `src/source/sqlite.rs` 단위 테스트에서 같은 연결로 `CREATE TABLE`과 `INSERT`를 시도하면 읽기 전용 오류로 실패한다.
  - [ ] oracle 빌드: Instant Client가 없는 CI에서 oracle 원본을 `check`하면 종료코드 1, 그 줄에 `DPI-1047`과 `docs/ORACLE_SETUP.md`. 기본 빌드: 같은 설정에서 `oracle support is not built in`, 종료코드 1.
  - [ ] `Source` 트레이트 메서드는 `name`, `kind`, `test_connection`, `select`뿐이고 SQL 문자열을 받는 공개 함수가 없다. `#[cfg(test)]` 밖에서 원본에 보내는 SQL은 `SELECT 1 FROM sqlite_master LIMIT 1`, `SELECT 1 FROM DUAL` 두 개뿐이다.

### PR3. 읽기 전용 대용량 추출 (`dipper dip`)
- SELECT 생성, 바인드 파라미터, 청크 스트리밍, DuckDB 메모리 상한·임시 디렉터리, 스테이징 교체, 진행률, 여러 테이블 실행과 요약, `_dipper_runs`, `LoggingSource`, `--log-sql`, `--fail-fast`
- 완료 기준
  - [ ] SQLite 테스트 원본(테이블 3개, 각 1만~10만 행)에서 `dipper dip` 한 번으로 세 테이블이 모두 적재되고, 각 테이블의 필터 결과와 DuckDB 행 수·값이 일치.
  - [ ] `dipper dip <name>`은 지정한 dip만 실행한다.
  - [ ] **SELECT 전용 검증**: `LoggingSource`가 원본에 전달된 모든 SQL 문을 기록하고, 테스트가 기록된 문장이 1개 이상이며 전부 앞뒤 공백·주석 제거 후 `SELECT`로 시작하고 `;`를 포함하지 않음을 확인. `INSERT|UPDATE|DELETE|MERGE|CREATE|DROP|ALTER|TRUNCATE|GRANT` 단어가 문장 키워드로 나타나면 실패(`count_first = true`의 `SELECT COUNT(*)`도 같은 검사 통과).
  - [ ] 실행 전후 SQLite 원본 파일의 SHA-256이 같다.
  - [ ] 필터 값은 모두 바인드 파라미터로 전달된다(값에 `' OR 1=1 --`를 넣은 테스트에서 결과 0행, SQL 문자열에 값이 없음).
  - [ ] 원본 청크가 `batch_rows` 이하 크기로만 전달됨을 테스트용 원본으로 확인(한 번에 전체를 요구하는 코드 경로 없음).
  - [ ] 설정한 `memory_limit`이 DuckDB에 실제로 적용된다(`SELECT current_setting('memory_limit')` 확인 테스트).
  - [ ] 같은 dip을 두 번 실행해도 테이블이 교체되어 중복 행이 없다.
  - [ ] 적재 도중 실패(원본이 중간 청크에서 오류를 내는 테스트용 원본)하면 기존 DuckDB 테이블이 그대로 남고, 다음 실행에서 남은 스테이징 테이블이 정리된다.
  - [ ] 세 테이블 중 하나가 실패해도 나머지 둘은 적재되고 요약표에 실패가 표시되며 종료코드 1. `--fail-fast`면 첫 실패에서 멈춘다.
  - [ ] 비대화형 실행(테스트 환경)에서 `progress_every_rows`마다 진행 로그 줄이 출력된다.
  - [ ] `_dipper_runs`에 테이블 실행 1회당 1행(dip 이름, 원본, 시작·종료 시각, 행 수, 성공 여부, 오류 메시지)이 기록된다.

### PR4. 로컬 조회·통계·내보내기
- `dipper tables`, `dipper show <table> --where col=val ... --limit N`, `dipper stats <table>`, `dipper export <table> --format csv|parquet --out PATH`
- 완료 기준
  - [ ] `tables`는 사용자 테이블과 행 수를 나열(메타·스테이징 테이블 제외, `--all`이면 포함).
  - [ ] `show`의 `--where`는 `=`, `!=`, `>`, `>=`, `<`, `<=`, `~`(포함 검색)만 허용하고 값은 바인드로 전달된다. 기본 `--limit`은 100. 사용자가 SQL을 직접 입력하는 경로가 없다.
  - [ ] `stats`는 행 수, 컬럼별 null 수, 숫자·날짜 컬럼의 min/max를 출력한다.
  - [ ] export는 DuckDB `COPY`로 스트리밍 기록되며(결과를 메모리에 모으지 않음), 내보낸 CSV·Parquet의 행 수가 DuckDB 테이블과 일치(테스트에서 다시 읽어 확인).
  - [ ] Windows 경로(`C:\...`, 공백 포함 경로)로 export가 동작한다(Windows CI 테스트).
  - [ ] 없는 테이블·컬럼은 이름이 포함된 오류와 종료코드 1.

### PR5. 대용량 성능 테스트
- 테스트 데이터 생성기(`examples/gen_large.rs` 또는 테스트 헬퍼): 컬럼 10개(정수·실수·문자·날짜 혼합) SQLite 테이블을 원하는 행 수로 생성
- 성능 테스트 두 단계:
  - 일반 CI: 20만 행 스모크 테스트(매 PR)
  - 대용량: 500만 행 테스트, `#[ignore]`로 표시하고 별도 워크플로(`workflow_dispatch`, 수동 실행)에서 `cargo test --release -- --ignored`로 Linux·Windows 각각 실행
- 측정 항목: 총 시간, 초당 행 수, 최대 메모리(Linux `/proc/self/status`의 `VmHWM`, Windows `PeakWorkingSetSize`), DuckDB 파일 크기
- 완료 기준
  - [ ] 500만 행 테스트가 Linux·Windows에서 성공하고, 적재 행 수가 원본과 일치.
  - [ ] `memory_limit = "1GB"`, `batch_rows = 10000`에서 프로세스 최대 메모리가 1.5GB 이하.
  - [ ] `batch_rows`를 1,000과 50,000으로 바꿔도 결과 행 수가 같고, 각 설정의 측정값이 표로 PR 설명에 기록된다(속도는 환경에 따라 달라서 통과 기준이 아니라 기록용).
  - [ ] 500만 행 테스트의 측정 결과를 `docs/PERFORMANCE.md`에 기록한다.

### PR6. 예제, 문서, Windows 배포
- `examples/`에 샘플 SQLite DB 생성 방법과 예제 `dipper.toml`
- README에 Quickstart(Windows PowerShell 기준 명령), 설치 방법 추가
- `docs/ORACLE_SETUP.md`: Windows용 Oracle Instant Client 설치 안내(Basic 또는 Basic Light 패키지 zip 받기, 예: `C:\oracle\instantclient_XX_X`에 풀기, 그 폴더를 사용자 `PATH`에 추가, 필요한 경우 해당 버전이 요구하는 VC++ 재배포 패키지 설치, `dipper check`로 확인). 회사 PC에서 설치 가능한지는 아직 확인되지 않았음을 문서에 명시하고, 관리자 권한 없이 사용자 폴더에 풀어 쓰는 방법을 함께 적습니다.
- 릴리스 워크플로: `v*` 태그를 푸시하면 Windows에서 `--release --features oracle`로 빌드한 `dipper.exe`를 zip으로 묶어 GitHub Release에 올림
- 완료 기준
  - [ ] 깨끗한 Linux·Windows 환경에서 Quickstart 명령을 순서대로 실행하면 에러 없이 끝까지 동작하고, CI 스모크 테스트가 같은 명령을 실행한다.
  - [ ] 테스트 태그(또는 `workflow_dispatch`)로 릴리스 워크플로를 실행하면 `dipper-<version>-windows-x86_64.zip`이 만들어지고, 그 안의 `dipper.exe --version`이 새 Windows 러너에서 추가 설치 없이 실행된다.
  - [ ] `docs/ORACLE_SETUP.md`가 있고 README에서 링크된다.
  - [ ] README의 소개 부분(한 줄 소개, Why, Features)에는 특정 DB 제품명이 없다. 설치·Quickstart에 DB 이름을 쓸지는 결정 필요 3번을 따른다.

### PR7 (선택). Oracle 실연결 확인
- 클라우드 에이전트는 회사 Oracle에 접속할 수 없으므로 코드 변경 없이 `docs/ORACLE_CHECKLIST.md`에 수동 확인 절차를 적고, 민호님 Windows PC에서 `dipper check`, `dipper dip`(여러 테이블)를 실행해 결과를 기록합니다.
- 완료 기준: 체크리스트의 각 항목(Instant Client 설치, 연결, 테이블별 행 수 일치, 진행률 표시, 최대 메모리, `--log-sql`에 SELECT만 있음)에 민호님 확인 결과가 기록된다.

## 5. 결정됨

| 항목 | 결정 |
|---|---|
| 언어 | Rust |
| 로컬 저장소 | DuckDB (다른 검색 엔진·서버형 DB 사용 안 함, 전문 검색은 MVP 이후) |
| 시계열 갱신(1년치 유지 + 매일 추가) | MVP에서 제외, MVP 이후 1순위로 진행(7절) |
| 주 사용 환경 | 회사 PC는 Windows. 단일 `dipper.exe` 배포, CI에 Windows 포함 |
| 데이터 규모 | 매우 크고 테이블이 여러 개(정확한 행 수 미상). 대용량 스트리밍 적재, 메모리 상한, 진행률, 여러 테이블 실행, 성능 테스트를 MVP에 포함 |
| 라이선스 | MIT (Copyright (c) 2026 funMango), PR1에 `LICENSE` 포함 |
| 웹 서버/WAS | MVP에서 사용 안 함. 단일 CLI exe 유지, 나중에 `dipper serve`를 붙일 수 있게 lib/main 분리 유지 |
| 폴더·파일 구조 | 3절 '폴더·파일 구조'대로 확정 |
| 설정 파일 형식 | 설정 파일 형식은 TOML(`dipper.toml`)로 확정 (2026-11-05, 민호님 승인) |

## 6. 결정 필요

1. **Oracle Instant Client 설치 가능 여부(미확인)**: 회사 Windows PC에 Instant Client를 설치하거나 사용자 폴더에 풀어 `PATH`에 추가할 수 있나요? 안 되면 Oracle 연결 방식을 다시 검토해야 합니다.
2. **Oracle 실연결 확인(PR7)**을 민호님 PC에서 직접 해 주실 수 있나요? 회사 DB 접속 정보는 저장소나 봇에 저장하지 않는 것을 전제로 합니다.
3. **문서의 DB 이름 표기**: 소개 부분은 계속 "multiple databases"로 두고, 설치·Quickstart에서만 DuckDB와 Oracle 같은 실제 이름을 써도 될까요? 제안: 그렇게 함.
4. **사용 방식**: 필터를 설정 파일에 적는 방식이면 "SQL 몰라도 쓴다"는 목표에 충분한가요, 아니면 MVP에 테이블·컬럼을 대화형으로 고르는 기능이 필요한가요? 제안: MVP는 설정 파일 방식.
5. **회사 PC 자원**: 기본 메모리 상한 2GB와, 수십 GB 이상일 수 있는 DuckDB 파일·임시 파일을 둘 디스크 공간이 괜찮나요? 대략적인 PC 메모리와 남은 디스크 용량을 알려 주시면 기본값을 맞춥니다.

## 7. MVP 이후: 시계열 갱신 설계 초안

MVP 이후 첫 작업으로 진행합니다. 착수 전에 이 절을 확정합니다.

- 설정: `mode = "rolling"`, `time_column`, `retention_days`(기본 365), `lookback_days`(기본 0), `timezone`
- 동작: 기준일 `as_of`(기본 오늘, 테스트용 `--as-of`)에서 `[as_of - retention_days, as_of - 1]` 기간을 유지합니다. `_dipper_partitions`(dip, 날짜, 행 수, 적재 시각)로 빠진 날짜를 찾아 날짜별 기간 조건 SELECT로 읽고, DuckDB 트랜잭션 안에서 그 날짜를 지우고 다시 넣습니다(재실행해도 결과 동일). 기간보다 오래된 날짜는 삭제합니다. 첫 실행은 1년치 백필이며 MVP의 스트리밍·메모리 상한을 그대로 씁니다.
- 매일 실행: 내장 스케줄러 없이 Windows 작업 스케줄러로 `dipper dip`을 호출하도록 안내합니다.
- 검증 방향: SQLite에 400일치 데이터를 넣고 `--as-of`를 바꿔 가며 첫 실행 범위, 재실행 시 행 수 불변, 하루 진행 시 하루 추가·하루 삭제, 며칠 빠진 경우 자동 보충, 원본에 SELECT만 실행됨을 확인합니다.
- 그때 정할 것: "어제"의 기준 시간대(제안: `Asia/Seoul`), 지난 날짜 데이터가 나중에 바뀌는 경우 다시 읽을 일수(`lookback_days`).
