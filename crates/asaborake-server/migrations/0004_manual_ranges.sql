-- Cuts somebody chose by hand, as JSON, when a job was created by re-cutting
-- an earlier one. NULL for every ordinary job, which is decided by the
-- segmenter rather than by a person.
ALTER TABLE jobs ADD COLUMN manual_ranges TEXT;
