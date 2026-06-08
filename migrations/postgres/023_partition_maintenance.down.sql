DROP FUNCTION IF EXISTS drop_old_month_partitions(regclass, timestamptz);
DROP FUNCTION IF EXISTS ensure_month_partitions(regclass, int, int);
