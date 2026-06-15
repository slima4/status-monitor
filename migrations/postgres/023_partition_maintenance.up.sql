-- Month-partition maintenance for the time-retention tables, driven by the app
-- (boot + daily retention tick). The DEFAULT partition on each parent is the
-- backstop for any row outside the provisioned window.
CREATE OR REPLACE FUNCTION ensure_month_partitions(
    parent regclass, months_back int, months_ahead int
) RETURNS void AS $$
DECLARE
    nsp   text;
    base  text;
    lo    date;
    hi    date;
    child text;
    i     int;
BEGIN
    -- Partition DDL takes ACCESS EXCLUSIVE on the parent; serialize all
    -- maintenance (boot + daily tick across app instances) so concurrent runs
    -- queue instead of deadlocking on lock ordering.
    PERFORM pg_advisory_xact_lock(hashtextextended('partition_maintenance', 0));
    SELECT n.nspname, c.relname INTO nsp, base
    FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
    WHERE c.oid = parent;
    FOR i IN -months_back .. months_ahead LOOP
        lo    := (date_trunc('month', now()) + make_interval(months => i))::date;
        hi    := (lo + interval '1 month')::date;
        child := base || '_p' || to_char(lo, 'YYYYMM');
        BEGIN
            -- Schema-qualify: the pinned search_path would otherwise create in pg_catalog.
            EXECUTE format(
                'CREATE TABLE IF NOT EXISTS %I.%I PARTITION OF %s FOR VALUES FROM (%L) TO (%L)',
                nsp, child, parent::text, lo, hi
            );
        EXCEPTION WHEN check_violation THEN
            -- Can't carve a month whose rows already sit in DEFAULT; leave them
            -- there, skip. Any other error propagates.
            RAISE WARNING 'ensure_month_partitions: skipped % (%): %', child, SQLSTATE, SQLERRM;
        END;
    END LOOP;
END;
$$ LANGUAGE plpgsql SET search_path = pg_catalog, public;

-- Drop concrete partitions whose upper bound is at or before older_than. Leaves
-- DEFAULT; the caller's boundary DELETE trims the partition straddling the cutoff.
CREATE OR REPLACE FUNCTION drop_old_month_partitions(
    parent regclass, older_than timestamptz
) RETURNS int AS $$
DECLARE
    child   regclass;
    bound   text;
    upper_b timestamptz;
    dropped int := 0;
BEGIN
    PERFORM pg_advisory_xact_lock(hashtextextended('partition_maintenance', 0));
    FOR child, bound IN
        SELECT c.oid::regclass, pg_get_expr(c.relpartbound, c.oid)
        FROM pg_inherits inh
        JOIN pg_class c ON c.oid = inh.inhrelid
        WHERE inh.inhparent = parent
    LOOP
        IF bound = 'DEFAULT' THEN
            CONTINUE;
        END IF;
        upper_b := (regexp_match(bound, $re$TO \('([^']+)'\)$re$))[1]::timestamptz;
        IF upper_b IS NOT NULL AND upper_b <= older_than THEN
            EXECUTE format('DROP TABLE %s', child::text);
            dropped := dropped + 1;
        END IF;
    END LOOP;
    RETURN dropped;
END;
$$ LANGUAGE plpgsql SET search_path = pg_catalog, public;
