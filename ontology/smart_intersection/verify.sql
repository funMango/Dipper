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
UNION ALL SELECT 'v_excluded_approach', count(*) FROM onto_v2.v_excluded_approach
UNION ALL SELECT 'v_camera_exception', count(*) FROM onto_v2.v_camera_exception;
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
-- 카메라 역할 표시 (기대값: '역할 구분 안 함' 1343 = v_camera 행 수)
SELECT camera_role_status, count(*) FROM onto_v2.v_camera GROUP BY ALL;
-- 카메라 없음 기록 (기대값: 2행, 모두 camera_count=0, camera_note 있음. camera_note가 있는 행은 전체 2행, 마스터 카메라 0)
SELECT node_id, acsr_id, crsrd_nm, direction_label, camera_count, camera_note,
       (SELECT count(*) FROM onto_v2.m_cmr_inf m WHERE m.node_id = a.node_id AND m.acsr_id = a.acsr_id) AS master_cameras,
       (SELECT count(*) FROM onto_v2.v_approach_camera WHERE camera_note IS NOT NULL) AS note_rows
FROM onto_v2.v_approach_camera a WHERE camera_note IS NOT NULL ORDER BY node_id, acsr_id;
-- 카메라 예외 확인 (기대값: v_camera 1343, 마스터 m_cmr_inf 1344, v_camera에 BC002P1061 0행, BC001P0198는 v_camera에서 ACSR000003에 연결(approach_key 있음), 여월 ACSR000003 카메라 1대,
--   마스터에 BC002P1061·BC001P0198 행 그대로, 도약삼거리 v_intersection.camera_count 4, 예외 1행)
SELECT (SELECT count(*) FROM onto_v2.v_camera) AS v_camera_rows,
       (SELECT count(*) FROM onto_v2.m_cmr_inf) AS master_cameras,
       (SELECT count(*) FROM onto_v2.v_camera WHERE cmr_id = 'BC002P1061') AS excluded_in_v_camera,
       (SELECT count(*) FROM onto_v2.v_camera WHERE cmr_id = 'BC001P0198' AND node_id = 'BC001N0056' AND acsr_id = 'ACSR000003' AND approach_key IS NOT NULL) AS yeowol_camera_linked_acsr3,
       (SELECT count(*) FROM onto_v2.v_camera WHERE node_id = 'BC001N0056' AND acsr_id = 'ACSR000003') AS yeowol_acsr3_cameras,
       (SELECT count(*) FROM onto_v2.m_cmr_inf WHERE cmr_id = 'BC002P1061') AS master_excluded_row,
       (SELECT count(*) FROM onto_v2.m_cmr_inf WHERE cmr_id = 'BC001P0198' AND acsr_id = 'ACSR000003') AS master_yeowol_row,
       (SELECT camera_count FROM onto_v2.v_intersection WHERE node_id = 'BC002N0207') AS dohyak_camera_count,
       (SELECT count(*) FROM onto_v2.v_camera_exception) AS exception_rows;
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
-- 없는 방향이 있는 교차로 2곳 (기대값: 각 접근로 3개, 없는 방향(도약삼거리 북, 여월 서북=북서) 0개, intersection_note 있음, 노트 있는 교차로 전체 2곳)
SELECT i.node_id, i.crsrd_nm, i.approach_count,
       (SELECT count(*) FROM onto_v2.v_approach_key k WHERE k.node_id = i.node_id AND k.position_bearing = CASE i.node_id WHEN 'BC002N0207' THEN '북' ELSE '북서' END) AS missing_direction_rows,
       (SELECT string_agg(k.position_bearing, ',' ORDER BY k.acsr_id) FROM onto_v2.v_approach_key k WHERE k.node_id = i.node_id) AS positions,
       i.intersection_note,
       (SELECT count(*) FROM onto_v2.v_intersection WHERE intersection_note IS NOT NULL) AS note_rows
FROM onto_v2.v_intersection i WHERE i.intersection_note IS NOT NULL ORDER BY i.node_id;
