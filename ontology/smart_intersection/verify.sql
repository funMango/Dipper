-- 온톨로지 v2 store 검증 쿼리 (개수만 본다)
SELECT table_name, source_count, loaded_rows, expected FROM onto_v2._load_log ORDER BY table_name;
SELECT 'v_approach' v, count(*) FROM onto_v2.v_approach
UNION ALL SELECT 'v_approach_key', count(*) FROM onto_v2.v_approach_key
UNION ALL SELECT 'v_movement', count(*) FROM onto_v2.v_movement
UNION ALL SELECT 'v_lane', count(*) FROM onto_v2.v_lane
UNION ALL SELECT 'v_camera', count(*) FROM onto_v2.v_camera
UNION ALL SELECT 'v_intersection', count(*) FROM onto_v2.v_intersection
UNION ALL SELECT 'v_signal_controller', count(*) FROM onto_v2.v_signal_controller
UNION ALL SELECT 'v_approach_signal', count(*) FROM onto_v2.v_approach_signal
UNION ALL SELECT 'v_code', count(*) FROM onto_v2.v_code
UNION ALL SELECT 'v_bearing', count(*) FROM onto_v2.v_bearing
UNION ALL SELECT 'v_approach_camera', count(*) FROM onto_v2.v_approach_camera
UNION ALL SELECT 'v_unconfirmed', count(*) FROM onto_v2.v_unconfirmed
UNION ALL SELECT 'v_excluded_approach', count(*) FROM onto_v2.v_excluded_approach;
-- 2026-11-26 결정 반영 확인 (기대값: 차로 마스터 2709 / v_lane 2347 / 코드 밖 0 / 이름 없음 0 / 무시 362)
SELECT (SELECT count(*) FROM onto_v2.m_crsrd_lane_inf) AS lane_master,
       (SELECT count(*) FROM onto_v2.v_lane) AS v_lane,
       (SELECT count(*) FROM onto_v2.v_lane WHERE lane_drct_cd NOT IN ('01','02','03','04','05')) AS v_lane_out_of_code,
       (SELECT count(*) FROM onto_v2.v_lane WHERE lane_drct_nm IS NULL) AS v_lane_no_name,
       (SELECT count(*) FROM onto_v2.m_crsrd_lane_inf WHERE drct_cd NOT IN ('01','02','03','04','05')) AS lane_ignored_06_10;
-- 접근로별 카메라 (기대값: 1030행 / 카메라 합 1338 / 카메라 있는 접근로 1028 / ACSR_ID 없는 카메라 6)
SELECT count(*) AS approaches, sum(camera_count) AS cameras, count(*) FILTER (WHERE camera_count > 0) AS approaches_with_camera,
       (SELECT count(*) FROM onto_v2.m_cmr_inf WHERE acsr_id IS NULL) AS cameras_without_acsr
FROM onto_v2.v_approach_camera;
-- 카메라 역할 표시 (기대값: '역할 구분 안 함' 1344)
SELECT camera_role_status, count(*) FROM onto_v2.v_camera GROUP BY ALL;
-- 카메라 없음 기록 (기대값: 2행, 모두 camera_count=0, camera_note 있음. camera_note가 있는 행은 전체 2행, 마스터 카메라 0)
SELECT node_id, acsr_id, crsrd_nm, direction_label, camera_count, camera_note,
       (SELECT count(*) FROM onto_v2.m_cmr_inf m WHERE m.node_id = a.node_id AND m.acsr_id = a.acsr_id) AS master_cameras,
       (SELECT count(*) FROM onto_v2.v_approach_camera WHERE camera_note IS NOT NULL) AS note_rows
FROM onto_v2.v_approach_camera a WHERE camera_note IS NOT NULL ORDER BY node_id, acsr_id;
-- 미확인 목록 (기대값: 0행)
SELECT count(*) AS unconfirmed FROM onto_v2.v_unconfirmed;
-- 없는 접근로 제외 확인 (기대값: 마스터 접근로 2 / 회전 4 / 신호 1 / 유효 뷰에 남은 행 모두 0 / BC002N0275 유효 접근로 2)
SELECT (SELECT count(*) FROM onto_v2.m_crsrd_acsr_inf x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id)) AS master_approach,
       (SELECT count(*) FROM onto_v2.m_crsrd_drct_inf x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id)) AS master_movement,
       (SELECT count(*) FROM onto_v2.m_crsrd_acsr_sgnl_cndt x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id)) AS master_signal,
       (SELECT count(*) FROM onto_v2.v_approach x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id))
     + (SELECT count(*) FROM onto_v2.v_approach_key x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id))
     + (SELECT count(*) FROM onto_v2.v_movement x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id))
     + (SELECT count(*) FROM onto_v2.v_lane x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id))
     + (SELECT count(*) FROM onto_v2.v_approach_signal x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id))
     + (SELECT count(*) FROM onto_v2.v_approach_camera x JOIN onto_v2.v_excluded_approach e USING (node_id, acsr_id)) AS left_in_valid_views,
       (SELECT approach_count FROM onto_v2.v_intersection WHERE node_id = 'BC002N0275') AS bc002n0275_approaches;
