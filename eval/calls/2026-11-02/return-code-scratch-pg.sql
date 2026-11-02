CREATE PROCEDURE st(p_a integer, INOUT return_code integer DEFAULT NULL) LANGUAGE plpgsql AS $$ BEGIN return_code := 1; END $$;
DO $$ DECLARE rc_scratch integer; BEGIN CALL st(5, return_code => rc_scratch); CALL st(p_a => 5, return_code => rc_scratch); RAISE NOTICE 'ok'; END $$;
DO $$ BEGIN CALL st(5); END $$;
