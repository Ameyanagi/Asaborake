-- How big the result was, so the history can report what a job achieved
-- rather than only that it finished. NULL on anything recorded before this
-- column existed, and on anything that did not produce a file.
ALTER TABLE jobs ADD COLUMN output_bytes INTEGER;
