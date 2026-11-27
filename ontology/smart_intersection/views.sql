-- 스마트교차로 온톨로지 v2 DuckDB 조회 뷰 (store 스키마 onto_v2)
-- 원본 정의: ontology/smart_intersection/ontology.yaml
-- 적재 테이블(onto_v2.m_*)은 회사 DB CRSRD 마스터의 읽기 전용 사본이다. 이 파일은 뷰만 만든다.
-- 실행: DuckDB에서 한 번 실행. 이미 있는 이름은 덮어쓰지 않도록 CREATE VIEW(OR REPLACE 없음)를 쓴다.
-- 이미 만든 store에서 뷰를 바꿀 때: OR REPLACE를 쓰지 않고, 바뀐 뷰만 한 트랜잭션 안에서 DROP VIEW 후 이 파일의 CREATE VIEW를 의존 순서대로 실행한다.
-- 2026-11-26 미확인 결정 반영: v_lane(코드표 이름, 06~10 제외), v_camera(역할 구분 안 함), v_approach_camera(새 뷰), v_unconfirmed(중복 키만).
-- 2026-11-26 15:47 결정: BC002N0275/ACSR000002는 없는 접근로. v_excluded_approach(새 뷰)에 적고 v_approach·v_movement·v_lane·v_approach_signal에서 뺀다. v_unconfirmed는 비움.

-- 방위 값 목록: 8방위 정규형, 반대 방위, ACSR_DRCT 코드
CREATE VIEW onto_v2.v_bearing AS
SELECT * FROM (VALUES
  ('북','남','01'), ('북동','남서','02'), ('동','서','03'), ('남동','북서','04'),
  ('남','북','05'), ('남서','북동','06'), ('서','동','07'), ('북서','남동','08')
) AS t(bearing, opposite, acsr_drct_cd);

-- 코드표 (그룹명 포함)
CREATE VIEW onto_v2.v_code AS
SELECT c.grp_cd, g.grp_cd_nm, c.cd, c.cd_nm, c.use_yn, c.sort_no,
       (g.grp_cd IS NOT NULL) AS grp_in_master
FROM onto_v2.m_cd_inf c
LEFT JOIN onto_v2.m_cd_grp_inf g ON g.grp_cd = c.grp_cd;

-- 실제로는 없는 접근로(데이터 품질 예외). 마스터 행은 지우지 않고 유효 뷰에서 뺀다. 회전·신호 행은 다른 접근로로 옮기지 않는다.
-- 근거: 민호님 2026-11-26 15:47 "남부천신협앞은 남(북향), 북(남향)밖에 없어 이거 두개만 인정하고 서부분은 없다고 따로 기록해"
CREATE VIEW onto_v2.v_excluded_approach AS
SELECT * FROM (VALUES
  ('BC002N0275', 'ACSR000002', '남부천신협앞',
   '실제로는 없는 접근로. 유효 접근로는 ACSR000001 북(남향), ACSR000003 남(북향)뿐',
   '접근로 2행(동(서향), 서(동향)), 회전 4행(01~04), 신호 조건 1행을 모두 유효 뷰에서 제외, 다른 접근로로 옮기지 않음(확정: 민호님 2026-11-26 15:56 "승인"). 카메라 0, 차로 0',
   '민호님 2026-11-26 15:47 결정, 15:56 "승인"')
) AS t(node_id, acsr_id, crsrd_nm, reason, excluded_rows, source);

-- 카메라 예외(데이터 품질): 마스터 m_cmr_inf 행은 지우지 않고 카메라 뷰에서만 뺀다. 다시 적재해도 이 목록으로 같은 결과가 나온다.
--   v_camera·v_approach_camera·v_intersection.camera_count에서 뺀다. 접근로 연결은 바꾸지 않는다.
-- 근거: 민호님 2026-11-27 17:40 "도약삼거리 '교-북' 카메라 정보 필요없으니 지우고"
CREATE VIEW onto_v2.v_camera_exception AS
SELECT * FROM (VALUES
  ('BC002P1061', '308.도약3R-④-교-북(상행)', 'BC002N0207', 'exclude',
   '도약삼거리에는 북 접근로가 없다. 이 카메라 정보는 필요 없음(지움). 마스터 행은 유지', '민호님 2026-11-27 17:40')
) AS t(cmr_id, cmr_nm, node_id, action, reason, source);

-- 방향(접근로): 마스터 행에서 v_excluded_approach를 뺀 유효 행(1,030행). 진입 위치는 ACSR_NM의 '-위치 (' 부분, 진행 방향은 위치의 반대.
CREATE VIEW onto_v2.v_approach AS
WITH a AS (
  SELECT x.*,
         regexp_extract(x.acsr_nm, '-\s*(북동|북서|남동|남서|동북|동남|서북|서남|북|남|동|서)\s*\(', 1) AS pos_raw,
         regexp_extract(x.acsr_nm, '\(\s*(북동|북서|남동|남서|동북|동남|서북|서남|북|남|동|서)향\s*\)', 1) AS hdg_raw,
         count(*) OVER (PARTITION BY x.node_id, x.acsr_id) AS key_rows
  FROM onto_v2.m_crsrd_acsr_inf x
  WHERE NOT EXISTS (SELECT 1 FROM onto_v2.v_excluded_approach e WHERE e.node_id = x.node_id AND e.acsr_id = x.acsr_id)
), n AS (
  SELECT a.*,
         CASE pos_raw WHEN '동북' THEN '북동' WHEN '동남' THEN '남동' WHEN '서남' THEN '남서' WHEN '서북' THEN '북서' ELSE NULLIF(pos_raw,'') END AS position_bearing,
         CASE hdg_raw WHEN '동북' THEN '북동' WHEN '동남' THEN '남동' WHEN '서남' THEN '남서' WHEN '서북' THEN '북서' ELSE NULLIF(hdg_raw,'') END AS heading_in_name
  FROM a
)
SELECT n.node_id, n.acsr_id, n.node_id || ':' || n.acsr_id AS approach_key,
       i.crsrd_nm,
       n.position_bearing,
       b.opposite AS heading_bearing,
       n.position_bearing || '(' || b.opposite || '향)' AS direction_label,
       i.crsrd_nm || ' ' || n.position_bearing || '(' || b.opposite || '향)' AS human_key,
       (n.heading_in_name = b.opposite) AS heading_matches_name,
       n.acsr_nm, n.sgnl_acsr_nm, n.acsr_drct, c.cd_nm AS acsr_drct_nm,
       n.lane_qnty, n.acsr_angl, n.lnk_id, n.lng_crdn, n.lat_crdn,
       n.key_rows,
       CASE WHEN n.key_rows > 1 THEN '미확인: 접근로 중복 키' ELSE '정상' END AS key_status
FROM n
JOIN onto_v2.m_crsrd_inf i ON i.node_id = n.node_id
LEFT JOIN onto_v2.v_bearing b ON b.bearing = n.position_bearing
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'ACSR_DRCT' AND c.cd = n.acsr_drct;

-- 방향(접근로) 키당 1행(조인용). 중복 키는 표시값을 비우고 '미확인'으로 둔다(임의로 한 행을 고르지 않음).
CREATE VIEW onto_v2.v_approach_key AS
SELECT node_id, acsr_id, approach_key, crsrd_nm,
       CASE WHEN count(*) = 1 THEN any_value(position_bearing) END AS position_bearing,
       CASE WHEN count(*) = 1 THEN any_value(heading_bearing) END AS heading_bearing,
       CASE WHEN count(*) = 1 THEN any_value(direction_label) END AS direction_label,
       CASE WHEN count(*) = 1 THEN any_value(human_key) END AS human_key,
       count(*) AS key_rows,
       CASE WHEN count(*) > 1 THEN '미확인: 접근로 중복 키' ELSE '정상' END AS key_status
FROM onto_v2.v_approach
GROUP BY node_id, acsr_id, approach_key, crsrd_nm;

-- 신호 제어기
CREATE VIEW onto_v2.v_signal_controller AS
SELECT s.sgnl_ctrl_id, s.node_id, i.crsrd_nm, i.sgnl_crsrd_no, s.use_yn,
       (s.sgnl_ctrl_id = 'SGNLC' || lpad(CAST(i.sgnl_crsrd_no AS VARCHAR), 5, '0')) AS id_rule_ok
FROM onto_v2.m_crsrd_sgnl_inf s
LEFT JOIN onto_v2.m_crsrd_inf i ON i.node_id = s.node_id;

-- 교차로. intersection_note: 실제로 없는 방향이 있는 교차로만 표시(나머지 NULL). 근거: 민호님 2026-11-27 17:15.
--   BC002N0207(도약삼거리)는 북 접근로 없음, BC001N0056(여월휴먼시아1단지입구삼거리)는 서북(서) 접근로 없음. 접근로는 3개가 정상이다.
CREATE VIEW onto_v2.v_intersection AS
SELECT i.node_id, i.crsrd_nm, i.sgnl_crsrd_nm, i.sgnl_crsrd_no, i.stnd_node_id,
       i.crsrd_cls, i.lng_crdn, i.lat_crdn, i.use_yn,
       s.sgnl_ctrl_id,
       (SELECT count(*) FROM onto_v2.v_approach_key k WHERE k.node_id = i.node_id) AS approach_count,
       (SELECT count(*) FROM onto_v2.m_cmr_inf m WHERE m.node_id = i.node_id
          AND NOT EXISTS (SELECT 1 FROM onto_v2.v_camera_exception ce WHERE ce.cmr_id = m.cmr_id)) AS camera_count,
       CASE i.node_id WHEN 'BC002N0207' THEN '북 접근로 없음(실제로 없음, 민호님 확인 2026-11-27)' WHEN 'BC001N0056' THEN '서북 접근로 없음(실제로 없음, 민호님 확인 2026-11-27)' END AS intersection_note
FROM onto_v2.m_crsrd_inf i
LEFT JOIN onto_v2.m_crsrd_sgnl_inf s ON s.node_id = i.node_id;

-- 좌/직/우(회전): DB 이름은 '교차로 방향'(DRCT_CD)
CREATE VIEW onto_v2.v_movement AS
SELECT d.node_id, d.acsr_id, d.drct_cd,
       d.node_id || ':' || d.acsr_id || ':' || d.drct_cd AS movement_key,
       k.crsrd_nm, k.direction_label,
       c.cd_nm AS movement_nm,
       CASE d.drct_cd WHEN '01' THEN '좌' WHEN '02' THEN '직' WHEN '03' THEN '우' WHEN '04' THEN '유턴' WHEN '05' THEN '직우' END AS movement_short,
       k.human_key || ' ' || CASE d.drct_cd WHEN '01' THEN '좌' WHEN '02' THEN '직' WHEN '03' THEN '우' WHEN '04' THEN '유턴' WHEN '05' THEN '직우' END AS human_key,
       d.max_trf_qnty,
       CASE WHEN k.node_id IS NULL THEN '접근로 마스터에 없음' ELSE k.key_status END AS key_status
FROM onto_v2.m_crsrd_drct_inf d
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = d.node_id AND k.acsr_id = d.acsr_id
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'DRCT_CD' AND c.cd = d.drct_cd
WHERE NOT EXISTS (SELECT 1 FROM onto_v2.v_excluded_approach e WHERE e.node_id = d.node_id AND e.acsr_id = d.acsr_id);

-- 차로: DRCT_CD는 M_CD_INF GRP_CD='DRCT_CD' 코드표(01 좌회전, 02 직진, 03 우회전, 04 유턴, 05 직우)로 이름을 붙인다.
-- 01~05만 유효. 06~10은 무시한다(민호님 결정 2026-11-26). 이 뷰에서 빼고, 마스터 행은 지우지 않는다.
CREATE VIEW onto_v2.v_lane AS
SELECT l.node_id, l.acsr_id, l.drct_cd AS lane_drct_cd, c.cd_nm AS lane_drct_nm,
       CASE l.drct_cd WHEN '01' THEN '좌' WHEN '02' THEN '직' WHEN '03' THEN '우' WHEN '04' THEN '유턴' WHEN '05' THEN '직우' END AS lane_drct_short,
       l.lane_no, l.area_seqno,
       k.crsrd_nm, k.direction_label, l.max_trf_qnty,
       CASE WHEN k.node_id IS NULL THEN '접근로 마스터에 없음' ELSE k.key_status END AS key_status
FROM onto_v2.m_crsrd_lane_inf l
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = l.node_id AND k.acsr_id = l.acsr_id
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'DRCT_CD' AND c.cd = l.drct_cd
WHERE l.drct_cd IN ('01', '02', '03', '04', '05')
  AND NOT EXISTS (SELECT 1 FROM onto_v2.v_excluded_approach e WHERE e.node_id = l.node_id AND e.acsr_id = l.acsr_id);

-- 카메라: M_CMR_INF.NODE_ID, ACSR_ID로 교차로와 방향에 연결. M_CRSRD_SGNL_CMR_MC는 쓰지 않는다(사용 안 함, 민호님 결정 2026-11-26).
-- 4K·교·가상 카메라의 역할은 정하지 않는다(역할 구분 안 함). 접근로별 카메라 목록은 v_approach_camera.
-- v_camera_exception에 있는 카메라(도약 BC002P1061)는 이 뷰에서 뺀다(민호님 2026-11-27 17:40).
-- 여월 BC001P0198(이름 '교-서북')은 서(동향) ACSR000003 카메라가 맞다. 연결을 유지한다(민호님 2026-11-27 17:42).
CREATE VIEW onto_v2.v_camera AS
SELECT m.cmr_id, m.cmr_nm, m.node_id, i.crsrd_nm, m.acsr_id,
       k.approach_key, k.direction_label,
       i.crsrd_nm || ' ' || k.direction_label || ' 카메라 ' || m.cmr_id AS human_key,
       m.cmr_no, m.use_yn, m.ovr_stt_yn, m.vrtl_cmr_yn, m.gis_dspl_yn, m.srvr_id,
       (m.cmr_nm LIKE '%-교-%') AS name_has_gyo,
       trim(list_extract(string_split(m.cmr_nm, '-'), -2)) AS name_token_before_last,
       '역할 구분 안 함' AS camera_role_status,
       m.lng_crdn, m.lat_crdn, m.reg_dt, m.mdfcn_dt,
       CASE WHEN m.acsr_id IS NULL THEN 'ACSR_ID 없음'
            WHEN k.node_id IS NULL THEN '접근로 마스터에 없음'
            ELSE k.key_status END AS link_status
FROM onto_v2.m_cmr_inf m
LEFT JOIN onto_v2.m_crsrd_inf i ON i.node_id = m.node_id
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = m.node_id AND k.acsr_id = m.acsr_id
WHERE NOT EXISTS (SELECT 1 FROM onto_v2.v_camera_exception ce WHERE ce.cmr_id = m.cmr_id);

-- 방향(접근로)별 카메라 전체 목록: 접근로 키당 1행. 역할을 정하지 않고 그 접근로에 걸린 카메라를 모두 적는다(민호님 결정 2026-11-26).
-- 표시 형식 예: '<교차로명> 동(서향) = 카메라 A, B'. 중복 키 접근로는 key_status가 '미확인: 접근로 중복 키'.
-- camera_note: 카메라 수가 0인 접근로 중 '실제로 카메라가 없다'고 민호님이 확인한 것만 표시한다(나머지 NULL). 0인데 NULL이면 누락 여부를 단정하지 않는다.
--   확인된 접근로: BC002N0274/ACSR000001(부천여중사거리 북(남향)), BC001N0071/ACSR000004(역곡남부역삼거리 북(남향)). 근거: 민호님 2026-11-26 16:52.
CREATE VIEW onto_v2.v_approach_camera AS
SELECT k.node_id, k.acsr_id, k.approach_key, k.crsrd_nm, k.direction_label, k.human_key,
       count(m.cmr_id) AS camera_count,
       count(m.cmr_id) FILTER (WHERE m.cmr_nm LIKE '%-교-%') AS gyo_camera_count,
       coalesce(list(m.cmr_id ORDER BY m.cmr_id) FILTER (WHERE m.cmr_id IS NOT NULL), []) AS camera_ids,
       string_agg(m.cmr_nm, ', ' ORDER BY m.cmr_id) AS camera_names,
       k.key_status,
       CASE WHEN (k.node_id, k.acsr_id) IN (('BC002N0274', 'ACSR000001'), ('BC001N0071', 'ACSR000004')) THEN '카메라 없음(민호님 확인 2026-11-26)' END AS camera_note
FROM onto_v2.v_approach_key k
LEFT JOIN onto_v2.m_cmr_inf m ON m.node_id = k.node_id AND m.acsr_id = k.acsr_id
                              AND NOT EXISTS (SELECT 1 FROM onto_v2.v_camera_exception ce WHERE ce.cmr_id = m.cmr_id)
GROUP BY k.node_id, k.acsr_id, k.approach_key, k.crsrd_nm, k.direction_label, k.human_key, k.key_status;

-- 방향(접근로)별 신호 조건(현재값). 시간 단위 이력은 회사 DB L_KHCM_CRSRD_ACSR_SGNL_CNDT_15MI/1HH.
CREATE VIEW onto_v2.v_approach_signal AS
SELECT s.node_id, s.acsr_id, s.node_id || ':' || s.acsr_id AS approach_key,
       k.crsrd_nm, k.direction_label, k.human_key,
       s.cycl, s.grn_tm, s.ylw_tm, s.pdst_grn_tm, s.itrs_offs,
       s.lftrn_form, c.cd_nm AS lftrn_form_nm,
       s.lftrn_grn_tm, s.lftrn_ylw_tm, s.stlf_grn_tm, s.stlf_ylw_tm, s.mdfcn_dt,
       CASE WHEN k.node_id IS NULL THEN '접근로 마스터에 없음' ELSE k.key_status END AS key_status
FROM onto_v2.m_crsrd_acsr_sgnl_cndt s
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = s.node_id AND k.acsr_id = s.acsr_id
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'LFTRN_FORM' AND c.cd = s.lftrn_form
WHERE NOT EXISTS (SELECT 1 FROM onto_v2.v_excluded_approach e WHERE e.node_id = s.node_id AND e.acsr_id = s.acsr_id);

-- 아직 답이 없는 항목(AI가 추정하지 않도록). 결정된 항목은 ontology.yaml의 decisions에 있다.
-- 2026-11-26 현재 미확인 항목 없음(빈 뷰). 새로 모르는 것이 생기면 VALUES로 행을 넣는다.
CREATE VIEW onto_v2.v_unconfirmed AS
SELECT CAST(NULL AS VARCHAR) AS item, CAST(NULL AS VARCHAR) AS question, CAST(NULL AS VARCHAR) AS note
WHERE false;
