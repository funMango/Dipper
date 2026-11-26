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
UNION ALL SELECT 'v_code', count(*) FROM onto_v2.v_code;
