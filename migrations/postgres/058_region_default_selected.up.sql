-- Which regions a new monitor starts checked in. Every region defaulted in
-- until an operator opts one out, so an existing deployment keeps its coverage.
ALTER TABLE regions ADD COLUMN default_selected BOOLEAN NOT NULL DEFAULT true;
