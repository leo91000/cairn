-- Frozen from 1aa24dc825f909874852ffd24213080f9443b8db.
UPDATE shared_objects SET unused_at=?2
WHERE id IN (SELECT object FROM shared_references WHERE publication=?1)
AND NOT EXISTS (SELECT 1 FROM shared_references r
                WHERE r.object=shared_objects.id AND r.publication<>?1)
