-- 스마트교차로 온톨로지 v2 DuckDB 조회 뷰 (store 스키마 onto_v2)
-- 원본 정의: ontology/smart_intersection/ontology.yaml
-- 적재 테이블(onto_v2.m_*)은 회사 DB CRSRD 마스터의 읽기 전용 사본이다. 이 파일은 뷰만 만든다.
-- 실행: DuckDB에서 한 번 실행. 이미 있는 이름은 덮어쓰지 않도록 CREATE VIEW(OR REPLACE 없음)를 쓴다.

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

-- 방향(접근로): 마스터 행 그대로(1,032행). 진입 위치는 ACSR_NM의 '-위치 (' 부분, 진행 방향은 위치의 반대.
CREATE VIEW onto_v2.v_approach AS
WITH a AS (
  SELECT x.*,
         regexp_extract(x.acsr_nm, '-\s*(북동|북서|남동|남서|동북|동남|서북|서남|북|남|동|서)\s*\(', 1) AS pos_raw,
         regexp_extract(x.acsr_nm, '\(\s*(북동|북서|남동|남서|동북|동남|서북|서남|북|남|동|서)향\s*\)', 1) AS hdg_raw,
         count(*) OVER (PARTITION BY x.node_id, x.acsr_id) AS key_rows
  FROM onto_v2.m_crsrd_acsr_inf x
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

-- 교차로
CREATE VIEW onto_v2.v_intersection AS
SELECT i.node_id, i.crsrd_nm, i.sgnl_crsrd_nm, i.sgnl_crsrd_no, i.stnd_node_id,
       i.crsrd_cls, i.lng_crdn, i.lat_crdn, i.use_yn,
       s.sgnl_ctrl_id,
       (SELECT count(*) FROM onto_v2.v_approach_key k WHERE k.node_id = i.node_id) AS approach_count,
       (SELECT count(*) FROM onto_v2.m_cmr_inf m WHERE m.node_id = i.node_id) AS camera_count
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
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'DRCT_CD' AND c.cd = d.drct_cd;

-- 차로: 이 테이블의 DRCT_CD(01~10)는 회전 코드와 범위가 달라 코드표에 잇지 않는다(미확인).
CREATE VIEW onto_v2.v_lane AS
SELECT l.node_id, l.acsr_id, l.drct_cd AS lane_drct_cd_raw, l.lane_no, l.area_seqno,
       k.crsrd_nm, k.direction_label, l.max_trf_qnty,
       '미확인: 차로 DRCT_CD 코드표' AS lane_drct_cd_status,
       CASE WHEN k.node_id IS NULL THEN '접근로 마스터에 없음' ELSE k.key_status END AS key_status
FROM onto_v2.m_crsrd_lane_inf l
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = l.node_id AND k.acsr_id = l.acsr_id;

-- 카메라: M_CMR_INF.NODE_ID, ACSR_ID로 교차로와 방향에 연결. M_CRSRD_SGNL_CMR_MC는 쓰지 않는다.
CREATE VIEW onto_v2.v_camera AS
SELECT m.cmr_id, m.cmr_nm, m.node_id, i.crsrd_nm, m.acsr_id,
       k.approach_key, k.direction_label,
       i.crsrd_nm || ' ' || k.direction_label || ' 카메라 ' || m.cmr_id AS human_key,
       m.cmr_no, m.use_yn, m.ovr_stt_yn, m.vrtl_cmr_yn, m.gis_dspl_yn, m.srvr_id,
       (m.cmr_nm LIKE '%-교-%') AS name_has_gyo,
       trim(list_extract(string_split(m.cmr_nm, '-'), -2)) AS name_token_before_last,
       '미확인' AS camera_role_status,
       m.lng_crdn, m.lat_crdn, m.reg_dt, m.mdfcn_dt,
       CASE WHEN m.acsr_id IS NULL THEN 'ACSR_ID 없음'
            WHEN k.node_id IS NULL THEN '접근로 마스터에 없음'
            ELSE k.key_status END AS link_status
FROM onto_v2.m_cmr_inf m
LEFT JOIN onto_v2.m_crsrd_inf i ON i.node_id = m.node_id
LEFT JOIN onto_v2.v_approach_key k ON k.node_id = m.node_id AND k.acsr_id = m.acsr_id;

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
LEFT JOIN onto_v2.m_cd_inf c ON c.grp_cd = 'LFTRN_FORM' AND c.cd = s.lftrn_form;

-- 아직 답이 없는 항목(AI가 추정하지 않도록)
CREATE VIEW onto_v2.v_unconfirmed AS
SELECT * FROM (VALUES
  ('LOS', 'LOS 숫자의 단위와 산식', '측정 테이블 LOS는 0~999.9 연속 숫자. 단위 미확인'),
  ('RING_DRCT_CD', '신호 이력 링 방향 코드(이동류) 0~18의 뜻', '코드표 없음'),
  ('CAMERA_ROLE', '카메라 이름의 4K, 과, 가상 카메라(VRTL_CMR_YN=Y)의 역할', 'CMR_CLS 모두 비어 있음'),
  ('ACSR_DUP_KEY', '접근로 중복 키 BC002N0275 / ACSR000002', 'v_approach_key에서 표시값을 비움'),
  ('SGNL_CMR_MC', 'M_CRSRD_SGNL_CMR_MC 사용 여부', '조인 금지. store에 적재하지 않음'),
  ('LANE_DRCT_CD', '차로 테이블 DRCT_CD 01~10의 코드표', '회전 DRCT_CD와 범위가 다름')
) AS t(item, question, note);
