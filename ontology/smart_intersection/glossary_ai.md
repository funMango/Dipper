# 스마트교차로 AI 용어집 (온톨로지 v2)

원본은 `ontology/smart_intersection/ontology.yaml`입니다. 이 문서는 AI에게 그대로 넣는 요약본입니다. 조회는 Dipper store(`.dipper/store.duckdb`)의 `onto_v2` 스키마 뷰로 합니다.

## 1. 계층

교차로 → 방향(접근로) → 좌/직/우(회전). 카메라는 교차로와 방향에 붙습니다.

| 민호님 말 | 온톨로지 | DB 테이블(회사 DB CRSRD) | store 뷰 | 기계 키 | 사람이 읽는 키 |
|---|---|---|---|---|---|
| 교차로 | Intersection | M_CRSRD_INF | `onto_v2.v_intersection` | `NODE_ID` | 교차로명(CRSRD_NM) |
| 방향 | Approach(접근로) | M_CRSRD_ACSR_INF | `onto_v2.v_approach`, 조인용 `onto_v2.v_approach_key` | `NODE_ID:ACSR_ID` | `<교차로명> 북(남향)` |
| 좌/직/우 | Movement(회전) | M_CRSRD_DRCT_INF | `onto_v2.v_movement` | `NODE_ID:ACSR_ID:DRCT_CD` | `<교차로명> 북(남향) 좌` |
| 차로 | Lane | M_CRSRD_LANE_INF | `onto_v2.v_lane` | NODE_ID, ACSR_ID, DRCT_CD, LANE_NO, AREA_SEQNO | - |
| 카메라 | Camera | M_CMR_INF | `onto_v2.v_camera` | `CMR_ID` | `<교차로명> 북(남향) 카메라 <CMR_ID>` |
| 신호 제어기 | SignalController | M_CRSRD_SGNL_INF | `onto_v2.v_signal_controller` | `SGNL_CTRL_ID` | - |
| 신호값(주기·녹색) | ApproachSignalPlan | M_CRSRD_ACSR_SGNL_CNDT | `onto_v2.v_approach_signal` | `NODE_ID:ACSR_ID` | `<교차로명> 북(남향)` |
| 코드 | Code | M_CD_INF, M_CD_GRP_INF | `onto_v2.v_code` | GRP_CD, CD | - |

## 2. 꼭 지킬 것

1. **'방향'이라는 말이 둘입니다.** 민호님의 '방향'(예: 북(남향))은 접근로(ACSR)입니다. DB의 '교차로 방향'(DRCT_CD)은 좌/직/우입니다.
2. **방향 표기는 `북(남향)`** 입니다. 앞은 진입 위치, 괄호 안은 진행 방향이고, 진행 방향은 늘 진입 위치의 반대입니다. 공백 없이 씁니다. 동북→북동, 동남→남동, 서남→남서, 서북→북서로 맞춥니다.
3. 진입 위치는 `ACSR_NM`에서 읽습니다(뷰의 `position_bearing`). `ACSR_DRCT` 코드는 키로 쓰지 않습니다(56행이 다름).
4. `ACSR_ID`(ACSR000001 등)는 교차로 안에서만 유일합니다. 늘 `NODE_ID`와 함께 씁니다.
5. 이름은 키가 아닙니다. 사람이 읽는 키로 대상을 찾은 뒤, 조인은 기계 키로 합니다.
6. 좌/직/우 코드: 01 좌회전, 02 직진, 03 우회전, 04 유턴, 05 직우. 마스터에는 01~04만 있습니다. 측정 테이블에서는 `DRCT_CD IN ('01','02','03','04','05')`로 거릅니다.
7. 카메라는 `M_CMR_INF.NODE_ID`와 `ACSR_ID`로 교차로와 방향에 잇습니다. 교통량은 이름에 `-교-`가 있는 카메라만 셉니다(`v_camera.name_has_gyo`).
8. **`M_CRSRD_SGNL_CMR_MC`는 조인하지 않습니다**(사용 여부 미확인).
9. 차로 테이블의 `DRCT_CD`(01~10)는 좌/직/우 코드가 아닙니다. 코드표가 미확인이라 `lane_drct_cd_raw`로만 둡니다.
10. 접근로 중복 키 BC002N0275 / ACSR000002는 두 행의 진입 위치가 다릅니다. `v_approach_key`에서 방향 표시를 비워 두었습니다. 그 방향에 걸린 질문에는 '미확인'이라고 답합니다.
11. 신호 제어기 키: `SGNL_CTRL_ID = 'SGNLC' + SGNL_CRSRD_NO(5자리)`.
12. store의 신호 조건은 적재 시점(2026-11-26 08:53 KST) 값입니다. 최신값과 시간별 이력은 회사 DB(`M_CRSRD_ACSR_SGNL_CNDT`, `L_KHCM_CRSRD_ACSR_SGNL_CNDT_15MI/1HH`)에 있습니다.

## 3. 미확인 (추정해서 답하지 않음)

| 항목 | 알려진 것 | 상태 |
|---|---|---|
| LOS 단위·산식 | 측정 테이블 LOS는 0~999.9 연속 숫자. 주석은 'LOS'·'서비스 수준'뿐 | 미확인 |
| RING_DRCT_CD(신호 이력 링 방향 코드 0~18) 뜻 | 코드표 없음 | 미확인 |
| 카메라 '4K', '과', 가상 카메라 역할 | CMR_CLS 모두 비어 있음 | 미확인 |
| 접근로 중복 키 BC002N0275 / ACSR000002 | 두 행 진입 위치가 다름 | 미확인 |
| M_CRSRD_SGNL_CMR_MC 사용 여부 | 조인 금지 | 미확인 |
| 차로 DRCT_CD 01~10 코드표 | 회전 코드와 범위가 다름 | 미확인 |

`onto_v2.v_unconfirmed`에도 같은 목록이 있습니다.

## 4. 뷰 컬럼 요약

- `v_approach`: node_id, acsr_id, approach_key, crsrd_nm, position_bearing(진입 위치), heading_bearing(진행 방향), direction_label(`북(남향)`), human_key, heading_matches_name, acsr_nm, acsr_drct, acsr_drct_nm, lane_qnty, key_rows, key_status
- `v_approach_key`: 키당 1행. 중복 키는 표시값이 NULL이고 key_status가 '미확인: 접근로 중복 키'
- `v_movement`: movement_key, direction_label, movement_nm, movement_short(좌/직/우/유턴/직우), human_key, key_status
- `v_camera`: cmr_id, cmr_nm, node_id, crsrd_nm, acsr_id, approach_key, direction_label, human_key, cmr_no, use_yn, ovr_stt_yn, vrtl_cmr_yn, name_has_gyo, name_token_before_last, camera_role_status('미확인'), link_status
- `v_approach_signal`: cycl, grn_tm, ylw_tm, pdst_grn_tm, itrs_offs, lftrn_form(_nm), lftrn_grn_tm, lftrn_ylw_tm, stlf_grn_tm, stlf_ylw_tm, key_status
- `v_intersection`: node_id, crsrd_nm, sgnl_crsrd_no, sgnl_ctrl_id, approach_count, camera_count
- `v_signal_controller`: sgnl_ctrl_id, node_id, crsrd_nm, id_rule_ok
- `v_code`, `v_bearing`, `v_lane`, `v_unconfirmed`

## 5. 질문 예시와 SQL 틀

- "<교차로명>의 방향 목록": `SELECT direction_label, approach_key FROM onto_v2.v_approach_key WHERE crsrd_nm = '<교차로명>' ORDER BY acsr_id;`
- "<교차로명> 북(남향) 좌회전의 키": `SELECT movement_key FROM onto_v2.v_movement WHERE crsrd_nm = '<교차로명>' AND direction_label = '북(남향)' AND drct_cd = '01';`
- "<교차로명> 북(남향)을 보는 교통량 카메라": `SELECT cmr_id, cmr_nm FROM onto_v2.v_camera WHERE crsrd_nm = '<교차로명>' AND direction_label = '북(남향)' AND name_has_gyo;`
- "<교차로명> 방향별 주기·녹색 시간": `SELECT direction_label, cycl, grn_tm, lftrn_grn_tm FROM onto_v2.v_approach_signal WHERE crsrd_nm = '<교차로명>';`
- 교통량은 측정 테이블(`S_CRSRD_DRCT_TRF_*` 등)을 `NODE_ID, ACSR_ID, DRCT_CD`로 `v_movement`에 잇습니다.

## 6. 개수 (2026-11-26 확인)

교차로 286, 방향 1,032행(키 1,031), 좌/직/우 4,122, 차로 2,709, 카메라 1,344, 신호 제어기 286, 방향 신호 조건 1,033, 코드 136(그룹 22).
