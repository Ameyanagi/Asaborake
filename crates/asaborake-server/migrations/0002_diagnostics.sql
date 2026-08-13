-- What the source recording contained and what was wrong with it: the stream
-- inventory, the drop and scramble counters, and the warnings derived from
-- them. Kept as JSON beside the analysis because it is read whole, by the job
-- detail view, and never queried across jobs.
--
-- NULL on every job recorded before this column existed, and on any job whose
-- source was not a transport stream.
ALTER TABLE jobs ADD COLUMN diagnostics TEXT;
